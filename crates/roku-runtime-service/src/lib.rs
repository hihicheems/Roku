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
	ApprovalDecision, ApprovalId, ApprovalStatus, ApprovalTicket, ErrorClass, PlanningModeHint,
	RequestEnvelope, ResponseEnvelope, ResponseStatus, RuntimeError, Task, TaskNode, TaskState,
};
use roku_execution_graph_builder::{ExecutionGraphBuilder, GraphBuildConfig};
use roku_experiment_registry::ExperimentRegistry;
use roku_observability::{
	AuditCorrelation, AuditRecord, AuditSink, InMemoryAuditSink, LogLevel, LogRecord, Metrics,
	MetricsSnapshot, emit_global_log,
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
	planner: Box<dyn TaskPlanner + Send + Sync>,
	builder: ExecutionGraphBuilder,
	factory: AgentInstanceFactory,
	runtime: GenericAgentRuntime,
	validator: ValidationPipeline,
	metrics: Arc<Metrics>,
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
		Self::new_with_data_plane_and_runtime_and_metrics(
			task_repo,
			event_repo,
			approval_repo,
			result_repo,
			ArtifactStore::default(),
			ExperimentRegistry::default(),
			audit_sink,
			GenericAgentRuntime::default(),
			Arc::new(Metrics::default()),
			Box::new(AdaptiveTaskPlanner),
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
		Self::new_with_data_plane_and_runtime_and_metrics(
			task_repo,
			event_repo,
			approval_repo,
			result_repo,
			artifact_store,
			experiment_registry,
			audit_sink,
			GenericAgentRuntime::default(),
			Arc::new(Metrics::default()),
			Box::new(AdaptiveTaskPlanner),
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
		Self::new_with_data_plane_and_runtime_and_metrics(
			task_repo,
			event_repo,
			approval_repo,
			result_repo,
			artifact_store,
			experiment_registry,
			audit_sink,
			runtime,
			Arc::new(Metrics::default()),
			Box::new(AdaptiveTaskPlanner),
		)
	}

	pub fn new_with_data_plane_and_runtime_and_metrics(
		task_repo: Box<dyn TaskRepository + Send>,
		event_repo: Box<dyn EventRepository + Send>,
		approval_repo: Box<dyn ApprovalRepository + Send>,
		result_repo: Box<dyn ResultRepository + Send>,
		artifact_store: ArtifactStore,
		experiment_registry: ExperimentRegistry,
		audit_sink: Arc<dyn AuditSink>,
		runtime: GenericAgentRuntime,
		metrics: Arc<Metrics>,
		planner: Box<dyn TaskPlanner + Send + Sync>,
	) -> Self {
		Self {
			orchestrator: Orchestrator::default(),
			planning_engine: DefaultPlanningEngine,
			planner,
			builder: ExecutionGraphBuilder,
			factory: AgentInstanceFactory::default(),
			runtime,
			validator: ValidationPipeline::default(),
			metrics,
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
		Self::new_with_data_plane_and_runtime_and_metrics(
			Box::new(InMemoryTaskRepository::default()),
			Box::new(InMemoryEventRepository::default()),
			Box::new(InMemoryApprovalRepository::default()),
			Box::new(InMemoryResultRepository::default()),
			ArtifactStore::default(),
			ExperimentRegistry::default(),
			Arc::new(InMemoryAuditSink::default()),
			runtime,
			Arc::new(Metrics::default()),
			Box::new(AdaptiveTaskPlanner),
		)
	}

	pub fn in_memory_with_agent_runtime_and_metrics(
		runtime: GenericAgentRuntime,
		metrics: Arc<Metrics>,
	) -> Self {
		Self::new_with_data_plane_and_runtime_and_metrics(
			Box::new(InMemoryTaskRepository::default()),
			Box::new(InMemoryEventRepository::default()),
			Box::new(InMemoryApprovalRepository::default()),
			Box::new(InMemoryResultRepository::default()),
			ArtifactStore::default(),
			ExperimentRegistry::default(),
			Arc::new(InMemoryAuditSink::default()),
			runtime,
			metrics,
			Box::new(AdaptiveTaskPlanner),
		)
	}

	pub fn in_memory_with_agent_runtime_planner_and_metrics(
		runtime: GenericAgentRuntime,
		planner: Box<dyn TaskPlanner + Send + Sync>,
		metrics: Arc<Metrics>,
	) -> Self {
		Self::new_with_data_plane_and_runtime_and_metrics(
			Box::new(InMemoryTaskRepository::default()),
			Box::new(InMemoryEventRepository::default()),
			Box::new(InMemoryApprovalRepository::default()),
			Box::new(InMemoryResultRepository::default()),
			ArtifactStore::default(),
			ExperimentRegistry::default(),
			Arc::new(InMemoryAuditSink::default()),
			runtime,
			metrics,
			planner,
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
		log_runtime(
			LogLevel::Info,
			"received runtime request",
			[
				("request_id", request.request_id.0.clone()),
				("session_id", request.session_id.clone()),
				("mode", format!("{mode:?}")),
				("goal", truncate_for_log(&request.goal, 200)),
			],
		);
		let mut task = self.orchestrator.create_task(&request);

		self.record_transition(&mut task, TaskState::Planning, "start planning")?;

		let planning_input = planning_input_for_request(&request);
		let decision = request
			.planning_mode_hint
			.map(|hint| {
				self.planning_engine
					.decision_for_mode(planning_mode_from_hint(hint), &planning_input)
			})
			.unwrap_or_else(|| self.planning_engine.select(&planning_input));
		let planning_mode_label = format!("{:?}", decision.mode);
		log_runtime(
			LogLevel::Info,
			"selected planning mode",
			[
				("request_id", request.request_id.0.clone()),
				("planning_mode", planning_mode_label.clone()),
				(
					"complexity_score",
					planning_input.complexity_score.to_string(),
				),
				(
					"uncertainty_score",
					planning_input.uncertainty_score.to_string(),
				),
				("risk_level", format!("{:?}", planning_input.risk_level)),
				("budget_tokens", planning_input.budget_tokens.to_string()),
			],
		);
		self.metrics.inc_planning_run();
		self.metrics.inc_planning_strategy(&planning_mode_label);
		let mut outline = self.planner.build_outline(&request, &decision);
		log_runtime(
			LogLevel::Info,
			"built plan outline",
			[
				("request_id", request.request_id.0.clone()),
				("outline_steps", outline.steps.len().to_string()),
			],
		);
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
		let approval_id = ApprovalId(compact_approval_id(&task.task_id.0, &node.node_id.0));
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

pub(crate) fn planning_input_for_request(request: &RequestEnvelope) -> PlanningInput {
	let goal = request.goal.to_ascii_lowercase();
	let word_count = u64::try_from(goal.split_whitespace().count()).unwrap_or(u64::MAX);
	let complexity_keywords = [
		"and",
		"then",
		"compare",
		"analyze",
		"research",
		"plan",
		"build",
		"integrate",
		"deploy",
		"workflow",
	];
	let uncertainty_keywords = [
		"maybe",
		"explore",
		"option",
		"alternatives",
		"unknown",
		"unclear",
		"investigate",
		"hypothesis",
		"why",
	];
	let high_risk_keywords = [
		"delete",
		"production",
		"payment",
		"secret",
		"credential",
		"approve",
	];
	let medium_risk_keywords = ["write", "publish", "external", "notify", "mutation"];

	let complexity_hits = keyword_hits(&goal, &complexity_keywords);
	let uncertainty_hits = keyword_hits(&goal, &uncertainty_keywords);
	let complexity_score = score_from_hits(word_count, complexity_hits, 6, 10);
	let uncertainty_score = score_from_hits(word_count / 8, uncertainty_hits, 4, 10);
	let risk_level = if contains_any_keyword(&goal, &high_risk_keywords) {
		RiskLevel::High
	} else if contains_any_keyword(&goal, &medium_risk_keywords) {
		RiskLevel::Medium
	} else {
		RiskLevel::Low
	};
	let risk_budget = match risk_level {
		RiskLevel::Low => 0,
		RiskLevel::Medium => 2_000,
		RiskLevel::High => 4_000,
	};
	let budget_tokens = 4_000u64
		.saturating_add(word_count.saturating_mul(120))
		.saturating_add(u64::from(complexity_score).saturating_mul(250))
		.saturating_add(u64::from(uncertainty_score).saturating_mul(150))
		.saturating_add(risk_budget);

	PlanningInput {
		complexity_score,
		uncertainty_score,
		risk_level,
		budget_tokens,
	}
}

fn planning_mode_from_hint(hint: PlanningModeHint) -> roku_planning_engine::PlanningMode {
	match hint {
		PlanningModeHint::ReAct => roku_planning_engine::PlanningMode::ReAct,
		PlanningModeHint::TaskDecomposition => {
			roku_planning_engine::PlanningMode::TaskDecomposition
		}
		PlanningModeHint::TreeSearch => roku_planning_engine::PlanningMode::TreeSearch,
		PlanningModeHint::IterativeRefinement => {
			roku_planning_engine::PlanningMode::IterativeRefinement
		}
	}
}

fn keyword_hits(goal: &str, keywords: &[&str]) -> u8 {
	let hits = keywords
		.iter()
		.filter(|keyword| goal.contains(**keyword))
		.count();
	u8::try_from(hits).unwrap_or(u8::MAX)
}

fn contains_any_keyword(goal: &str, keywords: &[&str]) -> bool {
	keywords.iter().any(|keyword| goal.contains(keyword))
}

fn score_from_hits(base: u64, hits: u8, divisor: u64, max_score: u8) -> u8 {
	let derived = 2u64
		.saturating_add(base / divisor)
		.saturating_add(u64::from(hits).saturating_mul(2));
	let capped = derived.min(u64::from(max_score));
	u8::try_from(capped).unwrap_or(max_score)
}

fn truncate_for_log(value: &str, max_chars: usize) -> String {
	let mut chars = value.chars();
	let truncated = chars.by_ref().take(max_chars).collect::<String>();
	if chars.next().is_some() {
		format!("{truncated}...")
	} else {
		truncated
	}
}

fn log_runtime(
	level: LogLevel,
	message: &str,
	fields: impl IntoIterator<Item = (&'static str, String)>,
) {
	let record = fields.into_iter().fold(
		LogRecord::new("roku-runtime-service", level, message),
		|record, (key, value)| record.with_field(key, value),
	);
	let _ = emit_global_log(record);
}

pub(crate) fn compact_approval_id(task_id: &str, node_id: &str) -> String {
	let mut hash = 0xcbf29ce484222325u64;
	for byte in task_id.bytes().chain(node_id.bytes()) {
		hash ^= u64::from(byte);
		hash = hash.wrapping_mul(0x100000001b3);
	}

	format!("ap-{hash:016x}")
}
