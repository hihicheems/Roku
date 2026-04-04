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

use roku_agent_runtime::{EscalationAction, EscalationReason, RouteDecisionResult};
use roku_common_types::{
	PlanningModeHint, RequestEnvelope, ResponseEnvelope, RuntimeError, RuntimeMemorySections, Task,
};
use roku_observability::LogLevel;
use serde_json::{Value, json};

use crate::{
	ContextBundle, RuntimeMemoryLayers, RuntimeService, compatibility_fallback_plan,
	log_route_decision, log_runtime,
};

pub(super) struct RuntimeLoopOwner<'a> {
	service: &'a RuntimeService,
}

struct PreparedRuntimeLoopRequest {
	context_bundle: ContextBundle,
	runtime_memory_layers: RuntimeMemoryLayers,
}

impl PreparedRuntimeLoopRequest {
	fn runtime_memory_sections(&self) -> RuntimeMemorySections {
		self.runtime_memory_layers.structured_sections()
	}
}

impl<'a> RuntimeLoopOwner<'a> {
	pub(super) fn new(service: &'a RuntimeService) -> Self {
		Self { service }
	}

	pub(super) fn execute_request(
		&self,
		task: &mut Task,
		request: &RequestEnvelope,
	) -> Result<ResponseEnvelope, RuntimeError> {
		let mut resumable_loop = self.service.take_resumable_pending_loop(request)?;
		let mut prepared = self.prepare_request_context(
			task,
			request,
			resumable_loop.is_some(),
			resumable_loop
				.as_ref()
				.map(|loop_state| loop_state.working_summary.as_str()),
		)?;

		if let Some(mut loop_state) = resumable_loop.take() {
			self.service
				.attach_resumed_loop_resources(&mut prepared.context_bundle, &loop_state);
			self.service
				.start_experiment_run(task, &request.goal, "runtime_loop_resume")?;
			let runtime_memory_sections = prepared.runtime_memory_sections();
			return self.service.resume_pending_loop(
				task,
				request,
				&mut loop_state,
				&prepared.context_bundle,
				&runtime_memory_sections,
			);
		}

		let route = self
			.service
			.runtime
			.classify_route(request, &request.session_id);
		log_route_decision(request, &route);
		if let Some(planning_mode_hint) = request.planning_mode_hint.as_ref() {
			return self.handle_planning_mode_hint(
				task,
				request,
				planning_mode_hint,
				&route,
				&prepared,
			);
		}
		self.service
			.attach_visible_resources(&mut prepared.context_bundle, &route);
		let mut loop_state = self
			.service
			.initialize_runtime_loop_for_route(request, &route);
		self.dispatch_route(task, request, &route, &mut loop_state, &prepared)
	}

	fn handle_planning_mode_hint(
		&self,
		task: &mut Task,
		request: &RequestEnvelope,
		planning_mode_hint: &PlanningModeHint,
		replacement_route: &RouteDecisionResult,
		prepared: &PreparedRuntimeLoopRequest,
	) -> Result<ResponseEnvelope, RuntimeError> {
		self.service.metrics.inc_route_escalations();
		self.service.metrics.inc_route_limited_planning();
		log_runtime(
			LogLevel::Info,
			"planning mode hint resolved as compatibility-only shell",
			[
				("request_id", request.request_id.0.clone()),
				("planning_mode_hint", format!("{planning_mode_hint:?}")),
				(
					"replacement_route_kind",
					replacement_route_kind(replacement_route).to_string(),
				),
			],
		);
		self.service
			.start_experiment_run(task, &request.goal, "compatibility_fallback")?;
		let compatibility_plan = compatibility_fallback_plan(
			"planning mode hints are deprecated compatibility signals; planning-heavy workflow is not enabled in this runtime",
		);
		let mut loop_state = self
			.service
			.initialize_runtime_loop_for_route(request, replacement_route);
		let runtime_memory_sections = prepared.runtime_memory_sections();
		let response = self.service.process_direct_escalation(
			task,
			request,
			&compatibility_plan,
			&mut loop_state,
			&prepared.context_bundle,
			&runtime_memory_sections,
		)?;
		self.persist_planning_mode_shell_readiness(task, planning_mode_hint, replacement_route)?;
		Ok(response)
	}

	fn persist_planning_mode_shell_readiness(
		&self,
		task: &mut Task,
		planning_mode_hint: &PlanningModeHint,
		replacement_route: &RouteDecisionResult,
	) -> Result<(), RuntimeError> {
		let Some(last_result) = task.last_result.as_mut() else {
			return Ok(());
		};

		let original_payload = last_result.payload.clone();
		let mut payload = serde_json::from_str::<Value>(&original_payload).unwrap_or_else(|_| {
			json!({
				"message": original_payload.clone(),
			})
		});
		if !payload.is_object() {
			payload = json!({
				"message": payload,
			});
		}
		payload["planning_mode_compatibility_shell"] = planning_mode_shell_readiness_payload(
			planning_mode_hint,
			replacement_route,
			last_result.producer.as_str(),
		);
		last_result.payload = serde_json::to_string(&payload).unwrap_or(original_payload);

		self.service.save_result(last_result.clone())?;
		self.service.save_task(task.clone())?;
		Ok(())
	}

	fn prepare_request_context(
		&self,
		task: &Task,
		request: &RequestEnvelope,
		pending_loop_active: bool,
		working_memory: Option<&str>,
	) -> Result<PreparedRuntimeLoopRequest, RuntimeError> {
		let context_bundle = self
			.service
			.build_context_bundle(request, pending_loop_active)?;
		let runtime_memory_layers = context_bundle
			.runtime_memory_layers_with_working_memory(working_memory.unwrap_or_default());
		self.service
			.cache_runtime_memory_layers(&task.task_id, &runtime_memory_layers);
		Ok(PreparedRuntimeLoopRequest {
			context_bundle,
			runtime_memory_layers,
		})
	}

	fn dispatch_route(
		&self,
		task: &mut Task,
		request: &RequestEnvelope,
		route: &RouteDecisionResult,
		loop_state: &mut roku_agent_runtime::LoopState,
		prepared: &PreparedRuntimeLoopRequest,
	) -> Result<ResponseEnvelope, RuntimeError> {
		let runtime_memory_sections = prepared.runtime_memory_sections();
		match route {
			RouteDecisionResult::Direct(plan) => {
				self.service.metrics.inc_direct_route_hits();
				self.service
					.start_experiment_run(task, &request.goal, "direct_route")?;
				self.service.process_direct_route(
					task,
					request,
					plan,
					loop_state,
					&prepared.context_bundle,
					&runtime_memory_sections,
				)
			}
			RouteDecisionResult::Escalate(plan) => {
				self.service.metrics.inc_route_escalations();
				match plan.reason {
					EscalationReason::RouteClassifierFailure => {
						self.service.metrics.inc_route_classifier_failures();
					}
					EscalationReason::RouteParseGuardFailure => {
						self.service.metrics.inc_route_parse_guard_failures();
					}
					EscalationReason::MissingArguments
					| EscalationReason::RequiresMultiStep
					| EscalationReason::NoEnabledRouteTarget
					| EscalationReason::RouteModelUnavailable
					| EscalationReason::LowConfidence => {}
				}
				match plan.action {
					EscalationAction::AskForMoreInfo => {
						self.service
							.start_experiment_run(task, &request.goal, "direct_route")?;
						self.service.process_direct_escalation(
							task,
							request,
							plan,
							loop_state,
							&prepared.context_bundle,
							&runtime_memory_sections,
						)
					}
					EscalationAction::FallbackAnswer => {
						self.service.metrics.inc_direct_route_fallbacks();
						self.service
							.start_experiment_run(task, &request.goal, "direct_route")?;
						self.service.process_direct_escalation(
							task,
							request,
							plan,
							loop_state,
							&prepared.context_bundle,
							&runtime_memory_sections,
						)
					}
					EscalationAction::EnterLimitedPlanning => {
						self.service.metrics.inc_route_limited_planning();
						self.service.metrics.inc_direct_route_fallbacks();
						self.service.start_experiment_run(
							task,
							&request.goal,
							"compatibility_fallback",
						)?;
						self.service.process_direct_escalation(
							task,
							request,
							plan,
							loop_state,
							&prepared.context_bundle,
							&runtime_memory_sections,
						)
					}
				}
			}
		}
	}
}

fn planning_mode_shell_readiness_payload(
	planning_mode_hint: &PlanningModeHint,
	replacement_route: &RouteDecisionResult,
	result_producer: &str,
) -> Value {
	json!({
		"branch": format!("planning_mode_hint:{planning_mode_hint:?}"),
		"compatibility_only": true,
		"replacement_path": replacement_path_payload(replacement_route),
		"route_markers": {
			"experiment_strategy": "compatibility_fallback",
			"replacement_route_kind": replacement_route_kind(replacement_route),
			"result_producer": result_producer,
		},
		"remaining_blockers": [
			"Transport or session callers can still send PlanningModeHint::TreeSearch; remove those emitters before deleting this compatibility shell."
		],
	})
}

fn replacement_path_payload(replacement_route: &RouteDecisionResult) -> Value {
	match replacement_route {
		RouteDecisionResult::Direct(plan) => json!({
			"authority_path": "runtime_loop_owner.classify_route -> dispatch_route",
			"route_kind": "direct",
			"strategy": "direct_route",
			"decision": &plan.decision,
		}),
		RouteDecisionResult::Escalate(plan) => json!({
			"authority_path": "runtime_loop_owner.classify_route -> dispatch_route",
			"route_kind": "escalate",
			"strategy": "direct_escalation",
			"decision": &plan.decision,
			"escalation_action": format!("{:?}", plan.action),
			"escalation_reason": format!("{:?}", plan.reason),
		}),
	}
}

fn replacement_route_kind(replacement_route: &RouteDecisionResult) -> &'static str {
	match replacement_route {
		RouteDecisionResult::Direct(_) => "direct",
		RouteDecisionResult::Escalate(_) => "escalate",
	}
}
