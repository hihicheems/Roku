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
mod legacy_graph;
mod runtime_loop_bridge;
mod runtime_loop_recovery;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use roku_agent_runtime::{
	EscalationAction, EscalationReason, GenericAgentRuntime, IntentFamily, LoopState,
	RouteDecision, RouteDecisionResult, RouteEscalationPlan, RouteRisk,
};
use roku_artifact_store::ArtifactStore;
use roku_capability_auth::CapabilityAuthority;
use roku_common_types::{
	ApprovalDecision, ApprovalId, ApprovalStatus, ApprovalTicket, ErrorClass, RequestEnvelope,
	ResponseEnvelope, ResponseStatus, RuntimeError, Task, TaskEventKind, TaskNode, TaskState,
};
use roku_experiment_registry::ExperimentRegistry;
use roku_observability::{
	AuditCorrelation, AuditRecord, AuditSink, InMemoryAuditSink, LogLevel, LogRecord, Metrics,
	MetricsSnapshot, emit_global_log,
};
use roku_orchestrator::Orchestrator;
use roku_state_store::{
	ApprovalRepository, EventRepository, InMemoryApprovalRepository, InMemoryDispatchQueue,
	InMemoryEventRepository, InMemoryResultRepository, InMemoryTaskRepository, ResultRepository,
	TaskRepository,
};
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
	TimeoutRecovery,
}

struct RuntimeState {
	capability_auth: CapabilityAuthority,
	task_repo: Box<dyn TaskRepository + Send>,
	event_repo: Box<dyn EventRepository + Send>,
	approval_repo: Box<dyn ApprovalRepository + Send>,
	result_repo: Box<dyn ResultRepository + Send>,
	dispatch_queue: Box<dyn roku_state_store::DispatchQueue + Send>,
	artifact_store: ArtifactStore,
	experiment_registry: ExperimentRegistry,
}

pub struct RuntimeDataPlane {
	pub task_repo: Box<dyn TaskRepository + Send>,
	pub event_repo: Box<dyn EventRepository + Send>,
	pub approval_repo: Box<dyn ApprovalRepository + Send>,
	pub result_repo: Box<dyn ResultRepository + Send>,
	pub dispatch_queue: Box<dyn roku_state_store::DispatchQueue + Send>,
	pub artifact_store: ArtifactStore,
	pub experiment_registry: ExperimentRegistry,
}

pub struct RuntimeService {
	orchestrator: Orchestrator,
	runtime: GenericAgentRuntime,
	validator: ValidationPipeline,
	metrics: Arc<Metrics>,
	audit_sink: Arc<dyn AuditSink>,
	state: Mutex<RuntimeState>,
	pending_loops: Mutex<HashMap<String, LoopState>>,
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
				task_repo,
				event_repo,
				approval_repo,
				result_repo,
				dispatch_queue: Box::new(InMemoryDispatchQueue::default()),
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
				task_repo,
				event_repo,
				approval_repo,
				result_repo,
				dispatch_queue: Box::new(InMemoryDispatchQueue::default()),
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
				task_repo,
				event_repo,
				approval_repo,
				result_repo,
				dispatch_queue: Box::new(InMemoryDispatchQueue::default()),
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
				task_repo,
				event_repo,
				approval_repo,
				result_repo,
				dispatch_queue: Box::new(InMemoryDispatchQueue::default()),
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
			task_repo,
			event_repo,
			approval_repo,
			result_repo,
			dispatch_queue,
			artifact_store,
			experiment_registry,
		} = data_plane;

		Self {
			orchestrator: Orchestrator::default(),
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
				dispatch_queue,
				artifact_store,
				experiment_registry,
			}),
			pending_loops: Mutex::new(HashMap::new()),
		}
	}

	pub fn in_memory() -> Self {
		Self::in_memory_with_agent_runtime(GenericAgentRuntime::default())
	}

	pub fn in_memory_with_agent_runtime(runtime: GenericAgentRuntime) -> Self {
		Self::new_with_runtime_data_plane_and_metrics(
			RuntimeDataPlane {
				task_repo: Box::new(InMemoryTaskRepository::default()),
				event_repo: Box::new(InMemoryEventRepository::default()),
				approval_repo: Box::new(InMemoryApprovalRepository::default()),
				result_repo: Box::new(InMemoryResultRepository::default()),
				dispatch_queue: Box::new(InMemoryDispatchQueue::default()),
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
				task_repo: Box::new(InMemoryTaskRepository::default()),
				event_repo: Box::new(InMemoryEventRepository::default()),
				approval_repo: Box::new(InMemoryApprovalRepository::default()),
				result_repo: Box::new(InMemoryResultRepository::default()),
				dispatch_queue: Box::new(InMemoryDispatchQueue::default()),
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
		log_runtime(
			LogLevel::Info,
			"received runtime request",
			[
				("request_id", normalized_request.request_id.0.clone()),
				("session_id", normalized_request.session_id.clone()),
				("mode", format!("{mode:?}")),
				("goal", truncate_for_log(&normalized_request.goal, 200)),
			],
		);
		let mut task = self.orchestrator.create_task(&normalized_request);

		self.record_transition(&mut task, TaskState::Planning, "classify direct route")?;

		if let Some(planning_mode_hint) = normalized_request.planning_mode_hint {
			self.clear_pending_loop(&normalized_request.session_id)?;
			self.metrics.inc_route_escalations();
			self.metrics.inc_route_limited_planning();
			log_runtime(
				LogLevel::Info,
				"planning mode hint resolved as compatibility fallback",
				[
					("request_id", normalized_request.request_id.0.clone()),
					("planning_mode_hint", format!("{planning_mode_hint:?}")),
				],
			);
			self.start_experiment_run(&task, &normalized_request.goal, "compatibility_fallback")?;
			let compatibility_plan = compatibility_fallback_plan(
				"planning mode hints are deprecated compatibility signals; planning-heavy workflow is not enabled in this runtime",
			);
			let mut loop_state = self.initialize_runtime_loop_for_route(
				&normalized_request,
				&RouteDecisionResult::Escalate(compatibility_plan.clone()),
			);
			return self.process_direct_escalation(
				&mut task,
				&normalized_request,
				&compatibility_plan,
				&mut loop_state,
			);
		}

		if let Some(mut loop_state) = self.take_resumable_pending_loop(&normalized_request)? {
			self.start_experiment_run(&task, &normalized_request.goal, "runtime_loop_resume")?;
			return self.resume_pending_loop(&mut task, &normalized_request, &mut loop_state);
		}

		let route = self
			.runtime
			.classify_route(&normalized_request, &normalized_request.session_id);
		log_route_decision(&normalized_request, &route);
		let mut loop_state = self.initialize_runtime_loop_for_route(&normalized_request, &route);
		match &route {
			RouteDecisionResult::Direct(plan) => {
				self.metrics.inc_direct_route_hits();
				self.start_experiment_run(&task, &normalized_request.goal, "direct_route")?;
				self.process_direct_route(&mut task, &normalized_request, plan, &mut loop_state)
			}
			RouteDecisionResult::Escalate(plan) => {
				self.metrics.inc_route_escalations();
				match plan.reason {
					EscalationReason::RouteClassifierFailure => {
						self.metrics.inc_route_classifier_failures();
					}
					EscalationReason::RouteParseGuardFailure => {
						self.metrics.inc_route_parse_guard_failures();
					}
					EscalationReason::MissingArguments
					| EscalationReason::RequiresMultiStep
					| EscalationReason::NoEnabledRouteTarget
					| EscalationReason::RouteModelUnavailable
					| EscalationReason::LowConfidence => {}
				}
				match plan.action {
					EscalationAction::AskForMoreInfo => {
						self.start_experiment_run(&task, &normalized_request.goal, "direct_route")?;
						self.process_direct_escalation(
							&mut task,
							&normalized_request,
							plan,
							&mut loop_state,
						)
					}
					EscalationAction::FallbackAnswer => {
						self.metrics.inc_direct_route_fallbacks();
						self.start_experiment_run(&task, &normalized_request.goal, "direct_route")?;
						self.process_direct_escalation(
							&mut task,
							&normalized_request,
							plan,
							&mut loop_state,
						)
					}
					EscalationAction::EnterLimitedPlanning => {
						self.metrics.inc_route_limited_planning();
						self.metrics.inc_direct_route_fallbacks();
						self.start_experiment_run(
							&task,
							&normalized_request.goal,
							"compatibility_fallback",
						)?;
						self.process_direct_escalation(
							&mut task,
							&normalized_request,
							plan,
							&mut loop_state,
						)
					}
				}
			}
		}
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
			if let Some(graph_node) = task.graph.as_ref().and_then(|graph| {
				graph
					.nodes
					.iter()
					.find(|node| node.node_id == ticket.node_id)
			}) {
				self.append_node_event(
					&task,
					graph_node,
					TaskEventKind::ApprovalApproved,
					"approval granted",
				)?;
			}
			self.record_transition(&mut task, TaskState::Executing, "approval granted")?;
			self.process_task(&mut task, RunMode::Normal)
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
