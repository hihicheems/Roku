// Copyright 2025 itscheems
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use roku_agent_runtime::AgentWorker;
use roku_common_types::{
	ErrorClass, EvidenceItem, RecoveryEligibility, ResponseEnvelope, ResponseStatus, ResultStatus,
	RuntimeError, Task, TaskId, TaskNode, TaskNodeKind, TaskState,
};
use roku_execution_graph_builder::TaskGraphScheduler;
use roku_observability::{AuditCorrelation, AuditRecord};

use crate::helpers::{failure_message, result_message, success_message};
use crate::{RunMode, RuntimeService};

impl RuntimeService {
	pub(super) fn process_task(
		&self,
		task: &mut Task,
		mode: RunMode,
	) -> Result<ResponseEnvelope, RuntimeError> {
		let request_id = task.request_id.clone();
		let graph = task
			.graph
			.clone()
			.ok_or_else(|| RuntimeError::new("task graph is missing"))?;
		let scheduler = TaskGraphScheduler;

		while !scheduler
			.is_complete(&graph, &task.completed_nodes)
			.map_err(|error| RuntimeError::new(error.to_string()))?
		{
			let ready_nodes = scheduler
				.ready_nodes(&graph, &task.completed_nodes)
				.map_err(|error| RuntimeError::new(error.to_string()))?;
			if ready_nodes.is_empty() {
				return Err(RuntimeError::new(
					"task graph has no ready nodes but is not complete",
				));
			}

			for node in ready_nodes {
				match node.kind {
					TaskNodeKind::Execution => {
						if let Some(response) = self.process_execution_node(task, &node, mode)? {
							return Ok(response);
						}
					}
					TaskNodeKind::Approval => {
						return self.process_approval_node(task, &node);
					}
					TaskNodeKind::Validation => {
						let report = self.process_validation_node(task, &node, mode)?;
						if let Some(response) = report {
							return Ok(response);
						}
					}
					TaskNodeKind::Aggregation => {
						self.process_aggregation_node(task, &node)?;
					}
				}
			}
		}

		self.finalize_task(task, request_id)
	}

	pub fn resume_task(&self, task_id: &TaskId) -> Result<ResponseEnvelope, RuntimeError> {
		let mut task = self
			.get_task(task_id)?
			.ok_or_else(|| RuntimeError::new(format!("task not found: {}", task_id.0)))?;
		let analysis = self.analyze_task_recovery(&task)?;

		match task.state {
			TaskState::Succeeded | TaskState::Cancelled | TaskState::DeadLetter => {
				return Err(RuntimeError::new(format!(
					"task is not resumable from state {:?}",
					task.state
				)));
			}
			TaskState::WaitingApproval => {
				let approval_id = task.pending_approval_id.clone().ok_or_else(|| {
					RuntimeError::new("task is waiting for approval but no approval id is stored")
				})?;
				return Ok(ResponseEnvelope {
					request_id: task.request_id.clone(),
					status: ResponseStatus::PendingApproval,
					message: format!("approval required for {}", approval_id.0),
					artifacts: vec![format!("approval://{}", approval_id.0)],
				});
			}
			TaskState::Aggregating => {
				let request_id = task.request_id.clone();
				return self.finalize_task(&mut task, request_id);
			}
			_ => {}
		}

		let target_state = match analysis.recovery_eligibility {
			RecoveryEligibility::FinalizeReady if analysis.is_complete => TaskState::Validating,
			RecoveryEligibility::ResumeReady => classify_resume_state(&analysis.ready_nodes),
			RecoveryEligibility::RequiresManualResume => {
				return Err(RuntimeError::new(
					"task requires manual resume before automatic execution can continue",
				));
			}
			RecoveryEligibility::Blocked => {
				return Err(RuntimeError::new(
					"task graph has no replay-ready nodes and cannot be resumed",
				));
			}
			RecoveryEligibility::NotRecoverable => {
				return Err(RuntimeError::new(format!(
					"task is not resumable from state {:?}",
					task.state
				)));
			}
			RecoveryEligibility::PendingApproval => {
				return Err(RuntimeError::new(
					"task is waiting for approval and must be resumed through approval flow",
				));
			}
			RecoveryEligibility::FinalizeReady => TaskState::Validating,
		};
		self.normalize_task_for_resume(&mut task, target_state)?;
		self.process_task(&mut task, RunMode::Normal)
	}

	pub(super) fn fail_task(
		&self,
		task: &mut Task,
		reason: &str,
		error_class: roku_common_types::ErrorClass,
	) -> Result<TaskState, RuntimeError> {
		let disposition = self
			.orchestrator
			.register_failure(task, reason, Some(error_class))?;
		let mut state = self.lock_state()?;
		for event in disposition.events {
			state
				.event_repo
				.append_event(event)
				.map_err(|error| RuntimeError::new(error.to_string()))?;
		}
		if disposition.terminal_state == TaskState::DeadLetter {
			self.metrics.inc_dead_letters();
		}
		Ok(disposition.terminal_state)
	}

	pub(super) fn process_execution_node(
		&self,
		task: &mut Task,
		node: &TaskNode,
		mode: RunMode,
	) -> Result<Option<ResponseEnvelope>, RuntimeError> {
		let spec = self.factory.build_for_node_with_history(
			&task.task_id,
			node,
			&task.conversation_history,
		);
		let capability_allowed = {
			let mut state = self.lock_state()?;
			let token = state
				.capability_auth
				.issue(roku_capability_auth::CapabilityRequest {
					subject: spec.instance_id.clone(),
					resource: "tool.runtime".to_string(),
					actions: if matches!(mode, RunMode::CapabilityDenied) {
						vec!["read".to_string()]
					} else {
						vec!["invoke".to_string()]
					},
					expires_at_unix: 999_999,
				});
			let is_allowed = state.capability_auth.verify(&token, "invoke", 100);

			if !is_allowed {
				self.audit_sink
					.record(
						AuditRecord::new(
							spec.instance_id.clone(),
							"invoke",
							token.resource,
							"denied",
						)
						.with_correlation(AuditCorrelation {
							trace_id: format!("trace-{}", task.request_id.0),
							span_id: "capability-check".to_string(),
							task_id: Some(task.task_id.0.clone()),
							request_id: Some(task.request_id.0.clone()),
						})
						.with_attribute("node_id", node.node_id.0.clone()),
					)
					.map_err(|error| RuntimeError::new(error.to_string()))?;
			}

			is_allowed
		};

		if !capability_allowed {
			self.metrics.inc_failures();
			if matches!(mode, RunMode::RetryExhausted) {
				task.attempts = self.orchestrator.config.max_attempts.saturating_sub(1);
			}
			let terminal_state = self.fail_task(task, "capability denied", ErrorClass::Security)?;
			self.fail_experiment_run(task, "capability denied")?;
			self.save_task(task.clone())?;

			return Ok(Some(ResponseEnvelope {
				request_id: task.request_id.clone(),
				status: ResponseStatus::Failed,
				message: failure_message("capability denied", terminal_state),
				artifacts: Vec::new(),
			}));
		}

		if task.state != TaskState::Executing {
			self.record_transition(task, TaskState::Executing, "execute")?;
		}
		let mut result = self.runtime.execute(&spec, node);
		let artifact = self.persist_result_artifact(&result)?;
		if matches!(mode, RunMode::MissingEvidence | RunMode::RetryExhausted) {
			result.evidence.clear();
		} else {
			result.evidence.push(EvidenceItem {
				kind: "artifact_ref".to_string(),
				value: artifact.uri.clone(),
			});
		}

		self.save_result(result.clone())?;
		self.attach_artifact_to_experiment(&task.task_id, artifact.artifact_id.clone())?;
		self.metrics.inc_artifacts();

		if matches!(result.status, ResultStatus::Error) {
			self.metrics.inc_failures();
			if matches!(mode, RunMode::RetryExhausted) {
				task.attempts = self.orchestrator.config.max_attempts.saturating_sub(1);
			}
			let reason = result_message(&result);
			let terminal_state = self.fail_task(task, &reason, ErrorClass::Dependency)?;
			self.fail_experiment_run(task, &reason)?;
			self.save_task(task.clone())?;

			return Ok(Some(ResponseEnvelope {
				request_id: task.request_id.clone(),
				status: ResponseStatus::Failed,
				message: failure_message(&reason, terminal_state),
				artifacts: vec![artifact.uri],
			}));
		}

		task.last_result = Some(result);
		self.mark_node_completed(task, node);
		Ok(None)
	}

	pub(super) fn process_validation_node(
		&self,
		task: &mut Task,
		node: &TaskNode,
		mode: RunMode,
	) -> Result<Option<ResponseEnvelope>, RuntimeError> {
		let evidence_sets = self.collect_validation_evidence(task, node)?;
		if evidence_sets.is_empty() {
			return Err(RuntimeError::new(
				"validation node reached before upstream execution results",
			));
		}
		self.record_transition(task, TaskState::Validating, "validate")?;
		let mut failures = Vec::new();
		for evidence_set in &evidence_sets {
			let report = self.validator.validate_evidence_set(evidence_set);
			if !report.accepted {
				failures.extend(report.failures);
			}
		}
		if !failures.is_empty() {
			self.metrics.inc_failures();
			self.metrics.inc_validation_failures();
			if matches!(mode, RunMode::RetryExhausted) {
				task.attempts = self.orchestrator.config.max_attempts.saturating_sub(1);
			}
			let terminal_state =
				self.fail_task(task, "validation failed", ErrorClass::Validation)?;
			self.fail_experiment_run(task, &failures.join(", "))?;
			self.save_task(task.clone())?;

			return Ok(Some(ResponseEnvelope {
				request_id: task.request_id.clone(),
				status: ResponseStatus::Failed,
				message: failure_message(&failures.join(", "), terminal_state),
				artifacts: Vec::new(),
			}));
		}

		for evidence_set in &evidence_sets {
			self.audit_sink
				.record(
					AuditRecord::new(
						evidence_set.result.producer.clone(),
						"validate",
						evidence_set.result.schema_version.clone(),
						"accepted",
					)
					.with_correlation(AuditCorrelation {
						trace_id: format!("trace-{}", task.request_id.0),
						span_id: "validation".to_string(),
						task_id: Some(task.task_id.0.clone()),
						request_id: Some(task.request_id.0.clone()),
					})
					.with_attribute("node_id", evidence_set.result.node_id.0.clone()),
				)
				.map_err(|error| RuntimeError::new(error.to_string()))?;
		}
		self.mark_node_completed(task, node);

		Ok(None)
	}

	pub(super) fn process_aggregation_node(
		&self,
		task: &mut Task,
		node: &TaskNode,
	) -> Result<(), RuntimeError> {
		let result_set = self.collect_node_result_set(task, node)?;
		if result_set.results.is_empty() {
			return Err(RuntimeError::new(format!(
				"aggregation node {} has no upstream results",
				node.node_id.0
			)));
		}
		self.mark_node_completed(task, node);
		Ok(())
	}

	fn finalize_task(
		&self,
		task: &mut Task,
		request_id: roku_common_types::RequestId,
	) -> Result<ResponseEnvelope, RuntimeError> {
		let completion = self.supervisor.assess_completion(task)?;
		if !completion.completed {
			return Err(RuntimeError::new(completion.reason));
		}
		if task.state != TaskState::Aggregating {
			self.record_transition(task, TaskState::Aggregating, "aggregate")?;
		}
		self.record_transition(task, TaskState::Succeeded, "done")?;
		let result_count = self.list_results(&task.task_id)?.len();
		self.complete_experiment_run(task, result_count)?;
		let artifacts = self
			.list_artifacts(&task.task_id)?
			.into_iter()
			.map(|artifact| artifact.uri)
			.collect();
		self.save_task(task.clone())?;

		Ok(ResponseEnvelope {
			request_id,
			status: ResponseStatus::Succeeded,
			message: success_message(task.last_result.as_ref()),
			artifacts,
		})
	}

	fn normalize_task_for_resume(
		&self,
		task: &mut Task,
		target_state: TaskState,
	) -> Result<(), RuntimeError> {
		for next_state in resume_transition_path(task.state, target_state)? {
			self.record_transition(task, next_state, "resume")?;
		}
		Ok(())
	}
}

fn classify_resume_state(ready_nodes: &[TaskNode]) -> TaskState {
	if ready_nodes
		.iter()
		.any(|node| matches!(node.kind, TaskNodeKind::Aggregation))
	{
		TaskState::Validating
	} else {
		TaskState::Executing
	}
}

fn resume_transition_path(
	current_state: TaskState,
	target_state: TaskState,
) -> Result<Vec<TaskState>, RuntimeError> {
	use TaskState::{
		Aggregating, Delegating, Executing, Failed, GraphBuilding, Planning, Validating,
	};

	let path = match (current_state, target_state) {
		(state, target) if state == target => Vec::new(),
		(Failed, Executing) => vec![Planning, GraphBuilding, Delegating, Executing],
		(Failed, Validating) => vec![Planning, GraphBuilding, Delegating, Executing, Validating],
		(Planning, Executing) => vec![GraphBuilding, Delegating, Executing],
		(Planning, Validating) => vec![GraphBuilding, Delegating, Executing, Validating],
		(GraphBuilding, Executing) => vec![Delegating, Executing],
		(GraphBuilding, Validating) => vec![Delegating, Executing, Validating],
		(Delegating, Executing) => vec![Executing],
		(Delegating, Validating) => vec![Executing, Validating],
		(Executing, Validating) => vec![Validating],
		(Validating, Executing) => vec![Executing],
		(Aggregating, Aggregating) => Vec::new(),
		_ => {
			return Err(RuntimeError::new(format!(
				"task cannot be resumed from state {:?} toward {:?}",
				current_state, target_state
			)));
		}
	};

	Ok(path)
}
