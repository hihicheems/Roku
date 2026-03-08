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
use std::collections::HashMap;

use roku_common_types::{
	ApprovalStatus, CompensationAction, CompensationRecord, CompensationStatus, ErrorClass,
	EvidenceItem, RecoveryEligibility, ResponseEnvelope, ResponseStatus, ResultEnvelope,
	ResultStatus, RuntimeError, Task, TaskId, TaskNode, TaskNodeKind, TaskState,
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
			let ready_nodes_by_id = ready_nodes
				.iter()
				.cloned()
				.map(|node| (node.node_id.0.clone(), node))
				.collect::<HashMap<_, _>>();
			self.enqueue_ready_nodes(task, &ready_nodes)?;

			while let Some(claim) = self.claim_dispatched_node(&task.task_id)? {
				let node = ready_nodes_by_id
					.get(&claim.envelope.node_id.0)
					.ok_or_else(|| {
						RuntimeError::new(format!(
							"dispatched node {} is not ready for task {}",
							claim.envelope.node_id.0, task.task_id.0
						))
					})?;
				match node.kind {
					TaskNodeKind::Execution => {
						if let Some(response) = self.process_execution_node(task, node, mode)? {
							self.ack_dispatched_node(&claim.lease)?;
							return Ok(response);
						}
						self.ack_dispatched_node(&claim.lease)?;
					}
					TaskNodeKind::Approval => {
						let response = self.process_approval_node(task, node)?;
						self.ack_dispatched_node(&claim.lease)?;
						return Ok(response);
					}
					TaskNodeKind::Validation => {
						let report = self.process_validation_node(task, node, mode)?;
						self.ack_dispatched_node(&claim.lease)?;
						if let Some(response) = report {
							return Ok(response);
						}
					}
					TaskNodeKind::Aggregation => {
						self.process_aggregation_node(task, node)?;
						self.ack_dispatched_node(&claim.lease)?;
					}
				}
			}
		}

		self.finalize_task(task, request_id)
	}

	pub fn resume_task(&self, task_id: &TaskId) -> Result<ResponseEnvelope, RuntimeError> {
		let task = self
			.get_task(task_id)?
			.ok_or_else(|| RuntimeError::new(format!("task not found: {}", task_id.0)))?;
		let analysis = self.analyze_task_recovery(&task)?;
		let mut task = analysis.reconstructed_task.clone();

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

	pub fn cancel_task(&self, task_id: &TaskId, actor: &str) -> Result<Task, RuntimeError> {
		let mut task = self
			.get_task(task_id)?
			.ok_or_else(|| RuntimeError::new(format!("task not found: {}", task_id.0)))?;

		match task.state {
			TaskState::Succeeded | TaskState::DeadLetter | TaskState::Cancelled => {
				return Err(RuntimeError::new(format!(
					"task cannot be cancelled from state {:?}",
					task.state
				)));
			}
			TaskState::CancelRequested | TaskState::Compensating => {
				return Err(RuntimeError::new(
					"task cancellation is already in progress",
				));
			}
			_ => {}
		}

		if let Some(approval_id) = task.pending_approval_id.clone() {
			self.cancel_pending_approval_ticket(&approval_id, actor)?;
			task.pending_approval_id = None;
		}

		task.compensation_records = self.plan_compensation_records(&task);
		let has_compensation_work = !task.compensation_records.is_empty();
		let disposition = self.orchestrator.request_cancellation(
			&mut task,
			format!("cancel requested by {actor}"),
			has_compensation_work,
		)?;
		let mut state = self.lock_state()?;
		for event in disposition {
			state
				.event_repo
				.append_event(event)
				.map_err(|error| RuntimeError::new(error.to_string()))?;
		}
		drop(state);

		self.complete_compensation_records(&mut task, actor);
		if self.get_experiment_run(&task.task_id)?.is_some() {
			self.fail_experiment_run(&task, "task cancelled")?;
		}
		self.save_task(task.clone())?;
		Ok(task)
	}

	pub fn recover_timed_out_task(
		&self,
		task_id: &TaskId,
	) -> Result<ResponseEnvelope, RuntimeError> {
		let task = self
			.get_task(task_id)?
			.ok_or_else(|| RuntimeError::new(format!("task not found: {}", task_id.0)))?;
		if task.state != TaskState::TimeoutRecovering {
			return Err(RuntimeError::new(format!(
				"task is not waiting for timeout recovery from state {:?}",
				task.state
			)));
		}

		self.resume_task(task_id)
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
		if matches!(mode, RunMode::TimeoutRecovery) {
			self.record_transition_with_error_class(
				task,
				TaskState::TimeoutRecovering,
				"execution timed out",
				Some(ErrorClass::Timeout),
			)?;
			self.save_task(task.clone())?;

			return Ok(Some(ResponseEnvelope {
				request_id: task.request_id.clone(),
				status: ResponseStatus::Failed,
				message: "task timed out and entered recovery flow".to_string(),
				artifacts: Vec::new(),
			}));
		}
		let mut result = self.runtime.execute(&spec, node);
		if let Some(limit_ms) = node_time_budget_limit_ms(&spec, node)
			&& let Some(elapsed_ms) = result_elapsed_ms(&result)
			&& elapsed_ms > limit_ms
		{
			result = node_budget_timeout_result(&spec, node, elapsed_ms, limit_ms);
		}
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
			let error_class = classify_result_error(&result);
			if matches!(error_class, ErrorClass::Timeout) && node.retry_policy.retry_on_timeout {
				self.record_transition_with_error_class(
					task,
					TaskState::TimeoutRecovering,
					&reason,
					Some(ErrorClass::Timeout),
				)?;
				self.save_task(task.clone())?;

				return Ok(Some(ResponseEnvelope {
					request_id: task.request_id.clone(),
					status: ResponseStatus::Failed,
					message: "task timed out and entered recovery flow".to_string(),
					artifacts: vec![artifact.uri],
				}));
			}
			let terminal_state = self.fail_task(task, &reason, error_class)?;
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

	fn plan_compensation_records(&self, task: &Task) -> Vec<CompensationRecord> {
		let node_by_id = task
			.graph
			.as_ref()
			.map(|graph| {
				graph
					.nodes
					.iter()
					.map(|node| (node.node_id.clone(), node.kind))
					.collect::<HashMap<_, _>>()
			})
			.unwrap_or_default();

		task.completed_nodes
			.iter()
			.map(|node_id| {
				let action = match node_by_id.get(node_id) {
					Some(TaskNodeKind::Execution) => CompensationAction::AuditOnly,
					_ => CompensationAction::Noop,
				};
				CompensationRecord {
					node_id: node_id.clone(),
					action,
					status: CompensationStatus::Pending,
					note: "cancellation compensation recorded".to_string(),
				}
			})
			.collect()
	}

	fn complete_compensation_records(&self, task: &mut Task, actor: &str) {
		for record in &mut task.compensation_records {
			record.status = CompensationStatus::Completed;
			record.note = format!("{} by {actor}", compensation_note(record.action));
		}
	}

	fn cancel_pending_approval_ticket(
		&self,
		approval_id: &roku_common_types::ApprovalId,
		actor: &str,
	) -> Result<(), RuntimeError> {
		let mut ticket = self
			.get_approval(approval_id)?
			.ok_or_else(|| RuntimeError::new("approval ticket not found for cancellation"))?;
		if ticket.status == ApprovalStatus::Pending {
			ticket.status = ApprovalStatus::Cancelled;
			ticket.decided_by = Some(actor.to_string());
			ticket.comment = Some("task cancelled before approval resolved".to_string());
			self.save_approval_ticket(ticket)?;
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
		Aggregating, Delegating, Executing, Failed, GraphBuilding, Planning, TimeoutRecovering,
		Validating,
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
		(TimeoutRecovering, Executing) => vec![Planning, GraphBuilding, Delegating, Executing],
		(TimeoutRecovering, Validating) => {
			vec![Planning, GraphBuilding, Delegating, Executing, Validating]
		}
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

fn compensation_note(action: CompensationAction) -> &'static str {
	match action {
		CompensationAction::Noop => "noop compensation recorded",
		CompensationAction::AuditOnly => "audit-only compensation recorded",
	}
}

fn node_time_budget_limit_ms(
	spec: &roku_common_types::AgentInstanceSpec,
	node: &TaskNode,
) -> Option<u64> {
	let mut limit_ms = spec.policy_bindings.time_budget_ms;
	if node.budget_snapshot.time_budget_ms > 0 {
		limit_ms = limit_ms.min(node.budget_snapshot.time_budget_ms);
	}
	if node.deadline_ms > 0 {
		limit_ms = limit_ms.min(node.deadline_ms);
	}
	(limit_ms > 0).then_some(limit_ms)
}

fn result_elapsed_ms(result: &ResultEnvelope) -> Option<u64> {
	let payload = serde_json::from_str::<serde_json::Value>(&result.payload).ok()?;
	payload.get("elapsed_ms")?.as_u64()
}

fn node_budget_timeout_result(
	spec: &roku_common_types::AgentInstanceSpec,
	node: &TaskNode,
	elapsed_ms: u64,
	limit_ms: u64,
) -> ResultEnvelope {
	ResultEnvelope {
		task_id: spec.context.task_id.clone(),
		node_id: node.node_id.clone(),
		producer: spec.instance_id.clone(),
		schema_version: "result.v1".to_string(),
		status: ResultStatus::Error,
		payload: serde_json::json!({
			"error_code": "node_deadline_exceeded",
			"message": format!(
				"node exceeded time budget: elapsed {elapsed_ms}ms > limit {limit_ms}ms"
			),
			"node_id": node.node_id.0,
			"elapsed_ms": elapsed_ms,
			"time_budget_ms": limit_ms,
		})
		.to_string(),
		evidence: vec![
			EvidenceItem {
				kind: "policy".to_string(),
				value: "deadline-exceeded".to_string(),
			},
			EvidenceItem {
				kind: "elapsed_ms".to_string(),
				value: elapsed_ms.to_string(),
			},
		],
		confidence: 0.0,
	}
}

fn classify_result_error(result: &ResultEnvelope) -> ErrorClass {
	let payload = serde_json::from_str::<serde_json::Value>(&result.payload).ok();
	let error_code = payload
		.as_ref()
		.and_then(|value| value.get("error_code"))
		.and_then(serde_json::Value::as_str);

	if result
		.evidence
		.iter()
		.any(|item| item.kind == "policy" && item.value == "budget-exhausted")
		|| matches!(error_code, Some("policy_bindings_rejected"))
	{
		ErrorClass::BudgetExhausted
	} else if result
		.evidence
		.iter()
		.any(|item| item.kind == "tool_error" && item.value == "timeout")
		|| matches!(error_code, Some("timeout" | "node_deadline_exceeded"))
	{
		ErrorClass::Timeout
	} else {
		ErrorClass::Dependency
	}
}
