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

use super::LoopState;

/// Estimate the approximate token usage of the current loop context.
///
/// Uses character count / 4 for text fields and JSON serialization length / 4
/// for structured data. This is intentionally approximate — the goal is a
/// reliable "are we getting close to the limit?" signal, not precise counting.
pub fn estimate_context_tokens(state: &LoopState) -> u64 {
	let mut chars: u64 = 0;

	chars += state.goal.len() as u64;
	chars += state.working_summary.len() as u64;

	for tool in &state.visible_tools {
		chars += tool.len() as u64;
	}

	if let Ok(json) = serde_json::to_string(&state.bound_resources) {
		chars += json.len() as u64;
	}

	if let Some(Ok(json)) = state.last_observation.as_ref().map(serde_json::to_string) {
		chars += json.len() as u64;
	}

	for step in &state.history {
		if let Ok(json) = serde_json::to_string(step) {
			chars += json.len() as u64;
		}
	}

	chars / 4
}

/// Check whether the loop context has exceeded the compact threshold.
#[cfg(test)]
fn should_compact(state: &LoopState, config: &crate::runtime_config::LoopRuntimeConfig) -> bool {
	let estimated = estimate_context_tokens(state);
	let threshold = config.compact_threshold_tokens();
	estimated > threshold
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::router::{IntentFamily, RouteDecision, RouteRisk};
	use crate::runtime_loop::observation::ToolObservation;
	use crate::runtime_loop::state_update::InterpretedObservation;
	use crate::runtime_loop::step_record::StepRecord;
	use crate::runtime_config::LoopRuntimeConfig;
	use crate::runtime_loop::{NextStepAction, NextStepDecision, StepObservation};
	use serde_json::json;

	fn minimal_loop_state() -> LoopState {
		LoopState {
			run_id: "run-1".to_string(),
			request_id: "req-1".to_string(),
			session_id: "session-1".to_string(),
			goal: "test goal".to_string(),
			route_decision: RouteDecision::new(
				IntentFamily::Chat,
				0.9,
				false,
				RouteRisk::Low,
				Vec::new(),
				Vec::new(),
				Vec::new(),
				"test",
			),
			status: crate::runtime_loop::LoopStatus::LoopRunning,
			step_index: 0,
			remaining_step_budget: 10,
			remaining_recovery_budget: 2,
			working_directory: "/workspace".to_string(),
			working_summary: String::new(),
			visible_tools: vec!["general.execute".to_string()],
			bound_resources: Vec::new(),
			history: Vec::new(),
			last_observation: None,
			awaiting_user: None,
			latest_explicit_grounding_fingerprint: String::new(),
			ambiguity_stagnation: None,
		}
	}

	fn sample_observation() -> ToolObservation {
		ToolObservation {
			ok: true,
			tool_name: "general.execute".to_string(),
			error_type: None,
			terminal: false,
			data: json!({ "result": "ok" }),
			message: "Command completed successfully".to_string(),
		}
	}

	fn sample_step(index: u32, budget_after: u32) -> StepRecord {
		let observation = sample_observation();
		let interpreted = InterpretedObservation {
			raw_observation: observation.clone(),
			continue_allowed: true,
			should_ask_user: false,
			should_emit_final_answer: false,
			should_fail: false,
			terminal: false,
			budget_exhausted: false,
			recovery_exhausted: false,
			remaining_step_budget: budget_after,
			remaining_recovery_budget: 2,
			new_working_directory: None,
			visible_tools: vec!["general.execute".to_string()],
		};
		StepRecord::tool_call(
			index,
			NextStepDecision {
				action: NextStepAction::CallTool,
				tool_name: Some("general.execute".to_string()),
				arguments: Some(json!({"command": "echo hello"})),
				reason: "Execute the command.".to_string(),
				final_message: None,
			},
			vec!["general.execute".to_string()],
			Vec::new(),
			json!({"ok": true, "message": "hello"}),
			StepObservation::Tool(observation),
			interpreted,
			Some(150),
			budget_after,
			2,
			"/workspace",
		)
	}

	#[test]
	fn estimation_returns_nonzero_for_nonempty_state() {
		let state = minimal_loop_state();
		let tokens = estimate_context_tokens(&state);
		assert!(
			tokens > 0,
			"estimation should be positive for a state with a goal"
		);
	}

	#[test]
	fn estimation_grows_with_history_size() {
		let mut state = minimal_loop_state();
		let tokens_empty = estimate_context_tokens(&state);

		state.record_step(sample_step(1, 9));
		let tokens_one = estimate_context_tokens(&state);
		assert!(
			tokens_one > tokens_empty,
			"one step ({tokens_one}) should exceed empty ({tokens_empty})"
		);

		state.record_step(sample_step(2, 8));
		let tokens_two = estimate_context_tokens(&state);
		assert!(
			tokens_two > tokens_one,
			"two steps ({tokens_two}) should exceed one step ({tokens_one})"
		);
	}

	#[test]
	fn estimation_includes_working_summary() {
		let mut state = minimal_loop_state();
		let before = estimate_context_tokens(&state);

		state.working_summary = "A".repeat(4000);
		let after = estimate_context_tokens(&state);
		assert!(
			after > before + 900,
			"4000 chars (~1000 tokens) should materially increase estimate"
		);
	}

	#[test]
	fn should_compact_returns_false_below_threshold() {
		let state = minimal_loop_state();
		let config = LoopRuntimeConfig::default();
		assert!(
			!should_compact(&state, &config),
			"minimal state should not trigger compact"
		);
	}

	#[test]
	fn should_compact_returns_true_above_threshold() {
		let mut state = minimal_loop_state();
		// Inject a large working summary to push tokens above threshold.
		// Default threshold: 200_000 * 0.75 = 150_000 tokens ≈ 600_000 chars.
		state.working_summary = "X".repeat(700_000);
		let config = LoopRuntimeConfig::default();
		assert!(
			should_compact(&state, &config),
			"large context should trigger compact"
		);
	}

	#[test]
	fn should_compact_respects_custom_config() {
		let mut state = minimal_loop_state();
		// 400 chars of summary ≈ 100 tokens
		state.working_summary = "X".repeat(400);
		let config = LoopRuntimeConfig {
			context_window_tokens: 100,
			compact_threshold_ratio: 0.5,
			..LoopRuntimeConfig::default()
		};
		// Threshold = 100 * 0.5 = 50 tokens. State has goal + summary + tools > 50 tokens.
		assert!(
			should_compact(&state, &config),
			"small window should trigger compact even with moderate content"
		);
	}
}
