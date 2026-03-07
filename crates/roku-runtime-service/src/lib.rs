//! Reusable runtime orchestration service.

mod data_plane;
mod execution;
mod helpers;
#[cfg(test)]
mod tests;

use std::sync::{Arc, Mutex};

use roku_agent_instance_factory::AgentInstanceFactory;
use roku_agent_runtime::GenericAgentRuntime;
use roku_artifact_store::ArtifactStore;
use roku_capability_auth::CapabilityAuthority;
use roku_common_types::{
	ApprovalDecision, ApprovalId, ApprovalStatus, ApprovalTicket, ErrorClass, RequestEnvelope,
	ResponseEnvelope, ResponseStatus, RuntimeError, Task, TaskNode, TaskState,
};
use roku_execution_graph_builder::{ExecutionGraphBuilder, GraphBuildConfig};
use roku_experiment_registry::ExperimentRegistry;
use roku_observability::{
	AuditCorrelation, AuditRecord, AuditSink, InMemoryAuditSink, Metrics, MetricsSnapshot,
};
use roku_orchestrator::Orchestrator;
use roku_planning_engine::{DefaultPlanningEngine, PlanningInput, RiskLevel, StrategySelector};
use roku_state_store::{
	ApprovalRepository, EventRepository, InMemoryApprovalRepository, InMemoryEventRepository,
	InMemoryResultRepository, InMemoryTaskRepository, ResultRepository, TaskRepository,
};
use roku_task_planner::{AdaptiveTaskPlanner, TaskPlanner};
use roku_validation_plane::ValidationPipeline;

use crate::helpers::{approval_artifact, failure_message, ticket_status_label};

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
	result_repo: Box<dyn ResultRepository + Send>,
	artifact_store: ArtifactStore,
	experiment_registry: ExperimentRegistry,
}

pub struct RuntimeService {
	orchestrator: Orchestrator,
	planning_engine: DefaultPlanningEngine,
	planner: AdaptiveTaskPlanner,
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
		result_repo: Box<dyn ResultRepository + Send>,
		audit_sink: Arc<dyn AuditSink>,
	) -> Self {
		Self::new_with_data_plane_and_runtime(
			task_repo,
			event_repo,
			approval_repo,
			result_repo,
			ArtifactStore::default(),
			ExperimentRegistry::default(),
			audit_sink,
			GenericAgentRuntime::default(),
		)
	}

	pub fn new_with_data_plane(
		task_repo: Box<dyn TaskRepository + Send>,
		event_repo: Box<dyn EventRepository + Send>,
		approval_repo: Box<dyn ApprovalRepository + Send>,
		result_repo: Box<dyn ResultRepository + Send>,
		artifact_store: ArtifactStore,
		experiment_registry: ExperimentRegistry,
		audit_sink: Arc<dyn AuditSink>,
	) -> Self {
		Self::new_with_data_plane_and_runtime(
			task_repo,
			event_repo,
			approval_repo,
			result_repo,
			artifact_store,
			experiment_registry,
			audit_sink,
			GenericAgentRuntime::default(),
		)
	}

	pub fn new_with_data_plane_and_runtime(
		task_repo: Box<dyn TaskRepository + Send>,
		event_repo: Box<dyn EventRepository + Send>,
		approval_repo: Box<dyn ApprovalRepository + Send>,
		result_repo: Box<dyn ResultRepository + Send>,
		artifact_store: ArtifactStore,
		experiment_registry: ExperimentRegistry,
		audit_sink: Arc<dyn AuditSink>,
		runtime: GenericAgentRuntime,
	) -> Self {
		Self {
			orchestrator: Orchestrator::default(),
			planning_engine: DefaultPlanningEngine,
			planner: AdaptiveTaskPlanner,
			builder: ExecutionGraphBuilder,
			factory: AgentInstanceFactory::default(),
			runtime,
			validator: ValidationPipeline::default(),
			metrics: Metrics::default(),
			audit_sink,
			state: Mutex::new(RuntimeState {
				capability_auth: CapabilityAuthority::default(),
				task_repo,
				event_repo,
				approval_repo,
				result_repo,
				artifact_store,
				experiment_registry,
			}),
		}
	}

	pub fn in_memory() -> Self {
		Self::in_memory_with_agent_runtime(GenericAgentRuntime::default())
	}

	pub fn in_memory_with_agent_runtime(runtime: GenericAgentRuntime) -> Self {
		Self::new_with_data_plane_and_runtime(
			Box::new(InMemoryTaskRepository::default()),
			Box::new(InMemoryEventRepository::default()),
			Box::new(InMemoryApprovalRepository::default()),
			Box::new(InMemoryResultRepository::default()),
			ArtifactStore::default(),
			ExperimentRegistry::default(),
			Arc::new(InMemoryAuditSink::default()),
			runtime,
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
		let planning_mode_label = format!("{:?}", decision.mode);
		self.metrics.inc_planning_run();
		self.metrics.inc_planning_strategy(&planning_mode_label);
		let mut outline = self.planner.build_outline(&request, &decision);
		if matches!(mode, RunMode::ApprovalRequired)
			&& let Some(step) = outline.steps.last_mut()
		{
			step.requires_approval = true;
		}

		self.record_transition(&mut task, TaskState::GraphBuilding, "build graph")?;
		let graph =
			match self
				.builder
				.compile(task.task_id.clone(), &outline, &GraphBuildConfig::default())
			{
				Ok(graph) => graph,
				Err(error) => {
					self.metrics.inc_failures();
					let terminal_state =
						self.fail_task(&mut task, "graph build failed", ErrorClass::Dependency)?;
					self.save_task(task)?;
					return Ok(ResponseEnvelope {
						request_id: request.request_id,
						status: ResponseStatus::Failed,
						message: failure_message(&error.to_string(), terminal_state),
						artifacts: Vec::new(),
					});
				}
			};
		task.graph = Some(graph);
		task.completed_nodes = Vec::new();
		task.next_node_index = 0;
		task.pending_approval_id = None;
		task.last_result = None;
		self.start_experiment_run(&task, &request.goal, &planning_mode_label)?;

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
		self.metrics.inc_approvals_resolved();

		self.audit_sink
			.record(
				AuditRecord::new(
					decision.actor.clone(),
					if decision.approved {
						"approve"
					} else {
						"reject"
					},
					approval_id.0.clone(),
					ticket_status_label(ticket.status),
				)
				.with_correlation(AuditCorrelation {
					trace_id: format!("trace-{}", ticket.request_id.0),
					span_id: "approval-decision".to_string(),
					task_id: Some(ticket.task_id.0.clone()),
					request_id: Some(ticket.request_id.0.clone()),
				})
				.with_attribute("approval_status", ticket_status_label(ticket.status)),
			)
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
			self.fail_experiment_run(&task, "approval rejected")?;
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
		self.metrics.inc_approvals_created();
		self.save_approval_ticket(ticket)?;
		self.save_task(task.clone())?;

		Ok(ResponseEnvelope {
			request_id: task.request_id.clone(),
			status: ResponseStatus::PendingApproval,
			message: format!("approval required for {}", node.node_id.0),
			artifacts: vec![approval_artifact(&approval_id)],
		})
	}
}

impl Default for RuntimeService {
	fn default() -> Self {
		Self::in_memory()
	}
}
