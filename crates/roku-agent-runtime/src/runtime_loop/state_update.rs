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

use crate::runtime_loop::{LoopState, ToolObservation};

/// Runtime interpretation layer derived from a single `ToolObservation`.
///
/// ## Why this exists
/// Tools return grounded facts, but the loop still needs a runtime-owned interpretation of what
/// those facts mean for budgets and control flow. `InterpretedObservation` is that translation
/// layer.
///
/// ## Fields
/// - `raw_observation`: Original grounded tool observation.
/// - `continue_allowed`: Whether the loop may ask for another explicit next-step decision.
/// - `should_ask_user`: Whether the runtime should stop in `ask_user`.
/// - `should_emit_final_answer`: Whether the runtime should stop in `final_answer`.
/// - `should_fail`: Whether the runtime should stop in `fail`.
/// - `terminal`: Whether the tool observation explicitly marked itself terminal.
/// - `budget_exhausted`: Whether step budget ended after this observation.
/// - `recovery_exhausted`: Whether recovery budget ended after this observation.
/// - `remaining_step_budget`: Remaining step budget after applying this observation.
/// - `remaining_recovery_budget`: Remaining recovery budget after applying this observation.
/// - `new_working_directory`: Updated working directory, if the observation changed it.
/// - `visible_tools`: Visible tools snapshot carried forward for the next round.
///
/// ## Invariants
/// - This struct interprets tool facts; it does not replace them.
/// - `continue_allowed` is false whenever any explicit terminal branch is selected.
/// - `multiple_candidates` remains a recoverable observation so the live loop may still choose a
///   better tool before pausing for user clarification.
/// - Non-`multiple_candidates` tool failures are treated as loop-failing observations today.
///
/// ## Non-Goals
/// - This struct does not directly choose the next tool.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InterpretedObservation {
	pub raw_observation: ToolObservation,
	pub continue_allowed: bool,
	pub should_ask_user: bool,
	pub should_emit_final_answer: bool,
	pub should_fail: bool,
	pub terminal: bool,
	pub budget_exhausted: bool,
	pub recovery_exhausted: bool,
	pub remaining_step_budget: u32,
	pub remaining_recovery_budget: u32,
	pub new_working_directory: Option<String>,
	pub visible_tools: Vec<String>,
}

pub fn interpret_observation(
	state: &LoopState,
	raw_observation: ToolObservation,
	new_working_directory: Option<String>,
) -> InterpretedObservation {
	let is_multiple_candidates =
		raw_observation.error_type.as_deref() == Some("multiple_candidates");
	let needs_more_information =
		raw_observation.error_type.as_deref() == Some("needs_more_information");
	let remaining_step_budget = state.remaining_step_budget.saturating_sub(1);
	let remaining_recovery_budget = if raw_observation.ok
		|| raw_observation.terminal
		|| is_multiple_candidates
		|| needs_more_information
	{
		state.remaining_recovery_budget
	} else {
		state.remaining_recovery_budget.saturating_sub(1)
	};
	let terminal = raw_observation.terminal;
	let budget_exhausted = remaining_step_budget == 0;
	let recovery_exhausted = !raw_observation.ok && !terminal && remaining_recovery_budget == 0;
	let should_ask_user = needs_more_information;
	let should_emit_final_answer = raw_observation.ok && terminal;
	// Non-terminal errors consume recovery budget and let the LLM see the error
	// as an observation so it can try alternatives.  Only terminal errors or
	// exhausted budgets force failure.
	let should_fail = (!raw_observation.ok && terminal) || budget_exhausted || recovery_exhausted;
	InterpretedObservation {
		continue_allowed: !terminal && !budget_exhausted && !recovery_exhausted && !should_ask_user,
		should_ask_user,
		should_emit_final_answer,
		should_fail,
		terminal,
		budget_exhausted,
		recovery_exhausted,
		remaining_step_budget,
		remaining_recovery_budget,
		new_working_directory,
		visible_tools: state.visible_tools.clone(),
		raw_observation,
	}
}

#[cfg(test)]
mod tests {
	use roku_common_types::ResourceSelector;
	use serde_json::json;

	use super::interpret_observation;
	use crate::router::{IntentFamily, RouteDecision, RouteRisk};
	use crate::runtime_loop::{LoopContext, LoopState, ToolObservation};

	fn loop_state() -> LoopState {
		let context = LoopContext {
			request_id: "req-1".to_string(),
			session_id: "session-1".to_string(),
			goal: "Inspect the runtime".to_string(),
			workspace_root: "/workspace".to_string(),
			working_directory: "/workspace".to_string(),
			visible_tools: vec!["inventory.describe".to_string()],
			bound_resources: vec![ResourceSelector::tool("inventory.describe".to_string())],
			route_decision: RouteDecision::new(
				IntentFamily::Chat,
				0.9,
				false,
				RouteRisk::Low,
				vec!["inventory.describe".to_string()],
				Vec::new(),
				Vec::new(),
				"chat request",
			),
			last_observation: None,
		};
		LoopState::new("loop-req-1", &context)
	}

	#[test]
	fn non_terminal_success_keeps_loop_running_when_budget_remains() {
		let state = loop_state();
		let interpreted = interpret_observation(
			&state,
			ToolObservation {
				ok: true,
				tool_name: "inventory.describe".to_string(),
				error_type: None,
				terminal: false,
				data: json!({}),
				message: "partial observation".to_string(),
			},
			None,
		);

		assert!(interpreted.continue_allowed);
		assert!(!interpreted.should_emit_final_answer);
		assert!(!interpreted.should_fail);
	}

	#[test]
	fn terminal_observation_stops_loop_without_continuing() {
		let state = loop_state();
		let interpreted = interpret_observation(
			&state,
			ToolObservation {
				ok: true,
				tool_name: "custom.tool".to_string(),
				error_type: None,
				terminal: true,
				data: json!({}),
				message: "terminal success".to_string(),
			},
			None,
		);

		assert!(!interpreted.continue_allowed);
		assert!(interpreted.should_emit_final_answer);
		assert!(!interpreted.should_fail);
		assert!(interpreted.terminal);
	}

	#[test]
	fn recovery_budget_exhaustion_stops_loop_as_failure() {
		let mut state = loop_state();
		state.remaining_recovery_budget = 1;
		let interpreted = interpret_observation(
			&state,
			ToolObservation {
				ok: false,
				tool_name: "custom.tool".to_string(),
				error_type: Some("execution_failed".to_string()),
				terminal: false,
				data: json!({}),
				message: "still failing".to_string(),
			},
			None,
		);

		assert!(!interpreted.continue_allowed);
		assert!(interpreted.should_fail);
		assert!(interpreted.recovery_exhausted);
	}

	#[test]
	fn non_terminal_errors_allow_recovery_via_budget() {
		let state = loop_state();
		let interpreted = interpret_observation(
			&state,
			ToolObservation {
				ok: false,
				tool_name: "command.run".to_string(),
				error_type: Some("non_zero_exit".to_string()),
				terminal: false,
				data: json!({}),
				message: "command failed".to_string(),
			},
			None,
		);

		// Non-terminal errors should let the LLM continue (consuming recovery budget).
		assert!(
			interpreted.continue_allowed,
			"non-terminal errors should allow continuation"
		);
		assert!(!interpreted.should_ask_user);
		assert!(!interpreted.should_emit_final_answer);
		assert!(
			!interpreted.should_fail,
			"non-terminal errors should not force failure while recovery budget remains"
		);
	}

	#[test]
	fn multiple_candidates_stays_recoverable_for_the_live_loop() {
		let state = loop_state();
		let interpreted = interpret_observation(
			&state,
			ToolObservation {
				ok: false,
				tool_name: "fs.find".to_string(),
				error_type: Some("multiple_candidates".to_string()),
				terminal: false,
				data: json!({
					"matches": ["/workspace/a/runtime.rs", "/workspace/b/runtime.rs"]
				}),
				message: "Found 2 matching candidates.".to_string(),
			},
			None,
		);

		assert!(interpreted.continue_allowed);
		assert!(!interpreted.should_ask_user);
		assert!(!interpreted.should_emit_final_answer);
		assert!(!interpreted.should_fail);
	}

	#[test]
	fn multiple_candidates_does_not_consume_recovery_budget() {
		let mut state = loop_state();
		state.remaining_recovery_budget = 1;
		let interpreted = interpret_observation(
			&state,
			ToolObservation {
				ok: false,
				tool_name: "fs.find".to_string(),
				error_type: Some("multiple_candidates".to_string()),
				terminal: false,
				data: json!({
					"match_count": 2,
					"matches": ["/workspace/a/runtime.rs", "/workspace/b/runtime.rs"]
				}),
				message: "Found 2 matching candidates.".to_string(),
			},
			None,
		);

		assert_eq!(interpreted.remaining_recovery_budget, 1);
		assert!(!interpreted.recovery_exhausted);
		assert!(interpreted.continue_allowed);
		assert!(!interpreted.should_fail);
	}

	#[test]
	fn needs_more_information_upgrades_to_ask_user_without_consuming_recovery_budget() {
		let mut state = loop_state();
		state.remaining_recovery_budget = 1;
		let interpreted = interpret_observation(
			&state,
			ToolObservation {
				ok: false,
				tool_name: "general.execute".to_string(),
				error_type: Some("needs_more_information".to_string()),
				terminal: false,
				data: json!({}),
				message: "请告诉我你想统计哪个具体目录或文件的代码行数。".to_string(),
			},
			None,
		);

		assert!(!interpreted.continue_allowed);
		assert!(interpreted.should_ask_user);
		assert!(!interpreted.should_fail);
		assert_eq!(interpreted.remaining_recovery_budget, 1);
	}
}
