//! Reusable runtime orchestration service.

use std::sync::{Arc, Mutex};

use roku_agent_instance_factory::AgentInstanceFactory;
use roku_agent_runtime::{AgentWorker, GenericAgentRuntime};
use roku_capability_auth::{CapabilityAuthority, CapabilityRequest};
use roku_common_types::{
	RequestEnvelope, ResponseEnvelope, ResponseStatus, RuntimeError, Task, TaskState,
};
use roku_execution_graph_builder::{ExecutionGraphBuilder, GraphBuildConfig};
use roku_observability::{AuditRecord, AuditSink, InMemoryAuditSink, Metrics, MetricsSnapshot};
use roku_orchestrator::Orchestrator;
use roku_planning_engine::{DefaultPlanningEngine, PlanningInput, RiskLevel, StrategySelector};
use roku_state_store::{
	EventRepository, InMemoryEventRepository, InMemoryTaskRepository, TaskRepository,
};
use roku_task_planner::{SimpleTaskPlanner, TaskPlanner};
use roku_validation_plane::ValidationPipeline;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RunMode {
	#[default]
	Normal,
	MissingEvidence,
	CapabilityDenied,
	ApprovalRequired,
	RetryExhausted,
}

struct RuntimeState {
	capability_auth: CapabilityAuthority,
	task_repo: Box<dyn TaskRepository + Send>,
	event_repo: Box<dyn EventRepository + Send>,
}

pub struct RuntimeService {
	orchestrator: Orchestrator,
	planning_engine: DefaultPlanningEngine,
	planner: SimpleTaskPlanner,
	builder: ExecutionGraphBuilder,
	factory: AgentInstanceFactory,
	runtime: GenericAgentRuntime,
	validator: ValidationPipeline,
	metrics: Metrics,
	audit_sink: Arc<dyn AuditSink>,
	state: Mutex<RuntimeState>,
}

impl RuntimeService {
	pub fn new(
		task_repo: Box<dyn TaskRepository + Send>,
		event_repo: Box<dyn EventRepository + Send>,
		audit_sink: Arc<dyn AuditSink>,
	) -> Self {
		Self {
			orchestrator: Orchestrator::default(),
			planning_engine: DefaultPlanningEngine,
			planner: SimpleTaskPlanner,
			builder: ExecutionGraphBuilder,
			factory: AgentInstanceFactory,
			runtime: GenericAgentRuntime,
			validator: ValidationPipeline::default(),
			metrics: Metrics::default(),
			audit_sink,
			state: Mutex::new(RuntimeState {
				capability_auth: CapabilityAuthority::default(),
				task_repo,
				event_repo,
			}),
		}
	}

	pub fn in_memory() -> Self {
		Self::new(
			Box::new(InMemoryTaskRepository::default()),
			Box::new(InMemoryEventRepository::default()),
			Arc::new(InMemoryAuditSink::default()),
		)
	}

	pub fn execute(&self, request: RequestEnvelope) -> Result<ResponseEnvelope, RuntimeError> {
		self.execute_with_mode(request, RunMode::Normal)
	}

	pub fn execute_with_mode(
		&self,
		request: RequestEnvelope,
		mode: RunMode,
	) -> Result<ResponseEnvelope, RuntimeError> {
		self.metrics.inc_requests();
		let mut task = self.orchestrator.create_task(&request);

		self.record_transition(&mut task, TaskState::Planning, "start planning")?;

		let decision = self.planning_engine.select(&PlanningInput {
			complexity_score: 5,
			uncertainty_score: 4,
			risk_level: RiskLevel::Low,
			budget_tokens: 10_000,
		});
		let mut outline = self.planner.build_outline(&request, decision.mode);
		if matches!(mode, RunMode::ApprovalRequired)
			&& let Some(step) = outline.steps.last_mut()
		{
			step.requires_approval = true;
		}

		self.record_transition(&mut task, TaskState::GraphBuilding, "build graph")?;
		let graph =
			self.builder
				.compile(task.task_id.clone(), &outline, &GraphBuildConfig::default());
		task.graph = Some(graph.clone());

		self.record_transition(&mut task, TaskState::Delegating, "delegate")?;

		let mut latest_result = None;
		for node in &graph.nodes {
			match node.kind {
				roku_common_types::TaskNodeKind::Execution => {
					let spec = self.factory.build_for_node(&task.task_id, node);
					let capability_allowed = {
						let mut state = self.lock_state()?;
						let token = state.capability_auth.issue(CapabilityRequest {
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
						let terminal_state = self.fail_task(
							&mut task,
							"capability denied",
							roku_common_types::ErrorClass::Security,
						)?;

						return Ok(ResponseEnvelope {
							request_id: request.request_id,
							status: ResponseStatus::Failed,
							message: failure_message("capability denied", terminal_state),
							artifacts: Vec::new(),
						});
					}

					if task.state != TaskState::Executing {
						self.record_transition(&mut task, TaskState::Executing, "execute")?;
					}
					let mut result = self.runtime.execute(&spec, node);
					if matches!(mode, RunMode::MissingEvidence | RunMode::RetryExhausted) {
						result.evidence.clear();
					}
					latest_result = Some(result);
				}
				roku_common_types::TaskNodeKind::Approval => {
					self.record_transition(
						&mut task,
						TaskState::WaitingApproval,
						"approval required",
					)?;
					self.save_task(task)?;

					return Ok(ResponseEnvelope {
						request_id: request.request_id,
						status: ResponseStatus::PendingApproval,
						message: format!("approval required for {}", node.node_id.0),
						artifacts: Vec::new(),
					});
				}
				roku_common_types::TaskNodeKind::Validation => {
					let result = latest_result.as_ref().ok_or_else(|| {
						RuntimeError::new("validation node reached before execution result")
					})?;
					self.record_transition(&mut task, TaskState::Validating, "validate")?;
					let report = self.validator.validate(result);
					if !report.accepted {
						self.metrics.inc_failures();
						self.metrics.inc_validation_failures();
						if matches!(mode, RunMode::RetryExhausted) {
							task.attempts = self.orchestrator.config.max_attempts.saturating_sub(1);
						}
						let terminal_state = self.fail_task(
							&mut task,
							"validation failed",
							roku_common_types::ErrorClass::Validation,
						)?;

						return Ok(ResponseEnvelope {
							request_id: request.request_id,
							status: ResponseStatus::Failed,
							message: failure_message(&report.failures.join(", "), terminal_state),
							artifacts: Vec::new(),
						});
					}

					self.audit_sink
						.record(AuditRecord {
							actor: result.producer.clone(),
							action: "validate".to_string(),
							resource: result.schema_version.clone(),
							outcome: "accepted".to_string(),
						})
						.map_err(|error| RuntimeError::new(error.to_string()))?;
				}
				roku_common_types::TaskNodeKind::Aggregation => {}
			}
		}

		let response = {
			self.record_transition(&mut task, TaskState::Aggregating, "aggregate")?;
			self.record_transition(&mut task, TaskState::Succeeded, "done")?;

			ResponseEnvelope {
				request_id: request.request_id,
				status: ResponseStatus::Succeeded,
				message: "task succeeded".to_string(),
				artifacts: vec!["artifact://result/1".to_string()],
			}
		};

		let _ = self.metrics.snapshot();
		self.save_task(task)?;

		Ok(response)
	}

	pub fn metrics_snapshot(&self) -> MetricsSnapshot {
		self.metrics.snapshot()
	}

	fn record_transition(
		&self,
		task: &mut Task,
		next: TaskState,
		reason: &str,
	) -> Result<(), RuntimeError> {
		let event = self.orchestrator.transition(task, next, reason, None)?;
		let mut state = self.lock_state()?;
		state
			.event_repo
			.append_event(event)
			.map_err(|error| RuntimeError::new(error.to_string()))
	}

	fn save_task(&self, task: Task) -> Result<(), RuntimeError> {
		let mut state = self.lock_state()?;
		state
			.task_repo
			.save_task(task)
			.map_err(|error| RuntimeError::new(error.to_string()))
	}

	fn lock_state(&self) -> Result<std::sync::MutexGuard<'_, RuntimeState>, RuntimeError> {
		self.state
			.lock()
			.map_err(|_| RuntimeError::new("runtime state lock poisoned"))
	}

	fn fail_task(
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
		Ok(disposition.terminal_state)
	}
}

impl Default for RuntimeService {
	fn default() -> Self {
		Self::in_memory()
	}
}

fn failure_message(reason: &str, terminal_state: TaskState) -> String {
	if terminal_state == TaskState::DeadLetter {
		format!("task dead-lettered: {reason}")
	} else {
		format!("task failed: {reason}")
	}
}

#[cfg(test)]
mod tests {
	use roku_common_types::{RequestId, ResponseStatus};

	use super::*;

	fn sample_request() -> RequestEnvelope {
		RequestEnvelope {
			request_id: RequestId("req-1".to_string()),
			session_id: "session-1".to_string(),
			goal: "analyze market".to_string(),
		}
	}

	#[test]
	fn service_succeeds_for_happy_path() {
		let service = RuntimeService::default();
		let response = service
			.execute(sample_request())
			.expect("runtime service should succeed");

		assert_eq!(response.status, ResponseStatus::Succeeded);
	}

	#[test]
	fn service_reports_validation_failure() {
		let service = RuntimeService::default();
		let response = service
			.execute_with_mode(sample_request(), RunMode::MissingEvidence)
			.expect("runtime service should return failed response");

		assert_eq!(response.status, ResponseStatus::Failed);
		assert!(response.message.contains("evidence is required"));
	}

	#[test]
	fn service_reports_capability_denied() {
		let service = RuntimeService::default();
		let response = service
			.execute_with_mode(sample_request(), RunMode::CapabilityDenied)
			.expect("runtime service should return failed response");

		assert_eq!(response.status, ResponseStatus::Failed);
		assert!(response.message.contains("capability denied"));
	}

	#[test]
	fn service_returns_pending_approval_when_graph_contains_approval_gate() {
		let service = RuntimeService::default();
		let response = service
			.execute_with_mode(sample_request(), RunMode::ApprovalRequired)
			.expect("runtime service should stop at approval gate");

		assert_eq!(response.status, ResponseStatus::PendingApproval);
		assert!(response.message.contains("approval required"));
	}

	#[test]
	fn service_dead_letters_when_retry_budget_is_exhausted() {
		let service = RuntimeService::default();
		let response = service
			.execute_with_mode(sample_request(), RunMode::RetryExhausted)
			.expect("runtime service should report dead-letter");

		assert_eq!(response.status, ResponseStatus::Failed);
		assert!(response.message.contains("dead-lettered"));
	}
}
