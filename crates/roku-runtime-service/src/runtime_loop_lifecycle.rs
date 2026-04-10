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
	AskUserPayload, EscalationAction, LoopState, RouteDecisionResult, StepAction, StepRecord,
};
use roku_common_types::LogLevel;
use roku_common_types::{RequestEnvelope, ResponseStatus};

use crate::RuntimeService;
use crate::{log_runtime, truncate_for_log};

impl RuntimeService {
	pub(super) fn record_runtime_loop_history(&self, loop_state: &LoopState, start_index: usize) {
		for step in loop_state.history.iter().skip(start_index) {
			let mut fields = vec![
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
			];
			if let Some(raw_tool_output) = step.raw_tool_output.as_ref() {
				fields.push((
					"raw_tool_output",
					truncate_for_log(
						&serde_json::to_string(raw_tool_output)
							.unwrap_or_else(|_| "<unserializable-json>".to_string()),
						200,
					),
				));
			}
			if let Some(interpreted) = step.interpreted_observation.as_ref() {
				fields.push((
					"interpreted_continue_allowed",
					interpreted.continue_allowed.to_string(),
				));
				fields.push((
					"interpreted_should_ask_user",
					interpreted.should_ask_user.to_string(),
				));
				fields.push((
					"interpreted_should_emit_final_answer",
					interpreted.should_emit_final_answer.to_string(),
				));
				fields.push((
					"interpreted_should_fail",
					interpreted.should_fail.to_string(),
				));
			}
			log_runtime(LogLevel::Debug, "runtime loop step recorded", fields);
		}
	}

	pub(super) fn initialize_runtime_loop_for_route(
		&self,
		request: &RequestEnvelope,
		route: &RouteDecisionResult,
	) -> LoopState {
		let decision = route_decision(route);
		let bound_resources = route_bound_resources(route);
		let loop_state = self.runtime.initialize_runtime_loop(
			request,
			&request.session_id,
			decision,
			bound_resources,
		);
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
		self.record_runtime_loop_terminal_step_with_action(
			loop_state,
			action,
			response_status,
			message,
		)
	}

	pub(super) fn record_runtime_loop_terminal_step_with_action(
		&self,
		loop_state: &mut LoopState,
		action: StepAction,
		response_status: ResponseStatus,
		message: &str,
	) -> StepRecord {
		if let Some(existing) = self.existing_terminal_step(loop_state, action) {
			self.log_runtime_loop_terminated(loop_state);
			return existing;
		}
		self.record_runtime_loop_step_with_action(loop_state, action, response_status, message)
	}

	pub(super) fn record_runtime_loop_stop_step(
		&self,
		loop_state: &mut LoopState,
		message: &str,
	) -> StepRecord {
		let step = self.runtime.record_terminal_step(
			loop_state,
			StepAction::Stop,
			"runtime loop discarded a stale pending pause before accepting a fresh intake",
			Some(message.to_string()),
		);
		self.record_runtime_loop_history(loop_state, loop_state.history.len().saturating_sub(1));
		self.log_runtime_loop_terminated(loop_state);
		step
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

	pub(super) fn record_runtime_loop_ask_user_payload_step(
		&self,
		loop_state: &mut LoopState,
		response_status: ResponseStatus,
		payload: AskUserPayload,
	) -> StepRecord {
		let reason = match response_status {
			ResponseStatus::Succeeded => "runtime loop captured direct route completion",
			ResponseStatus::PendingApproval => {
				"runtime loop captured pending approval terminal state"
			}
			ResponseStatus::Failed => "runtime loop captured direct route failure",
		};
		let step = self
			.runtime
			.record_ask_user_step(loop_state, reason, payload);
		self.record_runtime_loop_history(loop_state, loop_state.history.len().saturating_sub(1));
		self.log_runtime_loop_terminated(loop_state);
		step
	}

	fn record_runtime_loop_step_with_action(
		&self,
		loop_state: &mut LoopState,
		action: StepAction,
		response_status: ResponseStatus,
		message: &str,
	) -> StepRecord {
		let reason = match response_status {
			ResponseStatus::Succeeded => "runtime loop captured direct route completion",
			ResponseStatus::PendingApproval => {
				"runtime loop captured pending approval terminal state"
			}
			ResponseStatus::Failed => "runtime loop captured direct route failure",
		};
		let step = if action == StepAction::AskUser {
			self.runtime.record_ask_user_step(
				loop_state,
				reason,
				AskUserPayload::freeform(message.to_string()),
			)
		} else {
			self.runtime
				.record_terminal_step(loop_state, action, reason, Some(message.to_string()))
		};
		self.record_runtime_loop_history(loop_state, loop_state.history.len().saturating_sub(1));
		self.log_runtime_loop_terminated(loop_state);
		step
	}

	fn existing_terminal_step(
		&self,
		loop_state: &LoopState,
		action: StepAction,
	) -> Option<StepRecord> {
		let terminal_status_matches = match action {
			StepAction::AskUser => {
				loop_state.status == roku_agent_runtime::LoopStatus::AwaitingUser
			}
			StepAction::FinalAnswer => {
				loop_state.status == roku_agent_runtime::LoopStatus::Succeeded
			}
			StepAction::Fail => loop_state.status == roku_agent_runtime::LoopStatus::Failed,
			StepAction::Stop => loop_state.status == roku_agent_runtime::LoopStatus::Stopped,
			StepAction::CallTool | StepAction::CompactBoundary => {
				loop_state.status == roku_agent_runtime::LoopStatus::LoopRunning
			}
		};
		terminal_status_matches
			.then(|| loop_state.history.last().cloned())
			.flatten()
			.filter(|step| step.action == action)
	}

	fn log_runtime_loop_terminated(&self, loop_state: &LoopState) {
		log_runtime(
			LogLevel::Info,
			"runtime loop terminated",
			[
				("run_id", loop_state.run_id.clone()),
				("status", format!("{:?}", loop_state.status)),
				("step_count", loop_state.history.len().to_string()),
			],
		);
	}
}

fn route_decision(route: &RouteDecisionResult) -> &roku_agent_runtime::RouteDecision {
	match route {
		RouteDecisionResult::Direct(plan) => &plan.decision,
		RouteDecisionResult::Escalate(plan) => &plan.decision,
	}
}

fn route_bound_resources(route: &RouteDecisionResult) -> Vec<roku_common_types::ResourceSelector> {
	match route {
		RouteDecisionResult::Direct(plan) => plan.bound_resources.clone(),
		RouteDecisionResult::Escalate(_) => Vec::new(),
	}
}

#[cfg(test)]
mod tests {
	use roku_agent_runtime::{
		AskUserPayload, DirectRoutePlan, IntentFamily, RouteDecision, RouteRisk, StepAction,
		runtime_loop_trace,
	};
	use roku_common_types::{RequestEnvelope, RequestId, ResourceSelector, ResponseStatus};

	use super::*;

	fn direct_route_request(goal: &str) -> RequestEnvelope {
		RequestEnvelope {
			request_id: RequestId("req-direct-route-terminal-trace".to_string()),
			session_id: "session-direct-route-terminal-trace".to_string(),
			goal: goal.to_string(),
			planning_mode_hint: None,
			conversation_history: Vec::new(),
		}
	}

	fn direct_route() -> RouteDecisionResult {
		RouteDecisionResult::Direct(DirectRoutePlan {
			decision: RouteDecision::new(
				IntentFamily::CodeExec,
				0.95,
				false,
				RouteRisk::Low,
				vec!["command.run".to_string()],
				vec!["core-command".to_string()],
				Vec::new(),
				"direct route terminal trace test",
			),
			bound_resources: vec![ResourceSelector::tool("command.run".to_string())],
		})
	}

	fn direct_route_loop_state(service: &RuntimeService, goal: &str) -> LoopState {
		let request = direct_route_request(goal);
		service.initialize_runtime_loop_for_route(&request, &direct_route())
	}

	#[test]
	fn direct_route_pending_approval_terminal_trace_is_explicit() {
		let service = RuntimeService::default();
		let mut loop_state =
			direct_route_loop_state(&service, "Run pwd after approval so I can inspect the cwd.");
		let approval_message = "🛡️ Approval Request\n\nTool: command.run\nAction: Run command pwd from /workspace\nRisk: medium\nReason: the command is outside the constrained built-in allowlist";

		service.record_runtime_loop_terminal_step_with_action(
			&mut loop_state,
			StepAction::Fail,
			ResponseStatus::PendingApproval,
			approval_message,
		);

		let trace = runtime_loop_trace(&loop_state);
		let step = &trace.steps[0];

		assert_eq!(trace.step_count, 1);
		assert_eq!(step.decision.action, "fail");
		assert_eq!(
			step.visible_resources_before,
			Some(vec![ResourceSelector::tool("command.run".to_string())])
		);
		assert_eq!(
			step.decision.reason,
			"runtime loop captured pending approval terminal state"
		);
		assert_eq!(
			step.decision.final_message.as_deref(),
			Some(approval_message)
		);
		assert_eq!(trace.final_outcome.status, "failed");
		assert_eq!(trace.final_outcome.terminal_action.as_deref(), Some("fail"));
		assert_eq!(
			trace.final_outcome.final_message.as_deref(),
			Some(approval_message)
		);
	}

	#[test]
	fn direct_route_awaiting_user_trace_is_explicit() {
		let service = RuntimeService::default();
		let mut loop_state = direct_route_loop_state(
			&service,
			"Clarify which task to continue before resuming execution.",
		);
		let payload = AskUserPayload::freeform("您想继续什么任务？");
		let pause_message = payload.final_message.clone();

		service.record_runtime_loop_ask_user_payload_step(
			&mut loop_state,
			ResponseStatus::Succeeded,
			payload.clone(),
		);

		let trace = runtime_loop_trace(&loop_state);
		let step = &trace.steps[0];

		assert_eq!(trace.status, "awaiting_user");
		assert_eq!(trace.step_count, 1);
		assert_eq!(step.decision.action, "ask_user");
		assert_eq!(
			step.visible_resources_before,
			Some(vec![ResourceSelector::tool("command.run".to_string())])
		);
		assert_eq!(
			step.decision.reason,
			"runtime loop captured direct route completion"
		);
		assert_eq!(
			step.decision.final_message.as_deref(),
			Some(pause_message.as_str())
		);
		assert_eq!(trace.final_outcome.status, "awaiting_user");
		assert_eq!(
			trace.final_outcome.terminal_action.as_deref(),
			Some("ask_user")
		);
		assert_eq!(
			trace.final_outcome.final_message.as_deref(),
			Some(pause_message.as_str())
		);
		assert_eq!(loop_state.awaiting_user.as_ref(), Some(&payload));
	}

	#[test]
	fn direct_route_completion_terminal_trace_is_explicit() {
		let service = RuntimeService::default();
		let mut loop_state = direct_route_loop_state(
			&service,
			"Run pwd and report the current working directory when the route succeeds.",
		);
		let completion_message = "The current working directory is /workspace.";

		service.record_runtime_loop_terminal_step_with_action(
			&mut loop_state,
			StepAction::FinalAnswer,
			ResponseStatus::Succeeded,
			completion_message,
		);

		let trace = runtime_loop_trace(&loop_state);
		let step = &trace.steps[0];

		assert_eq!(trace.step_count, 1);
		assert_eq!(step.decision.action, "final_answer");
		assert_eq!(
			step.visible_resources_before,
			Some(vec![ResourceSelector::tool("command.run".to_string())])
		);
		assert_eq!(
			step.decision.reason,
			"runtime loop captured direct route completion"
		);
		assert_eq!(
			step.decision.final_message.as_deref(),
			Some(completion_message)
		);
		assert_eq!(trace.final_outcome.status, "succeeded");
		assert_eq!(
			trace.final_outcome.terminal_action.as_deref(),
			Some("final_answer")
		);
		assert_eq!(
			trace.final_outcome.final_message.as_deref(),
			Some(completion_message)
		);
	}
}
