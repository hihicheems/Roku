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

use roku_agent_runtime::{
	EscalationAction, LoopState, RouteDecisionResult, StepAction, StepRecord,
};
use roku_common_types::{RequestEnvelope, ResponseStatus};
use roku_observability::LogLevel;

use crate::RuntimeService;
use crate::{log_runtime, truncate_for_log};

impl RuntimeService {
	pub(super) fn initialize_runtime_loop_for_route(
		&self,
		request: &RequestEnvelope,
		route: &RouteDecisionResult,
	) -> LoopState {
		let decision = route_decision(route);
		let loop_state =
			self.runtime
				.initialize_runtime_loop(request, &request.session_id, decision);
		log_runtime(
			LogLevel::Info,
			"runtime loop initialized",
			[
				("request_id", loop_state.request_id.clone()),
				("session_id", loop_state.session_id.clone()),
				("run_id", loop_state.run_id.clone()),
				(
					"intent_family",
					format!("{:?}", loop_state.route_decision.intent_family),
				),
				(
					"working_directory",
					truncate_for_log(&loop_state.working_directory, 200),
				),
				(
					"visible_tools_count",
					loop_state.visible_tools.len().to_string(),
				),
			],
		);
		loop_state
	}

	pub(super) fn record_runtime_loop_terminal_step(
		&self,
		loop_state: &mut LoopState,
		response_status: ResponseStatus,
		message: &str,
	) -> StepRecord {
		let action = match response_status {
			ResponseStatus::Succeeded | ResponseStatus::PendingApproval => StepAction::FinalAnswer,
			ResponseStatus::Failed => StepAction::Fail,
		};
		self.record_runtime_loop_step_with_action(loop_state, action, response_status, message)
	}

	pub(super) fn record_runtime_loop_escalation_step(
		&self,
		loop_state: &mut LoopState,
		action: EscalationAction,
		response_status: ResponseStatus,
		message: &str,
	) -> StepRecord {
		let step_action = match action {
			EscalationAction::AskForMoreInfo => StepAction::AskUser,
			EscalationAction::FallbackAnswer | EscalationAction::EnterLimitedPlanning => {
				match response_status {
					ResponseStatus::Succeeded | ResponseStatus::PendingApproval => {
						StepAction::FinalAnswer
					}
					ResponseStatus::Failed => StepAction::Fail,
				}
			}
		};
		self.record_runtime_loop_step_with_action(loop_state, step_action, response_status, message)
	}

	fn record_runtime_loop_step_with_action(
		&self,
		loop_state: &mut LoopState,
		action: StepAction,
		response_status: ResponseStatus,
		message: &str,
	) -> StepRecord {
		let reason = match response_status {
			ResponseStatus::Succeeded => "phase1 loop bridge captured direct route completion",
			ResponseStatus::PendingApproval => {
				"phase1 loop bridge captured pending approval terminal state"
			}
			ResponseStatus::Failed => "phase1 loop bridge captured direct route failure",
		};
		let step = self.runtime.record_terminal_step(
			loop_state,
			action,
			reason,
			Some(message.to_string()),
		);
		log_runtime(
			LogLevel::Debug,
			"runtime loop step recorded",
			[
				("run_id", loop_state.run_id.clone()),
				("step_index", step.step_index.to_string()),
				("action", format!("{:?}", step.action)),
				(
					"tool_name",
					step.tool_name.clone().unwrap_or_else(|| "none".to_string()),
				),
				(
					"remaining_step_budget_after",
					step.remaining_step_budget_after.to_string(),
				),
				(
					"remaining_recovery_budget_after",
					step.remaining_recovery_budget_after.to_string(),
				),
			],
		);
		log_runtime(
			LogLevel::Info,
			"runtime loop terminated",
			[
				("run_id", loop_state.run_id.clone()),
				("status", format!("{:?}", loop_state.status)),
				("step_count", loop_state.history.len().to_string()),
			],
		);
		step
	}
}

fn route_decision(route: &RouteDecisionResult) -> &roku_agent_runtime::RouteDecision {
	match route {
		RouteDecisionResult::Direct(plan) => &plan.decision,
		RouteDecisionResult::Escalate(plan) => &plan.decision,
	}
}
