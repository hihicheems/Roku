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

use serde::{Deserialize, Serialize};

use crate::runtime_loop::{LoopState, StepAction, StepObservation};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolProbeCheckReport {
	pub expected_tool: String,
	pub selected_tool: Option<String>,
	pub tool_selected_correctly: bool,
	pub raw_output_captured: bool,
	pub normalized_observation_captured: bool,
	pub interpreted_observation_captured: bool,
	pub interpretation_matches_observation: bool,
	pub reached_terminal_outcome: bool,
	pub issues: Vec<String>,
}

pub fn check_seed_tool_probe(loop_state: &LoopState, expected_tool: &str) -> ToolProbeCheckReport {
	let tool_step = loop_state
		.history
		.iter()
		.find(|step| step.action == StepAction::CallTool);
	let terminal_step = loop_state.history.iter().rev().find(|step| {
		matches!(
			step.action,
			StepAction::AskUser | StepAction::FinalAnswer | StepAction::Fail
		)
	});
	let selected_tool = tool_step.and_then(|step| step.tool_name.clone());
	let tool_selected_correctly = selected_tool.as_deref() == Some(expected_tool);
	let raw_output_captured = tool_step
		.and_then(|step| step.raw_tool_output.as_ref())
		.is_some();
	let normalized_observation_captured = tool_step
		.and_then(|step| step.observation.as_ref())
		.is_some_and(|observation| matches!(observation, StepObservation::Tool(_)));
	let interpreted_observation_captured = tool_step
		.and_then(|step| step.interpreted_observation.as_ref())
		.is_some();
	let interpretation_matches_observation = tool_step.is_some_and(|step| {
		match (
			step.observation.as_ref(),
			step.interpreted_observation.as_ref(),
		) {
			(Some(StepObservation::Tool(observation)), Some(interpreted)) => {
				interpreted.raw_observation == *observation
			}
			_ => false,
		}
	});
	let reached_terminal_outcome = terminal_step.is_some();
	let mut issues = Vec::new();
	if !tool_selected_correctly {
		issues.push(format!(
			"expected tool `{expected_tool}`, but selected `{}`",
			selected_tool.clone().unwrap_or_else(|| "none".to_string())
		));
	}
	if !raw_output_captured {
		issues.push("tool step did not capture raw output".to_string());
	}
	if !normalized_observation_captured {
		issues.push("tool step did not capture a normalized ToolObservation".to_string());
	}
	if !interpreted_observation_captured {
		issues.push("tool step did not capture an InterpretedObservation".to_string());
	}
	if interpreted_observation_captured && !interpretation_matches_observation {
		issues.push(
			"interpreted observation diverges from the recorded normalized observation".to_string(),
		);
	}
	if !reached_terminal_outcome {
		issues.push("loop did not reach a terminal outcome".to_string());
	}
	ToolProbeCheckReport {
		expected_tool: expected_tool.to_string(),
		selected_tool,
		tool_selected_correctly,
		raw_output_captured,
		normalized_observation_captured,
		interpreted_observation_captured,
		interpretation_matches_observation,
		reached_terminal_outcome,
		issues,
	}
}

#[cfg(test)]
mod tests {
	use roku_common_types::ResourceSelector;
	use serde_json::json;

	use super::check_seed_tool_probe;
	use crate::router::{IntentFamily, RouteDecision, RouteRisk};
	use crate::runtime_loop::{
		InterpretedObservation, LoopContext, LoopState, NextStepAction, NextStepDecision,
		StepObservation, StepRecord, ToolObservation,
	};

	fn loop_state() -> LoopState {
		let context = LoopContext {
			request_id: "req-1".to_string(),
			session_id: "session-1".to_string(),
			goal: "Run `pwd`".to_string(),
			workspace_root: "/workspace".to_string(),
			working_directory: "/workspace".to_string(),
			visible_tools: vec!["command.run".to_string(), "inventory.describe".to_string()],
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
		LoopState::new("loop-1", &context)
	}

	#[test]
	fn reports_clean_probe_trace_when_all_layers_are_present() {
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
			visible_tools: vec!["command.run".to_string(), "inventory.describe".to_string()],
		};
		state.record_step(StepRecord::tool_call(
			1,
			NextStepDecision {
				action: NextStepAction::CallTool,
				tool_name: Some("command.run".to_string()),
				arguments: Some(json!({ "command": "pwd" })),
				tool_calls: None,
				reason: "run explicit command".to_string(),
				final_message: None,
			},
			state.visible_tools.clone(),
			state.bound_resources.clone(),
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
			crate::runtime_loop::StepAction::FinalAnswer,
			NextStepDecision {
				action: NextStepAction::FinalAnswer,
				tool_name: None,
				arguments: None,
				tool_calls: None,
				reason: "answer from grounded command".to_string(),
				final_message: Some("/workspace".to_string()),
			},
			state.visible_tools.clone(),
			state.bound_resources.clone(),
			Some(StepObservation::FinalMessage {
				final_message: "/workspace".to_string(),
			}),
			2,
			2,
			"/workspace",
		));

		let report = check_seed_tool_probe(&state, "command.run");
		assert!(report.issues.is_empty(), "unexpected issues: {report:?}");
		assert!(report.tool_selected_correctly);
		assert!(report.raw_output_captured);
		assert!(report.interpreted_observation_captured);
	}
}
