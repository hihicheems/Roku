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

//! Reusable runtime orchestration service.

mod data_plane;
mod direct;
mod execution;
mod helpers;
mod legacy_compat;
mod legacy_graph;
mod memory_context;
mod pending_loop_snapshot_store;
mod runtime_loop_lifecycle;
mod runtime_loop_owner;
mod runtime_loop_recovery;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use roku_agent_runtime::{
	EscalationAction, EscalationReason, GenericAgentRuntime, IntentFamily, RouteDecision,
	RouteDecisionResult, RouteEscalationPlan, RouteRisk,
};
use roku_artifact_store::ArtifactStore;
use roku_capability_auth::CapabilityAuthority;
use roku_common_types::{
	ApprovalDecision, ApprovalId, ApprovalStatus, ApprovalTicket, ErrorClass, RequestEnvelope,
	ResponseEnvelope, ResponseStatus, RuntimeError, Task, TaskEventKind, TaskNode, TaskState,
};
use roku_experiment_registry::ExperimentRegistry;
use roku_memory::{
	ApprovalRepository, ConservativeMemoryLifecyclePolicy, ControlPlaneDataPlane, DispatchQueue,
	EventRepository, InMemoryDispatchQueue, LongTermMemoryBackend, MemoryLifecyclePolicy,
	NoopLongTermMemoryBackend, ResultRepository, TaskRepository,
};
use roku_observability::{
	AuditCorrelation, AuditRecord, AuditSink, InMemoryAuditSink, LogLevel, LogRecord, Metrics,
	MetricsSnapshot, emit_global_log,
};
use roku_orchestrator::Orchestrator;
use roku_validation_plane::ValidationPipeline;

use crate::helpers::{approval_artifact, failure_message, ticket_status_label};
pub use crate::memory_context::{ContextBundle, RuntimeMemoryLayers};
pub use crate::pending_loop_snapshot_store::{
	InMemoryPendingLoopSnapshotStore, PendingLoopSnapshotStore,
};
use crate::runtime_loop_owner::RuntimeLoopOwner;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RunMode {
	#[default]
	Normal,
	MissingEvidence,
	CapabilityDenied,
	ApprovalRequired,
	RetryExhausted,
	TimeoutRecovery,
}

/// Identifies which runtime pipeline a service instance is expected to use.
///
/// ## Variants
/// - `Deterministic`: Uses the non-LLM fallback path with placeholder or rule-based behavior.
/// - `LiveReact`: Uses the live LLM-backed ReAct runtime path.
///
/// ## Non-Goals
/// - This enum does not describe task-level execution modes such as approval or retry recovery.
/// - This enum does not imply that a given request succeeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeExecutionMode {
	Deterministic,
	LiveReact,
}

impl RuntimeExecutionMode {
	pub fn as_str(self) -> &'static str {
		match self {
			Self::Deterministic => "deterministic",
			Self::LiveReact => "live-react",
		}
	}
}

/// Records the requested runtime path and the path that is actually active.
///
/// ## Why this exists
/// Runtime entry surfaces need an explicit source of truth for whether a
/// command is exercising the live ReAct runtime or a deterministic fallback.
/// Without this report, logs and CLI output can silently make deterministic
/// executions look like live runtime passes.
///
/// ## Invariants
/// - `effective` is the runtime path that will actually execute the request.
/// - `fallback_reason` is `Some` only when `requested != effective`.
/// - `requested == effective` means the runtime path was honored as configured.
///
/// ## Non-Goals
/// - This report is not user-facing business output.
/// - This report does not replace task-level status such as succeeded or failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeModeReport {
	pub requested: RuntimeExecutionMode,
	pub effective: RuntimeExecutionMode,
	pub fallback_reason: Option<String>,
}

impl RuntimeModeReport {
	pub fn deterministic() -> Self {
		Self {
			requested: RuntimeExecutionMode::Deterministic,
			effective: RuntimeExecutionMode::Deterministic,
			fallback_reason: None,
		}
	}

	pub fn live_react() -> Self {
		Self {
			requested: RuntimeExecutionMode::LiveReact,
			effective: RuntimeExecutionMode::LiveReact,
			fallback_reason: None,
		}
	}

	pub fn live_react_fallback_to_deterministic(fallback_reason: impl Into<String>) -> Self {
		Self {
			requested: RuntimeExecutionMode::LiveReact,
			effective: RuntimeExecutionMode::Deterministic,
			fallback_reason: Some(fallback_reason.into()),
		}
	}
}

struct RuntimeState {
	capability_auth: CapabilityAuthority,
	task_repo: Box<dyn TaskRepository + Send>,
	event_repo: Box<dyn EventRepository + Send>,
	approval_repo: Box<dyn ApprovalRepository + Send>,
	result_repo: Box<dyn ResultRepository + Send>,
	dispatch_queue: Box<dyn DispatchQueue + Send>,
	artifact_store: ArtifactStore,
	experiment_registry: ExperimentRegistry,
}

pub struct RuntimeDataPlane {
	pub control_plane: ControlPlaneDataPlane,
	pub artifact_store: ArtifactStore,
	pub experiment_registry: ExperimentRegistry,
}

pub struct RuntimeService {
	orchestrator: Orchestrator,
	runtime: GenericAgentRuntime,
	runtime_mode: RuntimeModeReport,
	validator: ValidationPipeline,
	metrics: Arc<Metrics>,
	audit_sink: Arc<dyn AuditSink>,
	memory_backend: Arc<dyn LongTermMemoryBackend>,
	memory_policy: Arc<dyn MemoryLifecyclePolicy>,
	state: Mutex<RuntimeState>,
	pending_loop_snapshot_store: Arc<dyn PendingLoopSnapshotStore>,
	runtime_memory_layers: Mutex<HashMap<String, RuntimeMemoryLayers>>,
}

impl RuntimeService {
	pub fn new(
		task_repo: Box<dyn TaskRepository + Send>,
		event_repo: Box<dyn EventRepository + Send>,
		approval_repo: Box<dyn ApprovalRepository + Send>,
		result_repo: Box<dyn ResultRepository + Send>,
		audit_sink: Arc<dyn AuditSink>,
	) -> Self {
		let runtime = GenericAgentRuntime::default();
		Self::new_with_runtime_data_plane_and_metrics(
			RuntimeDataPlane {
				control_plane: ControlPlaneDataPlane {
					task_repo,
					event_repo,
					approval_repo,
					result_repo,
					dispatch_queue: Box::new(InMemoryDispatchQueue::default()),
				},
				artifact_store: ArtifactStore::default(),
				experiment_registry: ExperimentRegistry::default(),
			},
			audit_sink,
			runtime,
			Arc::new(Metrics::default()),
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
		let runtime = GenericAgentRuntime::default();
		Self::new_with_runtime_data_plane_and_metrics(
			RuntimeDataPlane {
				control_plane: ControlPlaneDataPlane {
					task_repo,
					event_repo,
					approval_repo,
					result_repo,
					dispatch_queue: Box::new(InMemoryDispatchQueue::default()),
				},
				artifact_store,
				experiment_registry,
			},
			audit_sink,
			runtime,
			Arc::new(Metrics::default()),
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
		Self::new_with_runtime_data_plane_and_metrics(
			RuntimeDataPlane {
				control_plane: ControlPlaneDataPlane {
					task_repo,
					event_repo,
					approval_repo,
					result_repo,
					dispatch_queue: Box::new(InMemoryDispatchQueue::default()),
				},
				artifact_store,
				experiment_registry,
			},
			audit_sink,
			runtime,
			Arc::new(Metrics::default()),
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
	) -> Self {
		Self::new_with_runtime_data_plane_and_metrics(
			RuntimeDataPlane {
				control_plane: ControlPlaneDataPlane {
					task_repo,
					event_repo,
					approval_repo,
					result_repo,
					dispatch_queue: Box::new(InMemoryDispatchQueue::default()),
				},
				artifact_store,
				experiment_registry,
			},
			audit_sink,
			runtime,
			metrics,
		)
	}

	pub fn new_with_bundles_and_runtime_and_metrics(
		control_plane: ControlPlaneDataPlane,
		artifact_store: ArtifactStore,
		experiment_registry: ExperimentRegistry,
		audit_sink: Arc<dyn AuditSink>,
		runtime: GenericAgentRuntime,
		metrics: Arc<Metrics>,
	) -> Self {
		Self::new_with_runtime_data_plane_and_metrics(
			RuntimeDataPlane {
				control_plane,
				artifact_store,
				experiment_registry,
			},
			audit_sink,
			runtime,
			metrics,
		)
	}

	pub fn new_with_runtime_data_plane_and_metrics(
		data_plane: RuntimeDataPlane,
		audit_sink: Arc<dyn AuditSink>,
		runtime: GenericAgentRuntime,
		metrics: Arc<Metrics>,
	) -> Self {
		let RuntimeDataPlane {
			control_plane,
			artifact_store,
			experiment_registry,
		} = data_plane;
		let ControlPlaneDataPlane {
			task_repo,
			event_repo,
			approval_repo,
			result_repo,
			dispatch_queue,
		} = control_plane;

		Self {
			orchestrator: Orchestrator::default(),
			runtime,
			runtime_mode: RuntimeModeReport::deterministic(),
			validator: ValidationPipeline::default(),
			metrics,
			audit_sink,
			memory_backend: Arc::new(NoopLongTermMemoryBackend),
			memory_policy: Arc::new(ConservativeMemoryLifecyclePolicy::default()),
			state: Mutex::new(RuntimeState {
				capability_auth: CapabilityAuthority::default(),
				task_repo,
				event_repo,
				approval_repo,
				result_repo,
				dispatch_queue,
				artifact_store,
				experiment_registry,
			}),
			pending_loop_snapshot_store: Arc::new(InMemoryPendingLoopSnapshotStore::default()),
			runtime_memory_layers: Mutex::new(HashMap::new()),
		}
	}

	/// Overrides the runtime mode report attached to this service instance.
	///
	/// ## Why this exists
	/// CLI bootstrap code constructs deterministic and live service variants through
	/// different builders. The service itself must still expose the requested/effective
	/// runtime truth so logs and tests can verify which path is active.
	pub fn with_runtime_mode_report(mut self, runtime_mode: RuntimeModeReport) -> Self {
		self.runtime_mode = runtime_mode;
		self
	}

	pub fn with_pending_loop_snapshot_store(
		mut self,
		pending_loop_snapshot_store: Arc<dyn PendingLoopSnapshotStore>,
	) -> Self {
		self.pending_loop_snapshot_store = pending_loop_snapshot_store;
		self
	}

	pub fn runtime_mode_report(&self) -> RuntimeModeReport {
		self.runtime_mode.clone()
	}

	pub fn in_memory() -> Self {
		Self::in_memory_with_agent_runtime(GenericAgentRuntime::default())
	}

	pub fn in_memory_with_agent_runtime(runtime: GenericAgentRuntime) -> Self {
		Self::new_with_runtime_data_plane_and_metrics(
			RuntimeDataPlane {
				control_plane: ControlPlaneDataPlane::in_memory(),
				artifact_store: ArtifactStore::default(),
				experiment_registry: ExperimentRegistry::default(),
			},
			Arc::new(InMemoryAuditSink::default()),
			runtime,
			Arc::new(Metrics::default()),
		)
	}

	pub fn in_memory_with_agent_runtime_and_metrics(
		runtime: GenericAgentRuntime,
		metrics: Arc<Metrics>,
	) -> Self {
		Self::new_with_runtime_data_plane_and_metrics(
			RuntimeDataPlane {
				control_plane: ControlPlaneDataPlane::in_memory(),
				artifact_store: ArtifactStore::default(),
				experiment_registry: ExperimentRegistry::default(),
			},
			Arc::new(InMemoryAuditSink::default()),
			runtime,
			metrics,
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
		let normalized_request = normalize_request(&request);
		let runtime_mode = self.runtime_mode_report();
		log_runtime(
			LogLevel::Info,
			"received runtime request",
			[
				("request_id", normalized_request.request_id.0.clone()),
				("session_id", normalized_request.session_id.clone()),
				("run_mode", format!("{mode:?}")),
				(
					"requested_runtime_mode",
					runtime_mode.requested.as_str().to_string(),
				),
				(
					"effective_runtime_mode",
					runtime_mode.effective.as_str().to_string(),
				),
				("goal", truncate_for_log(&normalized_request.goal, 200)),
			],
		);
		if let Some(reason) = runtime_mode.fallback_reason.as_deref() {
			log_runtime(
				LogLevel::Warn,
				"live runtime request is executing in effective deterministic mode",
				[
					("request_id", normalized_request.request_id.0.clone()),
					("session_id", normalized_request.session_id.clone()),
					(
						"requested_runtime_mode",
						runtime_mode.requested.as_str().to_string(),
					),
					(
						"effective_runtime_mode",
						runtime_mode.effective.as_str().to_string(),
					),
					("fallback_reason", reason.to_string()),
				],
			);
		}
		let mut task = self.orchestrator.create_task(&normalized_request);

		self.record_transition(&mut task, TaskState::Planning, "classify direct route")?;

		RuntimeLoopOwner::new(self).execute_request(&mut task, &normalized_request)
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
		task = self.reconstruct_task_progress(&task)?;
		let execution_resume = if decision.approved {
			self.validated_execution_resume(&task, &ticket)?
		} else {
			None
		};

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
			if let Some(execution_resume) = execution_resume {
				return self.resume_approved_execution_ticket(&mut task, &ticket, execution_resume);
			}
			self.continue_legacy_graph_after_approval(&mut task, &ticket)
		} else {
			if let Some(graph_node) = task.graph.as_ref().and_then(|graph| {
				graph
					.nodes
					.iter()
					.find(|node| node.node_id == ticket.node_id)
			}) {
				self.append_node_event(
					&task,
					graph_node,
					TaskEventKind::ApprovalRejected,
					"approval rejected",
				)?;
			}
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
			pending_execution: None,
		};

		self.record_transition(task, TaskState::WaitingApproval, "approval required")?;
		task.pending_approval_id = Some(approval_id.clone());
		self.append_node_event(
			task,
			node,
			TaskEventKind::ApprovalPending,
			"approval required",
		)?;
		self.metrics.inc_approvals_created();
		let response_message = crate::execution::pending_approval_message(&ticket);
		self.save_approval_ticket(ticket)?;
		self.save_task(task.clone())?;

		Ok(ResponseEnvelope {
			request_id: task.request_id.clone(),
			status: ResponseStatus::PendingApproval,
			message: response_message,
			artifacts: vec![approval_artifact(&approval_id)],
		})
	}
}

impl Default for RuntimeService {
	fn default() -> Self {
		Self::in_memory()
	}
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

fn normalize_request(request: &RequestEnvelope) -> RequestEnvelope {
	let mut normalized = request.clone();
	normalized.goal = normalize_goal(&request.goal);
	normalized
}

fn normalize_goal(goal: &str) -> String {
	let normalized = goal.lines().map(str::trim).collect::<Vec<_>>().join("\n");
	if normalized.trim().is_empty() {
		goal.trim().to_string()
	} else {
		normalized
	}
}

fn compatibility_fallback_plan(reason: &str) -> RouteEscalationPlan {
	RouteEscalationPlan {
		decision: RouteDecision::new(
			IntentFamily::MultiStep,
			0.0,
			true,
			RouteRisk::Medium,
			Vec::new(),
			Vec::new(),
			Vec::new(),
			reason,
		),
		reason: EscalationReason::RequiresMultiStep,
		action: EscalationAction::EnterLimitedPlanning,
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

fn log_route_decision(request: &RequestEnvelope, route: &RouteDecisionResult) {
	let record = match route {
		RouteDecisionResult::Direct(plan) => LogRecord::new(
			"roku-runtime-service",
			LogLevel::Info,
			"selected direct route",
		)
		.with_field("request_id", request.request_id.0.clone())
		.with_field("session_id", request.session_id.clone())
		.with_field(
			"intent_family",
			format!("{:?}", plan.decision.intent_family),
		)
		.with_field("candidate_tools", plan.decision.candidate_tools.join(","))
		.with_field("reason", plan.decision.reason.clone()),
		RouteDecisionResult::Escalate(plan) => LogRecord::new(
			"roku-runtime-service",
			LogLevel::Info,
			"escalated route decision",
		)
		.with_field("request_id", request.request_id.0.clone())
		.with_field("session_id", request.session_id.clone())
		.with_field(
			"intent_family",
			format!("{:?}", plan.decision.intent_family),
		)
		.with_field("action", format!("{:?}", plan.action))
		.with_field("reason", plan.decision.reason.clone()),
	};
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
