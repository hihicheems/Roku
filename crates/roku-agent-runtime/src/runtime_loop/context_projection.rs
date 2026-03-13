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

use crate::runtime_loop::{LoopState, StepAction, StepObservation, ToolObservation};

/// Canonical model-facing projection derived from the current `LoopState`.
///
/// ## Why this exists
/// The runtime stores rich replay state in `LoopState`, but the next-step model should not
/// receive raw `StepRecord` logs verbatim. `ContextProjection` is the only supported prompt
/// projection for generic ReAct loop decisions.
///
/// ## Fields
/// - `goal`: The original user objective for the current loop.
/// - `intent_family`: The routed family that seeded the loop.
/// - `working_directory`: The current working directory after prior steps.
/// - `remaining_step_budget`: Step budget still available before the next action.
/// - `remaining_recovery_budget`: Recovery budget still available before the next action.
/// - `visible_tools`: Tool names currently visible to the next decision round.
/// - `last_observation`: The latest grounded tool observation, if any.
/// - `history_digest`: A compact summary of recent steps and current open state.
/// - `unresolved_blockers`: Open blockers that may require follow-up or user clarification.
/// - `working_assumptions`: Explicitly marked tentative assumptions, not grounded facts.
///
/// ## Invariants
/// - `history_digest` is derived from recent loop history and never stores raw `StepRecord`
///   payloads verbatim.
/// - `working_assumptions` must be clearly tentative and may be overturned by future
///   observations.
/// - `visible_tools` must come from the current runtime round, not a stale initialization-only
///   snapshot.
///
/// ## Non-Goals
/// - This projection is not a replay log or audit record.
/// - This projection does not encode multi-step plans.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextProjection {
	pub goal: String,
	pub intent_family: String,
	pub working_directory: String,
	pub remaining_step_budget: u32,
	pub remaining_recovery_budget: u32,
	pub visible_tools: Vec<String>,
	pub last_observation: Option<ToolObservation>,
	pub history_digest: String,
	pub unresolved_blockers: Vec<String>,
	pub working_assumptions: Vec<String>,
}

pub(crate) fn build_context_projection(loop_state: &LoopState) -> ContextProjection {
	let unresolved_blockers = unresolved_blockers(loop_state);
	let working_assumptions = working_assumptions(loop_state);
	ContextProjection {
		goal: loop_state.goal.clone(),
		intent_family: serde_json::to_string(&loop_state.route_decision.intent_family)
			.unwrap_or_else(|_| "\"unknown\"".to_string())
			.trim_matches('"')
			.to_string(),
		working_directory: loop_state.working_directory.clone(),
		remaining_step_budget: loop_state.remaining_step_budget,
		remaining_recovery_budget: loop_state.remaining_recovery_budget,
		visible_tools: loop_state.visible_tools.clone(),
		last_observation: loop_state.last_observation.clone(),
		history_digest: history_digest(loop_state, &unresolved_blockers, &working_assumptions),
		unresolved_blockers,
		working_assumptions,
	}
}

fn history_digest(
	loop_state: &LoopState,
	unresolved_blockers: &[String],
	working_assumptions: &[String],
) -> String {
	let recent_steps = loop_state
		.history
		.iter()
		.rev()
		.take(4)
		.cloned()
		.collect::<Vec<_>>()
		.into_iter()
		.rev()
		.collect::<Vec<_>>();

	let mut lines = vec!["Recent steps:".to_string()];
	if recent_steps.is_empty() {
		lines.push("- none".to_string());
	} else {
		lines.extend(recent_steps.iter().map(render_step_digest_line));
	}

	lines.push("Unresolved blockers:".to_string());
	if unresolved_blockers.is_empty() {
		lines.push("- none".to_string());
	} else {
		lines.extend(
			unresolved_blockers
				.iter()
				.map(|blocker| format!("- {blocker}")),
		);
	}

	lines.push("Working assumptions:".to_string());
	if working_assumptions.is_empty() {
		lines.push("- none".to_string());
	} else {
		lines.extend(
			working_assumptions
				.iter()
				.map(|assumption| format!("- {assumption}")),
		);
	}

	lines.join("\n")
}

fn render_step_digest_line(step: &crate::runtime_loop::StepRecord) -> String {
	let action = match step.action {
		StepAction::CallTool => "call_tool",
		StepAction::AskUser => "ask_user",
		StepAction::FinalAnswer => "final_answer",
		StepAction::Fail => "fail",
	};
	let tool_name = step.tool_name.as_deref().unwrap_or("none");
	let observation = step
		.observation
		.as_ref()
		.map(render_observation_digest)
		.unwrap_or_else(|| "observation=none".to_string());
	format!(
		"- step {}: action={} tool={} reason={} {}",
		step.step_index,
		action,
		tool_name,
		compact_text(&step.decision_reason),
		observation
	)
}

fn render_observation_digest(observation: &StepObservation) -> String {
	match observation {
		StepObservation::Tool(observation) => format!(
			"observation=tool ok={} terminal={} message={}",
			observation.ok,
			observation.terminal,
			compact_text(&observation.message)
		),
		StepObservation::AskUser { final_message } => {
			format!(
				"observation=ask_user message={}",
				compact_text(final_message)
			)
		}
		StepObservation::FinalMessage { final_message } => {
			format!(
				"observation=final_message message={}",
				compact_text(final_message)
			)
		}
	}
}

fn unresolved_blockers(loop_state: &LoopState) -> Vec<String> {
	let mut blockers = Vec::new();
	if let Some(observation) = loop_state.last_observation.as_ref()
		&& !observation.ok
		&& !observation.terminal
	{
		let error_type = observation.error_type.as_deref().unwrap_or("unknown_error");
		blockers.push(format!(
			"Latest observation may need follow-up: error_type={} message={}",
			error_type,
			compact_text(&observation.message)
		));
	}
	if matches!(
		loop_state.status,
		crate::runtime_loop::LoopStatus::AwaitingUser
	) {
		blockers.push(
			"The loop is awaiting a user reply and should continue from the existing state."
				.to_string(),
		);
	}
	blockers
}

fn working_assumptions(loop_state: &LoopState) -> Vec<String> {
	let mut assumptions = Vec::new();
	if let Some(observation) = loop_state.last_observation.as_ref()
		&& observation.ok
		&& !observation.terminal
	{
		assumptions.push(
			"The latest successful observation may still require another explicit tool step before completion."
				.to_string(),
		);
	}
	if !loop_state.visible_tools.is_empty() {
		assumptions.push(format!(
			"Any next tool call must stay within the currently visible tool set: {}.",
			loop_state.visible_tools.join(", ")
		));
	}
	assumptions
}

fn compact_text(text: &str) -> String {
	let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
	if compact.chars().count() <= 160 {
		return compact;
	}
	format!("{}...", compact.chars().take(157).collect::<String>())
}

#[cfg(test)]
mod tests {
	use roku_common_types::ResourceSelector;

	use super::build_context_projection;
	use crate::router::{IntentFamily, RouteDecision, RouteRisk};
	use crate::runtime_loop::{
		LoopContext, LoopState, StepObservation, ToolObservation, step_record::StepRecord,
	};

	fn sample_loop_state() -> LoopState {
		let context = LoopContext {
			request_id: "req-1".to_string(),
			session_id: "session-1".to_string(),
			goal: "Summarize the available tools".to_string(),
			workspace_root: "/workspace".to_string(),
			working_directory: "/workspace".to_string(),
			visible_tools: vec!["inventory.describe".to_string()],
			bound_resources: vec![ResourceSelector::tool("inventory.describe".to_string())],
			route_decision: RouteDecision::new(
				IntentFamily::Chat,
				0.92,
				false,
				RouteRisk::Low,
				vec!["inventory.describe".to_string()],
				Vec::new(),
				Vec::new(),
				"chat inventory request",
			),
			last_observation: None,
		};
		let mut state = LoopState::new("loop-req-1", &context);
		state.record_step(StepRecord::tool_call(
			1,
			"inventory.describe",
			"Use the inventory tool first.",
			StepObservation::Tool(ToolObservation {
				ok: true,
				tool_name: "inventory.describe".to_string(),
				error_type: None,
				terminal: false,
				data: serde_json::json!({ "runtime_mode": "deterministic" }),
				message: "deterministic placeholder only: inventory summary was not generated by a live runtime".to_string(),
			}),
			Some(12),
			3,
			2,
			"/workspace",
		));
		state
	}

	#[test]
	fn context_projection_summarizes_recent_history_without_raw_step_log_fields() {
		let projection = build_context_projection(&sample_loop_state());

		assert!(projection.history_digest.contains("step 1"));
		assert!(projection.history_digest.contains("inventory.describe"));
		assert!(projection.history_digest.contains("Working assumptions:"));
		assert!(!projection.history_digest.contains("started_at"));
		assert!(!projection.history_digest.contains("finished_at"));
	}
}
