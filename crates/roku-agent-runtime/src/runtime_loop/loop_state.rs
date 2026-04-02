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
