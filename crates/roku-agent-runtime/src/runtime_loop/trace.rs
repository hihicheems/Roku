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

use roku_common_types::{
	RuntimeLoopTrace, RuntimeLoopTraceDecision, RuntimeLoopTraceOutcome, RuntimeLoopTraceStep,
};
use serde::{Deserialize, Serialize};

use crate::runtime_loop::{LoopState, LoopStatus, NextStepAction, StepAction, StepObservation};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeLoopTraceCheckReport {
	pub schema_version: String,
	pub step_count_matches: bool,
	pub decisions_captured: bool,
	pub visible_tools_captured: bool,
	pub tool_steps_capture_raw_output: bool,
	pub tool_steps_capture_normalized_observation: bool,
	pub tool_steps_capture_interpreted_observation: bool,
	pub terminal_outcome_captured: bool,
	pub final_outcome_matches_last_step: bool,
	pub issues: Vec<String>,
}

pub fn runtime_loop_trace(loop_state: &LoopState) -> RuntimeLoopTrace {
	RuntimeLoopTrace {
		schema_version: RuntimeLoopTrace::schema_version().to_string(),
		run_id: loop_state.run_id.clone(),
		status: loop_status_label(loop_state.status).to_string(),
		step_count: loop_state.history.len(),
		steps: loop_state
			.history
			.iter()
			.map(|step| RuntimeLoopTraceStep {
				step_index: step.step_index,
				decision: RuntimeLoopTraceDecision {
					action: next_step_action_label(step.decision.action).to_string(),
					tool_name: step.decision.tool_name.clone(),
					arguments: step.decision.arguments.clone(),
					reason: step.decision.reason.clone(),
					final_message: step.decision.final_message.clone(),
				},
				visible_tools_before: step.visible_tools_before.clone(),
				started_at: step.started_at.clone(),
				finished_at: step.finished_at.clone(),
				tool_latency_ms: step.tool_latency_ms,
				raw_tool_output: step.raw_tool_output.clone(),
				observation: step
					.observation
					.as_ref()
					.map(|value| serde_json::to_value(value).unwrap_or(serde_json::Value::Null)),
				interpreted_observation: step
					.interpreted_observation
					.as_ref()
					.map(|value| serde_json::to_value(value).unwrap_or(serde_json::Value::Null)),
				remaining_step_budget_after: step.remaining_step_budget_after,
				remaining_recovery_budget_after: step.remaining_recovery_budget_after,
				working_directory_after: step.working_directory_after.clone(),
			})
			.collect(),
		final_outcome: RuntimeLoopTraceOutcome {
			status: loop_status_label(loop_state.status).to_string(),
			terminal_action: loop_state.history.last().and_then(|step| {
				matches!(
					step.action,
					StepAction::AskUser | StepAction::FinalAnswer | StepAction::Fail
				)
				.then(|| step_action_label(step.action).to_string())
			}),
			final_message: loop_state.history.last().and_then(step_final_message),
		},
	}
}

pub fn check_runtime_loop_trace(trace: &RuntimeLoopTrace) -> RuntimeLoopTraceCheckReport {
	let step_count_matches = trace.step_count == trace.steps.len();
	let decisions_captured = trace.steps.iter().all(|step| {
		let action_known = matches!(
			step.decision.action.as_str(),
			"call_tool" | "ask_user" | "final_answer" | "fail"
		);
		let reason_present = !step.decision.reason.trim().is_empty();
		let call_tool_has_name =
			step.decision.action != "call_tool" || step.decision.tool_name.is_some();
		action_known && reason_present && call_tool_has_name
	});
	let visible_tools_captured = trace
		.steps
		.iter()
		.all(|step| !step.visible_tools_before.is_empty());
	let tool_steps = trace
		.steps
		.iter()
		.filter(|step| step.decision.action == "call_tool")
		.collect::<Vec<_>>();
	let tool_steps_capture_raw_output =
		tool_steps.iter().all(|step| step.raw_tool_output.is_some());
	let tool_steps_capture_normalized_observation =
		tool_steps.iter().all(|step| step.observation.is_some());
	let tool_steps_capture_interpreted_observation = tool_steps
		.iter()
		.all(|step| step.interpreted_observation.is_some());
	let terminal_outcome_captured = trace.final_outcome.terminal_action.is_some();
	let final_outcome_matches_last_step = trace.steps.last().is_some_and(|last_step| {
		trace
			.final_outcome
			.terminal_action
			.as_deref()
			.is_some_and(|action| action == last_step.decision.action)
	});

	let mut issues = Vec::new();
	if trace.schema_version != RuntimeLoopTrace::schema_version() {
		issues.push(format!(
			"unexpected trace schema version `{}`",
			trace.schema_version
		));
	}
	if !step_count_matches {
		issues.push(format!(
			"trace step_count={} does not match steps.len()={}",
			trace.step_count,
			trace.steps.len()
		));
	}
	if !decisions_captured {
		issues.push("trace steps are missing a valid NextStepDecision".to_string());
	}
	if !visible_tools_captured {
		issues.push("trace steps did not capture visible tools for the decision round".to_string());
	}
	if !tool_steps_capture_raw_output {
		issues.push("at least one tool step is missing raw tool output".to_string());
	}
	if !tool_steps_capture_normalized_observation {
		issues.push("at least one tool step is missing a normalized observation".to_string());
	}
	if !tool_steps_capture_interpreted_observation {
		issues.push("at least one tool step is missing an interpreted observation".to_string());
	}
	if !terminal_outcome_captured {
		issues.push("trace did not capture a terminal outcome".to_string());
	}
	if terminal_outcome_captured && !final_outcome_matches_last_step {
		issues.push(
			"trace terminal outcome does not match the final recorded step decision".to_string(),
		);
	}

	RuntimeLoopTraceCheckReport {
		schema_version: trace.schema_version.clone(),
		step_count_matches,
		decisions_captured,
		visible_tools_captured,
		tool_steps_capture_raw_output,
		tool_steps_capture_normalized_observation,
		tool_steps_capture_interpreted_observation,
		terminal_outcome_captured,
		final_outcome_matches_last_step,
		issues,
	}
}

fn step_final_message(step: &crate::runtime_loop::StepRecord) -> Option<String> {
	match step.observation.as_ref() {
		Some(StepObservation::AskUser { final_message })
		| Some(StepObservation::FinalMessage { final_message }) => Some(final_message.clone()),
		Some(StepObservation::Tool(observation)) => Some(observation.message.clone()),
		None => step.decision.final_message.clone(),
	}
}

fn loop_status_label(status: LoopStatus) -> &'static str {
	match status {
		LoopStatus::Received => "received",
		LoopStatus::Classified => "classified",
		LoopStatus::LoopRunning => "loop_running",
		LoopStatus::AwaitingUser => "awaiting_user",
		LoopStatus::Succeeded => "succeeded",
		LoopStatus::Failed => "failed",
		LoopStatus::Stopped => "stopped",
	}
}

fn step_action_label(action: StepAction) -> &'static str {
	match action {
		StepAction::CallTool => "call_tool",
		StepAction::AskUser => "ask_user",
		StepAction::FinalAnswer => "final_answer",
		StepAction::Fail => "fail",
	}
}

fn next_step_action_label(action: NextStepAction) -> &'static str {
	match action {
		NextStepAction::CallTool => "call_tool",
		NextStepAction::AskUser => "ask_user",
		NextStepAction::FinalAnswer => "final_answer",
		NextStepAction::Fail => "fail",
	}
}

#[cfg(test)]
mod tests {
	use serde_json::json;

	use super::{check_runtime_loop_trace, runtime_loop_trace};
	use crate::router::{IntentFamily, RouteDecision, RouteRisk};
	use crate::runtime_loop::{
		InterpretedObservation, LoopContext, LoopState, NextStepAction, NextStepDecision,
		StepObservation, StepRecord, ToolObservation,
	};
	use roku_common_types::ResourceSelector;

	fn loop_state() -> LoopState {
		let context = LoopContext {
			request_id: "req-trace".to_string(),
			session_id: "session-trace".to_string(),
			goal: "Run `pwd`".to_string(),
			workspace_root: "/workspace".to_string(),
			working_directory: "/workspace".to_string(),
			visible_tools: vec!["command.run".to_string(), "general.execute".to_string()],
			bound_resources: vec![ResourceSelector::tool("command.run".to_string())],
			route_decision: RouteDecision::new(
				IntentFamily::CodeExec,
				0.95,
				false,
				RouteRisk::Medium,
				vec!["command.run".to_string()],
				Vec::new(),
				Vec::new(),
				"explicit command",
			),
			last_observation: None,
		};
		LoopState::new("loop-trace", &context)
	}

	#[test]
	fn runtime_loop_trace_captures_decision_and_visible_tools() {
		let mut state = loop_state();
		let observation = ToolObservation {
			ok: true,
			tool_name: "command.run".to_string(),
			error_type: None,
			terminal: false,
			data: json!({"stdout": "/workspace\n"}),
			message: "/workspace".to_string(),
		};
		let interpreted = InterpretedObservation {
			raw_observation: observation.clone(),
			continue_allowed: true,
			should_ask_user: false,
			should_emit_final_answer: false,
			should_fail: false,
			terminal: false,
			budget_exhausted: false,
			recovery_exhausted: false,
			remaining_step_budget: 3,
			remaining_recovery_budget: 2,
			new_working_directory: None,
			visible_tools: vec!["command.run".to_string(), "general.execute".to_string()],
		};
		state.record_step(StepRecord::tool_call(
			1,
			NextStepDecision {
				action: NextStepAction::CallTool,
				tool_name: Some("command.run".to_string()),
				arguments: Some(json!({ "command": "pwd" })),
				reason: "run explicit command".to_string(),
				final_message: None,
			},
			vec!["command.run".to_string(), "general.execute".to_string()],
			json!({"ok": true}),
			StepObservation::Tool(observation),
			interpreted,
			Some(10),
			3,
			2,
			"/workspace",
		));
		state.record_step(StepRecord::terminal(
			2,
			NextStepDecision {
				action: NextStepAction::FinalAnswer,
				tool_name: None,
				arguments: None,
				reason: "answer from grounded command".to_string(),
				final_message: Some("/workspace".to_string()),
			},
			vec!["command.run".to_string(), "general.execute".to_string()],
			Some(StepObservation::FinalMessage {
				final_message: "/workspace".to_string(),
			}),
			2,
			2,
			"/workspace",
		));

		let trace = runtime_loop_trace(&state);
		assert_eq!(trace.step_count, 2);
		assert_eq!(trace.steps[0].decision.action, "call_tool");
		assert_eq!(
			trace.steps[0].visible_tools_before,
			vec!["command.run".to_string(), "general.execute".to_string()]
		);
		let report = check_runtime_loop_trace(&trace);
		assert!(
			report.issues.is_empty(),
			"unexpected trace issues: {report:?}"
		);
	}
}
