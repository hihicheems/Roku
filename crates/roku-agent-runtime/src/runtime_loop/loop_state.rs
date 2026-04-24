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
use roku_plugin_llm::ToolDefinition;
use serde::{Deserialize, Serialize};

use crate::router::RouteDecision;
use crate::runtime_config::LoopRuntimeConfig;
use crate::runtime_loop::cache_break::CacheBreakDetector;
use crate::runtime_loop::compact::{CommittedBaseline, EstimatorCalibration};
use crate::runtime_loop::grounding::{
	extract_explicit_path_candidates, extract_explicit_python_code, extract_explicit_shell_command,
	extract_explicit_table_path, extract_glob_pattern, extract_web_query,
};
use crate::runtime_loop::tool_result_store::ToolResultStore;
use crate::runtime_loop::{AskUserPayload, LoopContext, StepRecord, ToolObservation};

/// State for deferred tool schema loading.
///
/// When the total estimated schema tokens exceed the configured threshold, non-core
/// tools are withheld from the LLM until the model explicitly loads them via
/// the `tool_search` pseudo-tool. Once loaded, a tool stays loaded for the
/// remainder of the session (monotonic — no unloading).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DeferredToolState {
	/// Tool names that are deferred (schema not in the current LLM request).
	pub deferred_names: Vec<String>,
	/// Tool names that were deferred but have been loaded via `tool_search`.
	/// Monotonic — once loaded a tool is never removed from this list.
	pub loaded_names: Vec<String>,
}

/// Cached serialized-stable snapshot of the tool schema for a session.
///
/// Held inside `LoopState` so the per-turn refresh can reuse the same
/// `Vec<ToolDefinition>` whenever no explicit schema-dirty event has fired —
/// the provider adapter then sees byte-identical tool blocks across turns,
/// which is the necessary condition for Anthropic `cache_read_input_tokens`
/// and OpenAI `cached_tokens` to accumulate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrozenToolSchema {
	pub definitions: Vec<ToolDefinition>,
}

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
	/// Depth counter for sub-agent nesting. 0 = top-level agent, 1 = sub-agent.
	/// Sub-agents are not permitted to spawn further sub-agents (max depth = 1).
	#[serde(default)]
	pub sub_agent_depth: u32,
	/// Tool names that must never appear in `visible_tools`, even after refresh.
	/// Used by sub-agents to enforce `SubAgentConfig::disallowed_tools`.
	#[serde(default)]
	pub disallowed_tools: Vec<String>,
	/// Run-scoped calibration state for the byte-based prompt token
	/// estimator. Updated after each successful LLM call from the provider's
	/// reported `usage.prompt_tokens` so subsequent estimates converge on
	/// the real token cost. Defaults to an uncalibrated (scale=1.0) state
	/// for backwards compatibility with serialized snapshots.
	#[serde(default)]
	pub estimator_calibration: EstimatorCalibration,
	/// Provider-reported `usage.prompt_tokens` captured right after the
	/// most recent successful call. Authoritative for everything that was
	/// in that request; `None` on cold-start or after any state that
	/// invalidates the previous commit (compaction, tool schema change).
	///
	/// Not serialized — a restored-from-checkpoint loop has its message
	/// buffer rebuilt from scratch and its `frozen_tool_schema` cleared,
	/// so the pre-checkpoint baseline no longer corresponds to the
	/// post-restore buffer layout. Starting fresh (None) makes the first
	/// post-restore turn fall back to the whole-history estimate, and the
	/// baseline re-populates once that turn's provider response lands.
	#[serde(skip)]
	pub(crate) last_observed_input_tokens: Option<u64>,
	/// Number of messages in the call that produced
	/// `last_observed_input_tokens`. Together with the snapshot fields
	/// below, these form a [`CommittedBaseline`]; see
	/// [`LoopState::committed_baseline`]. Not serialized, for the same
	/// reason as [`Self::last_observed_input_tokens`].
	#[serde(skip)]
	pub(crate) committed_message_count: usize,
	/// Byte length of the system prompt that was in the committed call.
	/// Used by [`CommittedBaseline::is_valid_for`] to detect when the
	/// dynamic system-prompt surface has shifted between turns (working
	/// directory, memory blocks, or runtime-memory sections updated).
	#[serde(skip)]
	pub(crate) committed_system_prompt_bytes: u64,
	/// Byte length of the serialized tool-schema block that was in the
	/// committed call. Used by [`CommittedBaseline::is_valid_for`] to
	/// detect deferred-tool transitions, plan-mode visibility changes,
	/// or disallowed-tools list mutations between turns.
	#[serde(skip)]
	pub(crate) committed_tool_schema_bytes_len: u64,
	/// Model ID the provider reported on the committed call. A change
	/// means the tokenizer family may differ from what priced
	/// `last_observed_input_tokens`, so the baseline cannot be trusted.
	#[serde(skip)]
	pub(crate) committed_model_id: Option<String>,
	/// Run-scoped counter of consecutive Layer 3 structured-summary
	/// compaction failures. Reset to `0` on any successful LLM-assisted
	/// compaction. Once it reaches
	/// [`MAX_CONSECUTIVE_AUTOCOMPACT_FAILURES`] the runtime stops
	/// attempting further auto-compactions within this run (circuit
	/// breaker); the cmd layer is notified via
	/// `LoopEvent::AutoCompactCircuitBreakerTripped`.
	#[serde(default)]
	pub consecutive_autocompact_failures: u32,
	/// Cached tool schema snapshot. `None` before the first turn and after
	/// any explicit schema-dirty event; `Some` while the session is in a
	/// stable window where subsequent turns should reuse the same bytes.
	///
	/// Never serialized as part of long-lived state — the `#[serde(skip)]`
	/// attribute keeps it out of snapshot round-trips, which means cache
	/// windows do not survive checkpoint reloads (intentional: a freshly
	/// loaded loop rebuilds the schema once on the first turn, restoring
	/// the freeze invariant thereafter).
	#[serde(skip)]
	pub frozen_tool_schema: Option<FrozenToolSchema>,
	/// Explicit schema-dirty flag. Set to `true` on construction so the
	/// first turn always rebuilds. Set back to `true` only via the
	/// enumerated dirty events (see [`LoopState::mark_tool_schema_dirty`]).
	#[serde(skip)]
	pub tool_schema_dirty: bool,
	/// Last-observed plan-mode state, used by the runtime refresh path to
	/// detect plan-mode transitions and mark the schema dirty exactly
	/// once per transition. `None` before the first refresh.
	#[serde(skip)]
	pub observed_plan_mode: Option<bool>,
	/// Session-scoped cache break detector. Tracks the prompt-state
	/// fingerprint and `cache_read_input_tokens` baseline across turns.
	/// Intentionally not serialized — a freshly restored loop starts with
	/// no baseline (first turn after restore → skip detection).
	#[serde(skip)]
	#[allow(private_interfaces)]
	pub(crate) cache_break_detector: CacheBreakDetector,
	/// Tools that have been deferred (schema not sent to LLM until loaded via
	/// `tool_search`). `None` means deferred mode is not active (tool schemas
	/// fit within threshold). Not serialized — a freshly restored loop
	/// re-evaluates the threshold on the first turn.
	#[serde(skip)]
	pub deferred_tools: Option<DeferredToolState>,
	/// Per-run tool result disk persistence store.
	/// Tracks content replacement state for cache byte stability.
	/// Not serialized — a freshly restored loop starts with an empty store.
	#[serde(skip)]
	pub(crate) tool_result_store: ToolResultStore,
	/// Per-run flag set on the first Layer 2 lookup attempt this run,
	/// regardless of whether the backend returned a summary. Subsequent
	/// mid-water triggers in the same run therefore skip the memory query
	/// and fall back to Layer 1 (mechanical collapse).
	///
	/// Two invariants motivate this:
	///
	/// - **At-most-one Layer 2 per run.** A stored compact summary is
	///   end-of-run material; re-splicing the same frozen digest on every
	///   subsequent mid-water trigger over-represents the older content
	///   relative to each turn's fresh material without adding new signal.
	/// - **Cache the miss.** `write_back_compact_summaries` only persists a
	///   summary at end-of-run, so a lookup that misses now will still miss
	///   on the next trigger within the same run — repeated queries only
	///   add backend latency. Setting the flag on *attempt* (not only on a
	///   Layer 2 outcome) caches the miss for the remainder of the run.
	///
	/// Persisted across checkpoint round-trips via `#[serde(default)]` so
	/// the invariant holds even when a run is paused and resumed.
	#[serde(default)]
	pub(crate) layer2_lookup_attempted_this_run: bool,
}

/// Maximum allowed consecutive Layer 3 structured-summary failures before
/// the per-run circuit breaker trips and further compaction attempts are
/// short-circuited to mechanical fallback within the same run.
pub const MAX_CONSECUTIVE_AUTOCOMPACT_FAILURES: u32 = 3;

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
			sub_agent_depth: 0,
			disallowed_tools: Vec::new(),
			estimator_calibration: EstimatorCalibration::default(),
			last_observed_input_tokens: None,
			committed_message_count: 0,
			committed_system_prompt_bytes: 0,
			committed_tool_schema_bytes_len: 0,
			committed_model_id: None,
			consecutive_autocompact_failures: 0,
			frozen_tool_schema: None,
			tool_schema_dirty: true,
			observed_plan_mode: None,
			cache_break_detector: CacheBreakDetector::default(),
			deferred_tools: None,
			tool_result_store: ToolResultStore::default(),
			layer2_lookup_attempted_this_run: false,
		}
	}

	/// Returns `true` when the run-scoped auto-compact circuit breaker has
	/// tripped. Callers should short-circuit further LLM-assisted
	/// compaction attempts and fall back to mechanical compaction for the
	/// remainder of this run.
	pub fn autocompact_circuit_breaker_tripped(&self) -> bool {
		self.consecutive_autocompact_failures >= MAX_CONSECUTIVE_AUTOCOMPACT_FAILURES
	}

	/// Record a Layer 3 auto-compact success: reset the consecutive
	/// failure counter. Calling this on a tripped circuit breaker will
	/// untrip it, but the runtime's default wiring never retries after a
	/// trip within the same run.
	pub fn note_autocompact_success(&mut self) {
		self.consecutive_autocompact_failures = 0;
	}

	/// Record a Layer 3 auto-compact failure. Returns `true` iff the
	/// circuit breaker transitioned from un-tripped to tripped with this
	/// call, so the caller can emit the one-shot notice event.
	pub fn note_autocompact_failure(&mut self) -> bool {
		let was_tripped = self.autocompact_circuit_breaker_tripped();
		self.consecutive_autocompact_failures =
			self.consecutive_autocompact_failures.saturating_add(1);
		!was_tripped && self.autocompact_circuit_breaker_tripped()
	}

	/// Mark the tool schema as dirty. The enumerated event sources are:
	/// - Plan-mode entry or exit (detected in the runtime refresh path)
	/// - A real mutation of `disallowed_tools` inside the refresh path
	/// - Sub-agent boundary (a fresh `LoopState` is created, so this flag
	///   is already `true` on construction — no explicit call needed there)
	///
	/// Any new dirty source must route through this method so the set of
	/// schema-invalidating events stays auditable in one place.
	///
	/// Also invalidates the committed token baseline: the baseline assumes
	/// that system + tools are identical to what the provider priced on
	/// the last call, and a schema-dirty event breaks that assumption.
	pub fn mark_tool_schema_dirty(&mut self) {
		self.tool_schema_dirty = true;
		self.invalidate_committed_baseline();
	}

	/// Return the current committed-token baseline, if any.
	///
	/// Populated after any successful provider call that reported a
	/// non-zero `usage.prompt_tokens`; cleared on compaction or any event
	/// that changes the non-tail surface (tool schema, system prompt).
	///
	/// Callers must check [`CommittedBaseline::is_valid_for`] with the
	/// current request context before trusting the baseline — a matching
	/// `message_count` is necessary but not sufficient for the baseline
	/// to describe the actual prefix the next call will send.
	pub fn committed_baseline(&self) -> Option<CommittedBaseline> {
		self.last_observed_input_tokens
			.map(|input_tokens| CommittedBaseline {
				input_tokens,
				message_count: self.committed_message_count,
				system_prompt_bytes: self.committed_system_prompt_bytes,
				tool_schema_bytes_len: self.committed_tool_schema_bytes_len,
				model_id: self.committed_model_id.clone(),
			})
	}

	/// Record the real `usage.prompt_tokens` and full committed-prefix
	/// snapshot for the most recent successful call.
	///
	/// `observed_input_tokens` is the provider's authoritative count;
	/// `committed_count` is the number of messages that were in that
	/// request. The three trailing arguments snapshot the other prefix
	/// surfaces the provider tokenized — the system-prompt byte length,
	/// the tool-schema byte length on the wire, and the model ID that
	/// actually served the request. [`CommittedBaseline::is_valid_for`]
	/// compares these against the next turn's context and invalidates
	/// the baseline when any has shifted.
	///
	/// Zero `observed_input_tokens` is a no-op — the provider either did
	/// not report usage or reported a cache-only hit, neither of which
	/// can serve as a baseline for the tail heuristic.
	pub fn record_observed_usage(
		&mut self,
		committed_count: usize,
		observed_input_tokens: u64,
		system_prompt_bytes: usize,
		tool_schema_bytes_len: usize,
		model_id: Option<String>,
	) {
		if observed_input_tokens == 0 {
			return;
		}
		self.last_observed_input_tokens = Some(observed_input_tokens);
		self.committed_message_count = committed_count;
		self.committed_system_prompt_bytes = system_prompt_bytes as u64;
		self.committed_tool_schema_bytes_len = tool_schema_bytes_len as u64;
		self.committed_model_id = model_id;
	}

	/// Clear any cached committed-token baseline and its prefix guards.
	///
	/// Called on compaction (which truncates / rewrites history so the
	/// previous message_count is no longer meaningful) and on schema
	/// changes (which invalidate the system/tools portion of the
	/// provider's previous `prompt_tokens`).
	pub fn invalidate_committed_baseline(&mut self) {
		self.last_observed_input_tokens = None;
		self.committed_message_count = 0;
		self.committed_system_prompt_bytes = 0;
		self.committed_tool_schema_bytes_len = 0;
		self.committed_model_id = None;
	}

	/// Return a `Vec<ToolDefinition>` for the next LLM call, reusing the
	/// cached bytes when no schema-dirty event fired since the last build.
	///
	/// The caller provides `fresh` eagerly — if the freeze is valid the
	/// value is dropped and the cached definitions are cloned instead. In
	/// exchange, the caller need not deal with the borrow-checker
	/// constraints of a lazy closure that reads other fields of `self`.
	pub fn freeze_or_reuse_tool_schema(
		&mut self,
		fresh: Vec<ToolDefinition>,
	) -> Vec<ToolDefinition> {
		if !self.tool_schema_dirty
			&& let Some(frozen) = &self.frozen_tool_schema
		{
			return frozen.definitions.clone();
		}
		self.frozen_tool_schema = Some(FrozenToolSchema {
			definitions: fresh.clone(),
		});
		self.tool_schema_dirty = false;
		fresh
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
	use crate::runtime_loop::CommittedBaseline;
	use crate::runtime_loop::LoopContext;
	use crate::runtime_loop::ask_user::{AskUserPayload, AskUserResumeContract};
	use crate::runtime_loop::cache_break::CacheBreakDetector;
	use crate::runtime_loop::next_step::{NextStepAction, NextStepDecision};
	use crate::runtime_loop::observation::{StepObservation, ToolObservation};
	use crate::runtime_loop::state_update::InterpretedObservation;
	use crate::runtime_loop::step_record::{StepAction, StepRecord};
	use crate::runtime_loop::tool_result_store::ToolResultStore;

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
			tool_calls: None,
			reason: "Need to inspect the file".to_string(),
			final_message: None,
		};

		let decision_ask_user = NextStepDecision {
			action: NextStepAction::AskUser,
			tool_name: None,
			arguments: None,
			tool_calls: None,
			reason: "Need clarification from user".to_string(),
			final_message: Some("Which file did you mean?".to_string()),
		};

		let decision_final = NextStepDecision {
			action: NextStepAction::FinalAnswer,
			tool_name: None,
			arguments: None,
			tool_calls: None,
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
			vec!["inventory.describe".to_string(), "WebSearch".to_string()],
			vec![ResourceSelector::tool("WebSearch".to_string())],
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
			"WebSearch".to_string(),
		];
		state.bound_resources = vec![
			ResourceSelector::tool("inventory.describe".to_string()),
			ResourceSelector::tool("WebSearch".to_string()),
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

		// `frozen_tool_schema`, `tool_schema_dirty`, `observed_plan_mode`,
		// `cache_break_detector`, `deferred_tools`, `tool_result_store`,
		// `last_observed_input_tokens`, and `committed_message_count` are
		// `#[serde(skip)]` — they do not survive a snapshot roundtrip by
		// design (baseline message-positions cannot be trusted against a
		// buffer rebuilt from scratch on restore), so normalize before
		// structural compare.
		state.frozen_tool_schema = None;
		state.tool_schema_dirty = false;
		state.observed_plan_mode = None;
		state.cache_break_detector = CacheBreakDetector::default();
		state.deferred_tools = None;
		state.tool_result_store = ToolResultStore::default();
		state.last_observed_input_tokens = None;
		state.committed_message_count = 0;

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

	#[test]
	fn layer2_lookup_attempted_this_run_roundtrips_through_checkpoint_serde() {
		// Lock the `#[serde(default)]` contract on
		// `layer2_lookup_attempted_this_run`: the flag must survive a JSON
		// round-trip so a run that already attempted the Layer 2 lookup
		// before a checkpoint does not re-attempt it after restore (which
		// would break both the one-Layer-2-per-run invariant and the
		// cache-the-miss invariant).
		let mut state = LoopState::new("loop-layer2-flag", &loop_context());
		assert!(
			!state.layer2_lookup_attempted_this_run,
			"fresh LoopState must start with the flag cleared",
		);
		state.layer2_lookup_attempted_this_run = true;
		let json = serde_json::to_string(&state).expect("serialize");
		let restored: LoopState = serde_json::from_str(&json).expect("deserialize");
		assert!(
			restored.layer2_lookup_attempted_this_run,
			"flag must survive a JSON round-trip (persisted across checkpoints)",
		);
	}

	#[test]
	fn committed_token_baseline_does_not_survive_checkpoint_roundtrip() {
		// Lock the `#[serde(skip)]` contract on `last_observed_input_tokens`
		// and `committed_message_count`: these fields refer to positions in
		// the pre-checkpoint message buffer, which is rebuilt from scratch
		// on restore. Carrying the old baseline would cause the next
		// pre-flight estimate to trust stale message counts against the
		// fresh buffer. The first post-restore call must fall back to the
		// whole-history estimate.
		let mut state = LoopState::new("loop-baseline-skip", &loop_context());
		state.record_observed_usage(7, 2048, 0, 0, None);
		assert_eq!(state.last_observed_input_tokens, Some(2048));
		assert_eq!(state.committed_message_count, 7);
		assert!(state.committed_baseline().is_some());

		let json = serde_json::to_string(&state).expect("serialize");
		let restored: LoopState = serde_json::from_str(&json).expect("deserialize");

		assert!(
			restored.last_observed_input_tokens.is_none(),
			"baseline must reset to None on restore so the next call \
			 falls back to whole-history estimate",
		);
		assert_eq!(restored.committed_message_count, 0);
		assert!(restored.committed_baseline().is_none());
	}

	#[test]
	fn mark_tool_schema_dirty_invalidates_committed_baseline() {
		// The schema-dirty signal invalidates both the frozen tool schema
		// and the committed baseline together — the baseline assumes the
		// committed system + tools surface is stable, and a schema-dirty
		// event explicitly breaks that assumption.
		let mut state = LoopState::new("loop-dirty-reset", &loop_context());
		state.record_observed_usage(5, 1500, 0, 0, None);
		assert!(state.committed_baseline().is_some());
		state.tool_schema_dirty = false;

		state.mark_tool_schema_dirty();

		assert!(state.tool_schema_dirty);
		assert!(
			state.committed_baseline().is_none(),
			"schema-dirty must clear the baseline so the next estimate \
			 re-counts the full request",
		);
	}

	#[test]
	fn invalidate_committed_baseline_is_idempotent() {
		// Calling invalidate on an already-empty baseline must be a no-op
		// (not a panic, not a value change). Multiple compaction hooks can
		// fire per turn so the helper must tolerate repeated calls.
		let mut state = LoopState::new("loop-invalidate-idempotent", &loop_context());
		state.invalidate_committed_baseline();
		state.invalidate_committed_baseline();
		assert!(state.committed_baseline().is_none());

		state.record_observed_usage(3, 900, 0, 0, None);
		state.invalidate_committed_baseline();
		state.invalidate_committed_baseline();
		assert!(state.committed_baseline().is_none());
	}

	#[test]
	fn committed_baseline_is_valid_for_rejects_mismatch() {
		// All three prefix guards must match before the baseline can be
		// trusted. Each mismatch scenario below exercises one guard in
		// isolation; regression for the reviewer-flagged scenarios where
		// dynamic system-prompt content, deferred-tool transitions, or
		// a model override swap silently reused stale `input_tokens`.
		let mut state = LoopState::new("loop-baseline-guard", &loop_context());
		state.record_observed_usage(5, 1500, 2048, 1024, Some("gpt-5.4".to_string()));
		let baseline = state
			.committed_baseline()
			.expect("baseline populated after record_observed_usage");

		// Exact match — baseline is valid.
		assert!(baseline.is_valid_for(2048, 1024, Some("gpt-5.4")));

		// System prompt byte length drifted (e.g. working-directory block grew).
		assert!(!baseline.is_valid_for(2100, 1024, Some("gpt-5.4")));

		// Tool-schema byte length drifted (e.g. new tool loaded via tool_search).
		assert!(!baseline.is_valid_for(2048, 1100, Some("gpt-5.4")));

		// Model changed — tokenizer family may differ, input_tokens figure
		// cannot be trusted even if the text surfaces are identical.
		assert!(!baseline.is_valid_for(2048, 1024, Some("claude-sonnet-4-6")));
		assert!(!baseline.is_valid_for(2048, 1024, None));
	}

	#[test]
	fn committed_baseline_is_valid_for_rejects_zero_input_tokens() {
		// A default-constructed baseline (all zeros) must never be
		// considered valid, even if the caller happens to pass matching
		// zero byte-lengths and a matching None model. The `input_tokens`
		// guard is the minimum bar.
		let baseline = CommittedBaseline::default();
		assert!(!baseline.is_valid_for(0, 0, None));
	}

	#[test]
	fn record_observed_usage_stores_all_prefix_guards() {
		// Pin the contract that `record_observed_usage` captures all three
		// prefix-surface snapshots, so [`LoopState::committed_baseline`]
		// returns them alongside `input_tokens`.
		let mut state = LoopState::new("loop-record-guards", &loop_context());
		state.record_observed_usage(6, 1800, 3072, 896, Some("gpt-5.4".to_string()));
		let b = state.committed_baseline().expect("populated");
		assert_eq!(b.input_tokens, 1800);
		assert_eq!(b.message_count, 6);
		assert_eq!(b.system_prompt_bytes, 3072);
		assert_eq!(b.tool_schema_bytes_len, 896);
		assert_eq!(b.model_id.as_deref(), Some("gpt-5.4"));
	}

	#[test]
	fn invalidate_committed_baseline_clears_all_prefix_guards() {
		// After invalidation, every prefix field returns to its default so
		// a subsequent `record_observed_usage` starts fresh.
		let mut state = LoopState::new("loop-invalidate-guards", &loop_context());
		state.record_observed_usage(6, 1800, 3072, 896, Some("gpt-5.4".to_string()));
		state.invalidate_committed_baseline();
		assert!(state.committed_baseline().is_none());
		assert_eq!(state.committed_system_prompt_bytes, 0);
		assert_eq!(state.committed_tool_schema_bytes_len, 0);
		assert!(state.committed_model_id.is_none());
	}

	#[test]
	fn record_observed_usage_ignores_zero_input_tokens() {
		// Guard the "real provider usage zero" branch: a cache-only hit or
		// a provider that failed to report usage must not be treated as a
		// valid baseline. The existing baseline (if any) stays untouched,
		// and an absent baseline stays absent.
		let mut state = LoopState::new("loop-zero-usage", &loop_context());
		state.record_observed_usage(4, 0, 0, 0, None);
		assert!(state.committed_baseline().is_none());

		state.record_observed_usage(4, 1200, 0, 0, None);
		let original = state.committed_baseline();
		state.record_observed_usage(8, 0, 0, 0, None);
		assert_eq!(state.committed_baseline(), original);
	}

	#[test]
	fn tool_result_store_drops_entries_across_checkpoint_roundtrip() {
		// Exercise the documented `#[serde(skip)]` contract on
		// `LoopState.tool_result_store`: entries registered pre-checkpoint
		// must not survive deserialization, and the restored state must
		// behave correctly for both `advance_turn` and fresh `register` calls.

		let mut state = LoopState::new("loop-restore", &loop_context());

		// Register a large tool result so ToolResultStore enters the preview
		// persistence path (content > PREVIEW_SIZE bytes).
		let large = "X".repeat(5_000);
		let (_content, is_preview) =
			state
				.tool_result_store
				.register("tool-pre", &large, "run-restore");
		assert!(is_preview, "large content should engage preview path");
		assert!(
			state.tool_result_store.get_preview("tool-pre").is_none(),
			"fresh state is not yet a preview for get_preview",
		);
		state.tool_result_store.advance_turn(); // Fresh -> Frozen
		assert!(
			state.tool_result_store.get_preview("tool-pre").is_some(),
			"after advance_turn entry should be Frozen and visible",
		);

		// Checkpoint round-trip.
		let json = serde_json::to_string(&state).expect("serialize");
		let restored: LoopState = serde_json::from_str(&json).expect("deserialize");

		// 1. The store is empty after deserialize.
		assert_eq!(
			restored.tool_result_store,
			ToolResultStore::default(),
			"restored store must match default (entries dropped)",
		);
		assert!(
			restored.tool_result_store.get_preview("tool-pre").is_none(),
			"pre-restore ids must not resolve in the restored store",
		);

		// 2. advance_turn on the restored store is a no-op and does not panic.
		let mut restored = restored;
		restored.tool_result_store.advance_turn();
		assert_eq!(
			restored.tool_result_store,
			ToolResultStore::default(),
			"advance_turn on empty restored store must remain empty",
		);

		// 3. Fresh register() calls interact correctly with the restored state.
		//    Re-registering the same tool_use_id that pre-existed before the
		//    checkpoint must be treated as a brand-new entry (no aliasing
		//    with pre-restore state), and a new distinct id must coexist.
		let (_c1, p1) = restored
			.tool_result_store
			.register("tool-pre", &large, "run-restore");
		assert!(
			p1,
			"re-registering after restore should re-engage preview path"
		);
		let new_payload = "Y".repeat(5_000);
		let (_c2, p2) =
			restored
				.tool_result_store
				.register("tool-post", &new_payload, "run-restore");
		assert!(p2, "new id should engage preview path");
		restored.tool_result_store.advance_turn();
		assert!(
			restored.tool_result_store.get_preview("tool-pre").is_some(),
			"post-restore re-registered id should be visible after advance_turn",
		);
		assert!(
			restored
				.tool_result_store
				.get_preview("tool-post")
				.is_some(),
			"newly registered id should coexist",
		);
	}

	#[test]
	fn autocompact_breaker_starts_untripped() {
		let state = LoopState::new("loop-1", &loop_context());

		assert_eq!(state.consecutive_autocompact_failures, 0);
		assert!(!state.autocompact_circuit_breaker_tripped());
	}

	#[test]
	fn autocompact_breaker_trips_after_three_failures() {
		let mut state = LoopState::new("loop-1", &loop_context());

		assert!(!state.note_autocompact_failure());
		assert!(!state.note_autocompact_failure());
		assert!(
			state.note_autocompact_failure(),
			"third failure must be reported as the transition edge"
		);
		assert!(state.autocompact_circuit_breaker_tripped());
		assert_eq!(state.consecutive_autocompact_failures, 3);
	}

	#[test]
	fn autocompact_failure_is_edge_triggered() {
		let mut state = LoopState::new("loop-1", &loop_context());

		state.note_autocompact_failure();
		state.note_autocompact_failure();
		state.note_autocompact_failure();
		// Already tripped → further failures do NOT re-emit the transition.
		assert!(!state.note_autocompact_failure());
		assert!(state.autocompact_circuit_breaker_tripped());
	}

	#[test]
	fn autocompact_success_resets_counter() {
		let mut state = LoopState::new("loop-1", &loop_context());

		state.note_autocompact_failure();
		state.note_autocompact_failure();
		state.note_autocompact_success();
		assert_eq!(state.consecutive_autocompact_failures, 0);
		assert!(!state.autocompact_circuit_breaker_tripped());
		// Counter must be fresh after reset; two more failures should not trip.
		state.note_autocompact_failure();
		assert!(!state.note_autocompact_failure());
		assert!(!state.autocompact_circuit_breaker_tripped());
	}

	fn tool_def(name: &str) -> roku_plugin_llm::ToolDefinition {
		roku_plugin_llm::ToolDefinition {
			name: name.to_string(),
			description: format!("desc for {name}"),
			parameters: json!({"type": "object", "properties": {}}),
		}
	}

	#[test]
	fn new_loop_state_starts_with_dirty_tool_schema_and_no_frozen_snapshot() {
		let state = LoopState::new("loop-1", &loop_context());

		assert!(state.tool_schema_dirty);
		assert!(state.frozen_tool_schema.is_none());
		assert_eq!(state.observed_plan_mode, None);
	}

	#[test]
	fn freeze_caches_first_build_and_returns_cached_when_clean() {
		let mut state = LoopState::new("loop-1", &loop_context());
		let first = vec![tool_def("Read"), tool_def("Grep")];

		let returned_first = state.freeze_or_reuse_tool_schema(first.clone());
		assert_eq!(returned_first, first);
		assert!(!state.tool_schema_dirty);
		assert!(state.frozen_tool_schema.is_some());

		// A later call with completely different `fresh` must still return
		// the cached bytes as long as no dirty event fired in between.
		let unrelated = vec![tool_def("Bash")];
		let returned_second = state.freeze_or_reuse_tool_schema(unrelated);
		assert_eq!(
			returned_second, first,
			"clean turn must reuse the frozen schema, not the fresh input"
		);
	}

	#[test]
	fn mark_tool_schema_dirty_forces_rebuild_on_next_call() {
		let mut state = LoopState::new("loop-1", &loop_context());
		let first = vec![tool_def("Read")];
		state.freeze_or_reuse_tool_schema(first);

		state.mark_tool_schema_dirty();
		assert!(state.tool_schema_dirty);

		let second = vec![tool_def("Read"), tool_def("Edit")];
		let returned = state.freeze_or_reuse_tool_schema(second.clone());
		assert_eq!(returned, second);
		assert!(!state.tool_schema_dirty);
	}

	#[test]
	fn tool_schema_dirty_flag_predicts_rebuild_before_freeze_call() {
		// Pin the observable invariant that the runtime emits a
		// `ToolSchemaFrozen` event against: reading `tool_schema_dirty` *before*
		// calling `freeze_or_reuse_tool_schema` gives the correct `rebuilt`
		// signal. If this relationship is ever broken (e.g. if freeze is
		// refactored to clear `tool_schema_dirty` at entry), the event's
		// `rebuilt` field will silently go wrong and the test catches it.
		let mut state = LoopState::new("loop-1", &loop_context());
		let defs = vec![tool_def("Read")];

		// Turn 1: fresh state -> dirty is true -> freeze rebuilds.
		let pre_call_dirty = state.tool_schema_dirty;
		assert!(
			pre_call_dirty,
			"new LoopState must expose dirty=true so runtime emits rebuilt=true"
		);
		state.freeze_or_reuse_tool_schema(defs.clone());

		// Turn 2: clean state -> dirty is false -> freeze reuses.
		let pre_call_dirty = state.tool_schema_dirty;
		assert!(
			!pre_call_dirty,
			"after a successful freeze the flag must expose dirty=false so runtime emits rebuilt=false"
		);
		state.freeze_or_reuse_tool_schema(defs.clone());

		// Turn 3: mark dirty -> dirty is true -> freeze rebuilds.
		state.mark_tool_schema_dirty();
		let pre_call_dirty = state.tool_schema_dirty;
		assert!(
			pre_call_dirty,
			"mark_tool_schema_dirty must re-expose dirty=true before the next freeze"
		);
		state.freeze_or_reuse_tool_schema(defs);
	}

	#[test]
	fn rebuild_predicate_accounts_for_checkpoint_resume_defaults() {
		// `frozen_tool_schema` and `tool_schema_dirty` are `#[serde(skip)]`,
		// so a post-resume `LoopState` arrives with `frozen = None` and
		// `dirty = false`. The runtime emits a `ToolSchemaFrozen { rebuilt }`
		// event whose boolean is derived BEFORE calling
		// `freeze_or_reuse_tool_schema`; it must correctly predict that the
		// freeze will rebuild. Reading `tool_schema_dirty` alone would say
		// "no rebuild" in this state, but the freeze actually rebuilds
		// because `frozen_tool_schema` is absent.
		let mut state = LoopState::new("loop-1", &loop_context());
		// Simulate a post-resume snapshot: both fields at their
		// `#[serde(skip)]` defaults.
		state.tool_schema_dirty = false;
		state.frozen_tool_schema = None;

		let predicted_rebuild = state.tool_schema_dirty || state.frozen_tool_schema.is_none();
		assert!(
			predicted_rebuild,
			"predicate must predict rebuild when the snapshot is absent, \
			 even if dirty=false (post-resume shape)"
		);

		let defs = vec![tool_def("Read")];
		state.freeze_or_reuse_tool_schema(defs.clone());
		assert!(
			state.frozen_tool_schema.is_some(),
			"freeze must have actually rebuilt the snapshot"
		);
	}

	#[test]
	fn freeze_survives_across_multiple_clean_turns_byte_identical() {
		let mut state = LoopState::new("loop-1", &loop_context());
		let canonical = vec![tool_def("Read"), tool_def("Grep"), tool_def("Bash")];
		let expected_bytes = serde_json::to_vec(&canonical).expect("tool defs serialize");

		// First turn: establishes freeze.
		state.freeze_or_reuse_tool_schema(canonical.clone());

		// Subsequent clean turns: must produce byte-identical serialization
		// regardless of what `fresh` the caller passes in.
		for scramble in 0..5 {
			let mutated = if scramble % 2 == 0 {
				vec![]
			} else {
				vec![tool_def("Mutated")]
			};
			let returned = state.freeze_or_reuse_tool_schema(mutated);
			let bytes = serde_json::to_vec(&returned).expect("tool defs serialize");
			assert_eq!(
				bytes, expected_bytes,
				"clean turn {scramble} must emit byte-identical tool schema"
			);
		}
	}
}
