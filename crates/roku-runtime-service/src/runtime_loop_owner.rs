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
	RequestEnvelope, ResponseEnvelope, RuntimeError, RuntimeMemorySections, Task,
};
use roku_observability::LogLevel;

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
		if let Some(response) = self.handle_planning_mode_hint(task, request)? {
			return Ok(response);
		}

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
		self.service
			.attach_visible_resources(&mut prepared.context_bundle, &route);
		log_route_decision(request, &route);
		let mut loop_state = self
			.service
			.initialize_runtime_loop_for_route(request, &route);
		self.dispatch_route(task, request, &route, &mut loop_state, &prepared)
	}

	fn handle_planning_mode_hint(
		&self,
		task: &mut Task,
		request: &RequestEnvelope,
	) -> Result<Option<ResponseEnvelope>, RuntimeError> {
		let Some(planning_mode_hint) = request.planning_mode_hint.as_ref() else {
			return Ok(None);
		};

		let prepared = self.prepare_request_context(task, request, false, None)?;
		self.service.clear_pending_loop(&request.session_id)?;
		self.service.metrics.inc_route_escalations();
		self.service.metrics.inc_route_limited_planning();
		log_runtime(
			LogLevel::Info,
			"planning mode hint resolved as compatibility fallback",
			[
				("request_id", request.request_id.0.clone()),
				("planning_mode_hint", format!("{planning_mode_hint:?}")),
			],
		);
		self.service
			.start_experiment_run(task, &request.goal, "compatibility_fallback")?;
		let compatibility_plan = compatibility_fallback_plan(
			"planning mode hints are deprecated compatibility signals; planning-heavy workflow is not enabled in this runtime",
		);
		let mut loop_state = self.service.initialize_runtime_loop_for_route(
			request,
			&RouteDecisionResult::Escalate(compatibility_plan.clone()),
		);
		let runtime_memory_sections = prepared.runtime_memory_sections();
		let response = self.service.process_direct_escalation(
			task,
			request,
			&compatibility_plan,
			&mut loop_state,
			&prepared.context_bundle,
			&runtime_memory_sections,
		)?;
		Ok(Some(response))
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
