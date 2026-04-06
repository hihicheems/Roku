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

use roku_common_types::ResourceSelector;
use serde::{Deserialize, Serialize};

use crate::router::RouteDecision;
use crate::runtime_config::LoopRuntimeConfig;
use crate::runtime_loop::grounding::{
	extract_explicit_path_candidates, extract_explicit_python_code, extract_explicit_shell_command,
	extract_explicit_table_path, extract_glob_pattern, extract_web_query,
};
use crate::runtime_loop::{AskUserPayload, LoopContext, StepRecord, ToolObservation};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoopStatus {
	Received,
	Classified,
	LoopRunning,
	AwaitingUser,
	Succeeded,
	Failed,
	Stopped,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AmbiguityStagnation {
	pub(crate) tool_name: String,
	pub(crate) candidate_fingerprint: String,
	pub(crate) match_count: usize,
	pub(crate) explicit_grounding_fingerprint: String,
	pub(crate) streak: u32,
}

/// Source-of-truth runtime state for a single ReAct loop run.
///
/// ## Why this exists
/// `LoopState` is the mutable state machine for runtime loop execution. It captures the current
/// goal, routing seed, budgets, visible tools, replay history, and the latest grounded
/// observation so each next-step decision can be derived from one canonical state object.
///
/// ## Fields
/// - `run_id`: Stable identifier for this loop instance.
/// - `request_id`: Original request identifier.
/// - `session_id`: Session identifier used for ask-user resume semantics.
/// - `goal`: User-visible goal for the current run.
/// - `route_decision`: Initial route seed that constrains the loop.
/// - `status`: Current lifecycle state of the loop.
/// - `step_index`: Index of the latest recorded step.
/// - `remaining_step_budget`: Remaining loop steps before forced termination.
/// - `remaining_recovery_budget`: Remaining recovery opportunities after non-terminal errors.
/// - `working_directory`: Current working directory after prior steps.
/// - `working_summary`: Runtime-owned short-term summary carried alongside replay history.
/// - `visible_tools`: Tools visible for the next decision round.
/// - `bound_resources`: Resources already bound to the loop.
/// - `history`: Recorded step facts for replay and context projection.
/// - `last_observation`: Latest grounded tool observation, if any.
/// - `awaiting_user`: Explicit resume contract for a paused `ask_user` step, if the loop is
///   currently waiting on the user.
///
/// ## Invariants
/// - `history` is append-only within a run.
/// - `last_observation` must reflect the most recent tool observation recorded in `history`.
/// - `visible_tools` may be recomputed between rounds, but the current round must treat this
///   field as the active visibility truth.
/// - `awaiting_user` must only be populated while the loop is paused in `AwaitingUser`.
///
/// ## Non-Goals
/// - `LoopState` is not the prompt projection passed directly to the model.
/// - `LoopState` does not encode a multi-step plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoopState {
	pub run_id: String,
	pub request_id: String,
	pub session_id: String,
	pub goal: String,
	pub route_decision: RouteDecision,
	pub status: LoopStatus,
	pub step_index: u32,
	pub remaining_step_budget: u32,
	pub remaining_recovery_budget: u32,
	pub working_directory: String,
	#[serde(default)]
	pub working_summary: String,
	pub visible_tools: Vec<String>,
	pub bound_resources: Vec<ResourceSelector>,
	pub history: Vec<StepRecord>,
	pub last_observation: Option<ToolObservation>,
	#[serde(default)]
	pub awaiting_user: Option<AskUserPayload>,
	#[serde(default)]
	pub(crate) latest_explicit_grounding_fingerprint: String,
	#[serde(default)]
	pub(crate) ambiguity_stagnation: Option<AmbiguityStagnation>,
}

impl LoopState {
	pub fn new(run_id: impl Into<String>, context: &LoopContext) -> Self {
		let defaults = LoopRuntimeConfig::default();
		Self::with_budgets(
			run_id,
			context,
			defaults.initial_step_budget,
			defaults.initial_recovery_budget,
		)
	}

	pub fn with_budgets(
		run_id: impl Into<String>,
		context: &LoopContext,
		initial_step_budget: u32,
		initial_recovery_budget: u32,
	) -> Self {
		Self {
			run_id: run_id.into(),
			request_id: context.request_id.clone(),
			session_id: context.session_id.clone(),
			goal: context.goal.clone(),
			route_decision: context.route_decision.clone(),
			status: LoopStatus::LoopRunning,
			step_index: 0,
			remaining_step_budget: initial_step_budget,
			remaining_recovery_budget: initial_recovery_budget,
			working_directory: context.working_directory.clone(),
			working_summary: String::new(),
			visible_tools: context.visible_tools.clone(),
			bound_resources: context.bound_resources.clone(),
			history: Vec::new(),
			last_observation: context.last_observation.clone(),
			awaiting_user: None,
			latest_explicit_grounding_fingerprint: explicit_grounding_fingerprint(&context.goal),
			ambiguity_stagnation: None,
		}
	}

	pub fn record_step(&mut self, step: StepRecord) {
		self.step_index = step.step_index;
		self.remaining_step_budget = step.remaining_step_budget_after;
		self.remaining_recovery_budget = step.remaining_recovery_budget_after;
		self.working_directory = step.working_directory_after.clone();
		match &step.observation {
			Some(crate::runtime_loop::StepObservation::Tool(observation)) => {
				self.last_observation = Some(observation.clone());
				self.awaiting_user = None;
				self.update_ambiguity_stagnation(observation);
			}
			Some(crate::runtime_loop::StepObservation::AskUser { final_message }) => {
				self.awaiting_user = Some(AskUserPayload::freeform(final_message.clone()));
				self.ambiguity_stagnation = None;
			}
			Some(crate::runtime_loop::StepObservation::FinalMessage { .. }) | None => {
				self.awaiting_user = None;
				self.ambiguity_stagnation = None;
			}
		}
		self.status = match step.action {
			crate::runtime_loop::StepAction::CallTool => LoopStatus::LoopRunning,
			crate::runtime_loop::StepAction::AskUser => LoopStatus::AwaitingUser,
			crate::runtime_loop::StepAction::FinalAnswer => LoopStatus::Succeeded,
			crate::runtime_loop::StepAction::Fail => LoopStatus::Failed,
			crate::runtime_loop::StepAction::Stop => LoopStatus::Stopped,
			crate::runtime_loop::StepAction::CompactBoundary => LoopStatus::LoopRunning,
		};
		self.history.push(step);
	}

	pub(crate) fn note_grounding_input(&mut self, grounding_input: &str) {
		let fingerprint = explicit_grounding_fingerprint(grounding_input);
		if !fingerprint.is_empty() {
			self.latest_explicit_grounding_fingerprint = fingerprint;
		}
	}

	pub(crate) fn ambiguity_requires_ask_user(&self, grounding_input: &str) -> bool {
		let Some(stagnation) = self.ambiguity_stagnation.as_ref() else {
			return false;
		};
		if stagnation.streak < 2 {
			return false;
		}
		let current_fingerprint = explicit_grounding_fingerprint(grounding_input);
		current_fingerprint.is_empty()
			|| current_fingerprint == stagnation.explicit_grounding_fingerprint
	}

	fn update_ambiguity_stagnation(&mut self, observation: &ToolObservation) {
		let Some(candidate_fingerprint) = ambiguous_candidate_fingerprint(observation) else {
			self.ambiguity_stagnation = None;
			return;
		};
		let match_count = observation
			.data
			.get("match_count")
			.and_then(serde_json::Value::as_u64)
			.unwrap_or_default() as usize;
		let next_streak = self
			.ambiguity_stagnation
			.as_ref()
			.filter(|previous| {
				previous.tool_name == observation.tool_name
					&& previous.candidate_fingerprint == candidate_fingerprint
					&& previous.match_count == match_count
					&& previous.explicit_grounding_fingerprint
						== self.latest_explicit_grounding_fingerprint
			})
			.map(|previous| previous.streak.saturating_add(1))
			.unwrap_or(1);
		self.ambiguity_stagnation = Some(AmbiguityStagnation {
			tool_name: observation.tool_name.clone(),
			candidate_fingerprint,
			match_count,
			explicit_grounding_fingerprint: self.latest_explicit_grounding_fingerprint.clone(),
			streak: next_streak,
		});
	}
}

fn explicit_grounding_fingerprint(goal: &str) -> String {
	let mut parts = Vec::new();
	let explicit_paths = extract_explicit_path_candidates(goal);
	if !explicit_paths.is_empty() {
		parts.push(format!("paths={}", explicit_paths.join("|")));
	}
	if let Some(table_path) = extract_explicit_table_path(goal) {
		parts.push(format!("table={table_path}"));
	}
	if let Some(glob) = extract_glob_pattern(goal) {
		parts.push(format!("glob={glob}"));
	}
	if let Some(query) = extract_web_query(goal) {
		parts.push(format!("web={query}"));
	}
	if let Some(command) = extract_explicit_shell_command(goal) {
		parts.push(format!("shell={command}"));
	}
	if let Some(code) = extract_explicit_python_code(goal) {
		parts.push(format!("python={code}"));
	}
	parts.join("||")
}

fn ambiguous_candidate_fingerprint(observation: &ToolObservation) -> Option<String> {
	if observation.error_type.as_deref() != Some("multiple_candidates") {
		return None;
	}
	if observation
		.data
		.get("resolved_path")
		.and_then(serde_json::Value::as_str)
		.is_some()
	{
		return None;
	}
	let mut matches = observation
		.data
		.get("matches")
		.and_then(serde_json::Value::as_array)
		.map(|values| {
			values
				.iter()
				.filter_map(serde_json::Value::as_str)
				.map(str::to_string)
				.collect::<Vec<_>>()
		})
		.unwrap_or_default();
	if matches.is_empty() {
		return None;
	}
	matches.sort();
	Some(matches.join("|"))
}

#[cfg(test)]
mod tests {
	use roku_common_types::ResourceSelector;
	use serde_json::json;

	use super::{LoopState, LoopStatus};
	use crate::router::{IntentFamily, RouteDecision, RouteRisk};
	use crate::runtime_loop::LoopContext;
	use crate::runtime_loop::ask_user::{AskUserPayload, AskUserResumeContract};
	use crate::runtime_loop::next_step::{NextStepAction, NextStepDecision};
	use crate::runtime_loop::observation::{StepObservation, ToolObservation};
	use crate::runtime_loop::state_update::InterpretedObservation;
	use crate::runtime_loop::step_record::{StepAction, StepRecord};

	fn loop_context() -> LoopContext {
		LoopContext {
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
		}
	}

	#[test]
	fn new_loop_state_starts_with_empty_working_summary() {
		let state = LoopState::new("loop-1", &loop_context());

		assert_eq!(state.working_summary, "");
	}

	#[test]
	fn serde_round_trip_preserves_explicit_working_summary() {
		let mut state = LoopState::new("loop-1", &loop_context());
		state.working_summary = "Grounded repo layout and pending blocker.".to_string();

		let value = serde_json::to_value(&state).expect("loop state should serialize");
		assert_eq!(
			value.get("working_summary"),
			Some(&json!("Grounded repo layout and pending blocker."))
		);

		let restored: LoopState =
			serde_json::from_value(value).expect("loop state should deserialize");
		assert_eq!(
			restored.working_summary,
			"Grounded repo layout and pending blocker."
		);
	}

	#[test]
	fn serde_defaults_missing_working_summary_for_legacy_snapshots() {
		let state = LoopState::new("loop-1", &loop_context());
		let mut value = serde_json::to_value(&state).expect("loop state should serialize");
		value
			.as_object_mut()
			.expect("loop state json should be an object")
			.remove("working_summary");

		let restored: LoopState =
			serde_json::from_value(value).expect("legacy loop state should deserialize");
		assert_eq!(restored.working_summary, "");
	}

	#[test]
	fn loop_state_serialization_roundtrip_preserves_all_fields() {
		let tool_observation = ToolObservation {
			ok: true,
			tool_name: "inventory.describe".to_string(),
			error_type: None,
			terminal: false,
			data: json!({"path": "/workspace/README.md", "size": 1024}),
			message: "File described successfully".to_string(),
		};

		let decision_call_tool = NextStepDecision {
			action: NextStepAction::CallTool,
			tool_name: Some("inventory.describe".to_string()),
			arguments: Some(json!({"path": "/workspace/README.md"})),
			reason: "Need to inspect the file".to_string(),
			final_message: None,
		};

		let decision_ask_user = NextStepDecision {
			action: NextStepAction::AskUser,
			tool_name: None,
			arguments: None,
			reason: "Need clarification from user".to_string(),
			final_message: Some("Which file did you mean?".to_string()),
		};

		let decision_final = NextStepDecision {
			action: NextStepAction::FinalAnswer,
			tool_name: None,
			arguments: None,
			reason: "Task complete".to_string(),
			final_message: Some("Done.".to_string()),
		};

		let interpreted = InterpretedObservation {
			raw_observation: tool_observation.clone(),
			continue_allowed: true,
			should_ask_user: false,
			should_emit_final_answer: false,
			should_fail: false,
			terminal: false,
			budget_exhausted: false,
			recovery_exhausted: false,
			remaining_step_budget: 9,
			remaining_recovery_budget: 3,
			new_working_directory: Some("/workspace/sub".to_string()),
			visible_tools: vec!["inventory.describe".to_string(), "shell.exec".to_string()],
		};

		let step_tool = StepRecord::tool_call(
			1,
			decision_call_tool,
			vec!["inventory.describe".to_string(), "shell.exec".to_string()],
			vec![ResourceSelector::tool("inventory.describe".to_string())],
			json!({"raw": "output"}),
			StepObservation::Tool(tool_observation.clone()),
			interpreted,
			Some(42),
			9,
			3,
			"/workspace/sub",
		);

		let step_ask = StepRecord::terminal(
			2,
			StepAction::AskUser,
			decision_ask_user,
			vec!["inventory.describe".to_string()],
			vec![],
			Some(StepObservation::AskUser {
				final_message: "Which file did you mean?".to_string(),
			}),
			8,
			3,
			"/workspace/sub",
		);

		let step_final = StepRecord::terminal(
			3,
			StepAction::FinalAnswer,
			decision_final,
			vec!["inventory.describe".to_string(), "web.search".to_string()],
			vec![ResourceSelector::tool("web.search".to_string())],
			Some(StepObservation::FinalMessage {
				final_message: "Done.".to_string(),
			}),
			7,
			3,
			"/workspace/sub",
		);

		let mut state = LoopState::new("loop-roundtrip", &loop_context());
		state.record_step(step_tool);
		state.record_step(step_ask);
		state.record_step(step_final);

		state.working_summary = "Inspected file, asked user, completed.".to_string();
		state.visible_tools = vec![
			"inventory.describe".to_string(),
			"shell.exec".to_string(),
			"web.search".to_string(),
		];
		state.bound_resources = vec![
			ResourceSelector::tool("inventory.describe".to_string()),
			ResourceSelector::tool("web.search".to_string()),
		];
		state.awaiting_user = Some(AskUserPayload {
			final_message: "Which file did you mean?".to_string(),
			resume_contract: AskUserResumeContract::CandidateSelection {
				candidates: vec!["file_a.txt".to_string(), "file_b.txt".to_string()],
			},
			resume_directive: None,
		});
		state.status = LoopStatus::AwaitingUser;

		let json_str = serde_json::to_string(&state).expect("loop state should serialize to JSON");
		let deserialized: LoopState =
			serde_json::from_str(&json_str).expect("loop state should deserialize from JSON");

		assert_eq!(state, deserialized);
		assert_eq!(deserialized.history.len(), 3);
		assert_eq!(deserialized.status, LoopStatus::AwaitingUser);
		assert_eq!(
			deserialized.working_summary,
			"Inspected file, asked user, completed."
		);
		assert!(deserialized.awaiting_user.is_some());
		assert_eq!(deserialized.visible_tools.len(), 3);
		assert_eq!(deserialized.bound_resources.len(), 2);
		assert!(deserialized.last_observation.is_some());
	}
}
