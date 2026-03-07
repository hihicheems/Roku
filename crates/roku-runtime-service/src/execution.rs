use roku_agent_runtime::AgentWorker;
use roku_common_types::{
	ErrorClass, EvidenceItem, ResponseEnvelope, ResponseStatus, RuntimeError, Task, TaskNode,
	TaskNodeKind, TaskState,
};
use roku_execution_graph_builder::TaskGraphScheduler;
use roku_observability::AuditRecord;

use crate::helpers::failure_message;
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

		self.record_transition(task, TaskState::Aggregating, "aggregate")?;
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
			message: "task succeeded".to_string(),
			artifacts,
		})
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
		let spec = self.factory.build_for_node(&task.task_id, node);
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
					.record(AuditRecord {
						actor: spec.instance_id.clone(),
						action: "invoke".to_string(),
						resource: token.resource,
						outcome: "denied".to_string(),
					})
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
				.record(AuditRecord {
					actor: evidence_set.result.producer.clone(),
					action: "validate".to_string(),
					resource: evidence_set.result.schema_version.clone(),
					outcome: "accepted".to_string(),
				})
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
}
