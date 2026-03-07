//! Reusable runtime orchestration service.

use std::sync::{Arc, Mutex};

use roku_agent_instance_factory::AgentInstanceFactory;
use roku_agent_runtime::{AgentWorker, GenericAgentRuntime};
use roku_capability_auth::{CapabilityAuthority, CapabilityRequest};
use roku_common_types::{
	ApprovalDecision, ApprovalId, ApprovalStatus, ApprovalTicket, ErrorClass, RequestEnvelope,
	ResponseEnvelope, ResponseStatus, RuntimeError, Task, TaskNode, TaskNodeKind, TaskState,
};
use roku_execution_graph_builder::{ExecutionGraphBuilder, GraphBuildConfig, TaskGraphScheduler};
use roku_observability::{AuditRecord, AuditSink, InMemoryAuditSink, Metrics, MetricsSnapshot};
use roku_orchestrator::Orchestrator;
use roku_planning_engine::{DefaultPlanningEngine, PlanningInput, RiskLevel, StrategySelector};
use roku_state_store::{
	ApprovalRepository, EventRepository, InMemoryApprovalRepository, InMemoryEventRepository,
	InMemoryTaskRepository, TaskRepository,
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
	approval_repo: Box<dyn ApprovalRepository + Send>,
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
		approval_repo: Box<dyn ApprovalRepository + Send>,
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
				approval_repo,
			}),
		}
	}

	pub fn in_memory() -> Self {
		Self::new(
			Box::new(InMemoryTaskRepository::default()),
			Box::new(InMemoryEventRepository::default()),
			Box::new(InMemoryApprovalRepository::default()),
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
		task.completed_nodes = Vec::new();
		task.next_node_index = 0;
		task.pending_approval_id = None;
		task.last_result = None;

		self.record_transition(&mut task, TaskState::Delegating, "delegate")?;
		self.process_task(&mut task, mode)
	}

	pub fn get_approval(
		&self,
		approval_id: &ApprovalId,
	) -> Result<Option<ApprovalTicket>, RuntimeError> {
		let state = self.lock_state()?;
		state
			.approval_repo
			.load_ticket(approval_id)
			.map_err(|error| RuntimeError::new(error.to_string()))
	}

	pub fn decide_approval(
		&self,
		approval_id: &ApprovalId,
		decision: ApprovalDecision,
	) -> Result<ResponseEnvelope, RuntimeError> {
		let (mut task, mut ticket) = {
			let state = self.lock_state()?;
			let ticket = state
				.approval_repo
				.load_ticket(approval_id)
				.map_err(|error| RuntimeError::new(error.to_string()))?
				.ok_or_else(|| RuntimeError::new("approval ticket not found"))?;
			let task = state
				.task_repo
				.load_task(&ticket.task_id)
				.map_err(|error| RuntimeError::new(error.to_string()))?
				.ok_or_else(|| RuntimeError::new("task for approval ticket not found"))?;
			(task, ticket)
		};

		if ticket.status != ApprovalStatus::Pending {
			return Err(RuntimeError::new(
				"approval ticket has already been decided",
			));
		}
		if task.state != TaskState::WaitingApproval {
			return Err(RuntimeError::new("task is not waiting for approval"));
		}
		if task.pending_approval_id.as_ref() != Some(approval_id) {
			return Err(RuntimeError::new(
				"approval ticket does not match task state",
			));
		}

		ticket.status = if decision.approved {
			ApprovalStatus::Approved
		} else {
			ApprovalStatus::Rejected
		};
		ticket.decided_by = Some(decision.actor.clone());
		ticket.comment = decision.comment.clone();
		self.save_approval_ticket(ticket.clone())?;

		self.audit_sink
			.record(AuditRecord {
				actor: decision.actor.clone(),
				action: if decision.approved {
					"approve".to_string()
				} else {
					"reject".to_string()
				},
				resource: approval_id.0.clone(),
				outcome: ticket_status_label(ticket.status).to_string(),
			})
			.map_err(|error| RuntimeError::new(error.to_string()))?;

		task.pending_approval_id = None;
		if decision.approved {
			self.mark_node_completed_by_id(&mut task, &ticket.node_id);
			self.record_transition(&mut task, TaskState::Executing, "approval granted")?;
			self.process_task(&mut task, RunMode::Normal)
		} else {
			self.metrics.inc_failures();
			let terminal_state =
				self.fail_task(&mut task, "approval rejected", ErrorClass::NonRetriable)?;
			self.save_task(task)?;

			Ok(ResponseEnvelope {
				request_id: ticket.request_id,
				status: ResponseStatus::Failed,
				message: failure_message("approval rejected", terminal_state),
				artifacts: Vec::new(),
			})
		}
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

	fn process_task(
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
						self.mark_node_completed(task, &node);
					}
				}
			}
		}

		self.record_transition(task, TaskState::Aggregating, "aggregate")?;
		self.record_transition(task, TaskState::Succeeded, "done")?;
		let _ = self.metrics.snapshot();
		self.save_task(task.clone())?;

		Ok(ResponseEnvelope {
			request_id,
			status: ResponseStatus::Succeeded,
			message: "task succeeded".to_string(),
			artifacts: vec!["artifact://result/1".to_string()],
		})
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

	fn save_approval_ticket(&self, ticket: ApprovalTicket) -> Result<(), RuntimeError> {
		let mut state = self.lock_state()?;
		state
			.approval_repo
			.save_ticket(ticket)
			.map_err(|error| RuntimeError::new(error.to_string()))
	}

	fn process_execution_node(
		&self,
		task: &mut Task,
		node: &TaskNode,
		mode: RunMode,
	) -> Result<Option<ResponseEnvelope>, RuntimeError> {
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
			let terminal_state = self.fail_task(task, "capability denied", ErrorClass::Security)?;
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
		if matches!(mode, RunMode::MissingEvidence | RunMode::RetryExhausted) {
			result.evidence.clear();
		}

		task.last_result = Some(result);
		self.mark_node_completed(task, node);
		Ok(None)
	}

	fn process_approval_node(
		&self,
		task: &mut Task,
		node: &TaskNode,
	) -> Result<ResponseEnvelope, RuntimeError> {
		let approval_id = ApprovalId(format!("approval-{}-{}", task.task_id.0, node.node_id.0));
		let ticket = ApprovalTicket {
			approval_id: approval_id.clone(),
			task_id: task.task_id.clone(),
			request_id: task.request_id.clone(),
			node_id: node.node_id.clone(),
			summary: node.description.clone(),
			status: ApprovalStatus::Pending,
			decided_by: None,
			comment: None,
		};

		self.record_transition(task, TaskState::WaitingApproval, "approval required")?;
		task.pending_approval_id = Some(approval_id.clone());
		self.save_approval_ticket(ticket)?;
		self.save_task(task.clone())?;

		Ok(ResponseEnvelope {
			request_id: task.request_id.clone(),
			status: ResponseStatus::PendingApproval,
			message: format!("approval required for {}", node.node_id.0),
			artifacts: vec![approval_artifact(&approval_id)],
		})
	}

	fn process_validation_node(
		&self,
		task: &mut Task,
		node: &TaskNode,
		mode: RunMode,
	) -> Result<Option<ResponseEnvelope>, RuntimeError> {
		let result = task
			.last_result
			.clone()
			.ok_or_else(|| RuntimeError::new("validation node reached before execution result"))?;
		self.record_transition(task, TaskState::Validating, "validate")?;
		let report = self.validator.validate(&result);
		if !report.accepted {
			self.metrics.inc_failures();
			self.metrics.inc_validation_failures();
			if matches!(mode, RunMode::RetryExhausted) {
				task.attempts = self.orchestrator.config.max_attempts.saturating_sub(1);
			}
			let terminal_state =
				self.fail_task(task, "validation failed", ErrorClass::Validation)?;
			self.save_task(task.clone())?;

			return Ok(Some(ResponseEnvelope {
				request_id: task.request_id.clone(),
				status: ResponseStatus::Failed,
				message: failure_message(&report.failures.join(", "), terminal_state),
				artifacts: Vec::new(),
			}));
		}

		self.audit_sink
			.record(AuditRecord {
				actor: result.producer.clone(),
				action: "validate".to_string(),
				resource: result.schema_version.clone(),
				outcome: "accepted".to_string(),
			})
			.map_err(|error| RuntimeError::new(error.to_string()))?;
		self.mark_node_completed(task, node);

		Ok(None)
	}

	fn mark_node_completed(&self, task: &mut Task, node: &TaskNode) {
		self.mark_node_completed_by_id(task, &node.node_id);
	}

	fn mark_node_completed_by_id(&self, task: &mut Task, node_id: &roku_common_types::NodeId) {
		if task
			.completed_nodes
			.iter()
			.all(|completed| completed != node_id)
		{
			task.completed_nodes.push(node_id.clone());
		}
		task.next_node_index = task.completed_nodes.len();
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

fn approval_artifact(approval_id: &ApprovalId) -> String {
	format!("approval://{}", approval_id.0)
}

fn ticket_status_label(status: ApprovalStatus) -> &'static str {
	match status {
		ApprovalStatus::Pending => "pending",
		ApprovalStatus::Approved => "approved",
		ApprovalStatus::Rejected => "rejected",
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
		assert_eq!(response.artifacts.len(), 1);
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

	#[test]
	fn service_resumes_after_approval_is_granted() {
		let service = RuntimeService::default();
		let response = service
			.execute_with_mode(sample_request(), RunMode::ApprovalRequired)
			.expect("runtime service should create approval ticket");
		let approval_id = ApprovalId(
			response.artifacts[0]
				.trim_start_matches("approval://")
				.to_string(),
		);

		let resumed = service
			.decide_approval(
				&approval_id,
				ApprovalDecision {
					actor: "reviewer".to_string(),
					approved: true,
					comment: Some("approved".to_string()),
				},
			)
			.expect("approval grant should resume task");

		assert_eq!(resumed.status, ResponseStatus::Succeeded);
	}

	#[test]
	fn service_fails_when_approval_is_rejected() {
		let service = RuntimeService::default();
		let response = service
			.execute_with_mode(sample_request(), RunMode::ApprovalRequired)
			.expect("runtime service should create approval ticket");
		let approval_id = ApprovalId(
			response.artifacts[0]
				.trim_start_matches("approval://")
				.to_string(),
		);

		let resumed = service
			.decide_approval(
				&approval_id,
				ApprovalDecision {
					actor: "reviewer".to_string(),
					approved: false,
					comment: Some("rejected".to_string()),
				},
			)
			.expect("approval rejection should finish task");

		assert_eq!(resumed.status, ResponseStatus::Failed);
		assert!(resumed.message.contains("approval rejected"));
	}

	#[test]
	fn approval_ticket_cannot_be_decided_twice() {
		let service = RuntimeService::default();
		let response = service
			.execute_with_mode(sample_request(), RunMode::ApprovalRequired)
			.expect("runtime service should create approval ticket");
		let approval_id = ApprovalId(
			response.artifacts[0]
				.trim_start_matches("approval://")
				.to_string(),
		);

		service
			.decide_approval(
				&approval_id,
				ApprovalDecision {
					actor: "reviewer".to_string(),
					approved: true,
					comment: None,
				},
			)
			.expect("first decision should succeed");

		let duplicate = service.decide_approval(
			&approval_id,
			ApprovalDecision {
				actor: "reviewer".to_string(),
				approved: true,
				comment: None,
			},
		);
		assert!(duplicate.is_err());
	}
}
