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

use roku_plugin_llm::{GenerationRequest, LlmRouter, Message, RiskTier};

/// Maximum time to wait for an LLM compact summarization call before falling
/// back to mechanical summarization. LLM compaction routinely takes 2-3 minutes
/// for large contexts; only truly hung calls (>5 min) should be timed out.
const COMPACT_LLM_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

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

/// Configuration for history compaction.
pub struct CompactConfig {
	pub retain_tail_steps: usize,
	pub working_summary_max_chars: usize,
	pub llm_expected_output_tokens: u64,
	pub llm_budget_tokens_remaining: u64,
	pub llm_budget_cost_remaining_usd: f64,
}

impl Default for CompactConfig {
	fn default() -> Self {
		Self {
			retain_tail_steps: 8,
			working_summary_max_chars: 4_000,
			llm_expected_output_tokens: 512_u64,
			llm_budget_tokens_remaining: 10_000,
			llm_budget_cost_remaining_usd: 0.50,
		}
	}
}

/// Generate a deterministic digest of discarded history steps.
///
/// Each step is rendered as a one-line summary: tool name, reason, and
/// observation outcome (truncated to 200 chars). No LLM call — this is
/// the lightweight first-layer compact.
pub fn summarize_discarded_steps(steps: &[super::StepRecord]) -> String {
	let mut lines = vec![format!(
		"[Compact summary — {} steps discarded]",
		steps.len()
	)];
	for step in steps {
		let tool = step.tool_name.as_deref().unwrap_or("unknown");
		let reason = truncate(&step.decision_reason, 80);
		let outcome = step_outcome_text(step, 100);
		lines.push(format!(
			"Step {}: {} — {} — {}",
			step.step_index, tool, reason, outcome
		));
	}
	lines.join("\n")
}

/// Render a step's observation outcome as a short text string.
fn step_outcome_text(step: &super::StepRecord, max_detail_chars: usize) -> String {
	step.observation
		.as_ref()
		.map(|obs| match obs {
			super::StepObservation::Tool(t) => {
				let status = if t.ok { "ok" } else { "error" };
				let detail = truncate(&t.message, max_detail_chars);
				if detail.is_empty() {
					status.to_string()
				} else {
					format!("{status}: {detail}")
				}
			}
			super::StepObservation::AskUser { final_message } => {
				format!("ask_user: {}", truncate(final_message, max_detail_chars))
			}
			super::StepObservation::FinalMessage { final_message } => {
				format!("final: {}", truncate(final_message, max_detail_chars))
			}
		})
		.unwrap_or_else(|| "no observation".to_string())
}

/// Compact the loop history by truncating old steps and populating working_summary.
///
/// 1. If history is short enough, no-op.
/// 2. Split into discarded (older) and retained (tail).
/// 3. Generate deterministic summary from discarded steps.
/// 4. Prepend a compact boundary record to retained history.
/// 5. Update working_summary (prepend new summary, cap total length).
pub fn compact_history(state: &mut LoopState, config: &CompactConfig) {
	if state.history.len() <= config.retain_tail_steps {
		return;
	}

	let split_point = state.history.len() - config.retain_tail_steps;
	let discarded: Vec<_> = state.history.drain(..split_point).collect();
	let summary = summarize_discarded_steps(&discarded);

	apply_compact_state(state, &discarded, summary, config);
}

/// Compact with LLM-assisted summarization, falling back to mechanical compact on failure.
///
/// Returns `true` if LLM summarization succeeded, `false` if it fell back to mechanical.
pub async fn compact_history_with_llm(
	state: &mut LoopState,
	config: &CompactConfig,
	router: &LlmRouter,
) -> bool {
	if state.history.len() <= config.retain_tail_steps {
		return false;
	}

	let split_point = state.history.len() - config.retain_tail_steps;
	let discarded: Vec<_> = state.history.drain(..split_point).collect();

	// Build a concise JSON representation of discarded steps for the LLM.
	let step_summaries: Vec<serde_json::Value> = discarded
		.iter()
		.map(|step| {
			let tool = step.tool_name.as_deref().unwrap_or("unknown");
			serde_json::json!({
				"step": step.step_index,
				"tool": tool,
				"reason": truncate(&step.decision_reason, 100),
				"outcome": step_outcome_text(step, 150),
			})
		})
		.collect();

	let steps_json = serde_json::to_string(&step_summaries).unwrap_or_default();
	let prompt = format!(
		"You are summarizing {} agent execution steps that are being compacted from history.\n\
		 Produce a concise plain-text summary that captures:\n\
		 - Key actions taken and their outcomes\n\
		 - Any errors encountered\n\
		 - Important state changes or discoveries\n\n\
		 Keep the summary under {} characters. Output plain text only.\n\n\
		 Steps:\n{}",
		discarded.len(),
		config.working_summary_max_chars,
		steps_json
	);

	let llm_result = tokio::time::timeout(
		COMPACT_LLM_TIMEOUT,
		router.generate(&GenerationRequest {
			system_prompt: Some(
				"You are a concise summarizer for an agent runtime's execution history."
					.to_string(),
			),
			prompt,
			messages: None,
			expected_output_tokens: config.llm_expected_output_tokens,
			risk_tier: RiskTier::Low,
			preferred_provider: None,
			budget_tokens_remaining: config.llm_budget_tokens_remaining,
			budget_cost_remaining_usd: config.llm_budget_cost_remaining_usd,
			tools: None,
			model_override: None,
			thinking_effort: None,
		}),
	)
	.await;

	let summary = match llm_result {
		Ok(Ok(response)) if !response.output.trim().is_empty() => response.output,
		_ => {
			// Fallback to mechanical summarization on error or timeout.
			let mechanical = summarize_discarded_steps(&discarded);
			apply_compact_state(state, &discarded, mechanical, config);
			return false;
		}
	};

	apply_compact_state(state, &discarded, summary, config);
	true
}

/// Apply compaction state updates after summarization (shared by both paths).
fn apply_compact_state(
	state: &mut LoopState,
	discarded: &[super::StepRecord],
	summary: String,
	config: &CompactConfig,
) {
	let summary_preview = truncate(&summary, 200);
	let boundary = super::StepRecord::compact_boundary(
		state.step_index,
		discarded.len(),
		&summary_preview,
		state.remaining_step_budget,
		state.remaining_recovery_budget,
		&state.working_directory,
	);

	state.history.insert(0, boundary);

	if state.working_summary.is_empty() {
		state.working_summary = summary;
	} else {
		state.working_summary = format!("{}\n\n{}", summary, state.working_summary);
	}

	if state.working_summary.len() > config.working_summary_max_chars {
		let boundary = state
			.working_summary
			.floor_char_boundary(config.working_summary_max_chars);
		state.working_summary.truncate(boundary);
	}
}

fn truncate(text: &str, max_chars: usize) -> String {
	if text.len() <= max_chars {
		text.to_string()
	} else {
		format!("{}…", &text[..text.floor_char_boundary(max_chars)])
	}
}

// ---------------------------------------------------------------------------
// Message-level compaction (operates on Vec<Message>)
// ---------------------------------------------------------------------------

/// Rough token estimate for a conversation message list.
pub fn estimate_message_tokens(messages: &[Message]) -> u64 {
	let chars: u64 = messages
		.iter()
		.map(|m| match m {
			Message::User { content } => content.len() as u64,
			Message::Assistant { text, tool_calls } => {
				text.len() as u64
					+ tool_calls
						.iter()
						.map(|tc| tc.name.len() as u64 + tc.arguments.to_string().len() as u64)
						.sum::<u64>()
			}
			Message::ToolResult { content, .. } => content.len() as u64,
		})
		.sum();
	chars / 4
}

/// Truncate a tool result that exceeds `max_chars`, keeping head + tail + a note.
pub fn truncate_tool_result(content: &str, max_chars: usize) -> String {
	if content.len() <= max_chars {
		return content.to_string();
	}
	let keep = max_chars.saturating_sub(60) / 2;
	let head = &content[..content.floor_char_boundary(keep)];
	let tail_start = content.len().saturating_sub(keep);
	let tail = &content[content.ceil_char_boundary(tail_start)..];
	let omitted = content.len() - head.len() - tail.len();
	format!("{head}\n\n[...{omitted} bytes omitted...]\n\n{tail}")
}

/// Truncate any oversized tool results in-place.
pub fn truncate_large_tool_results(messages: &mut [Message], max_chars: usize) {
	for msg in messages.iter_mut() {
		if let Message::ToolResult { content, .. } = msg
			&& content.len() > max_chars
		{
			*content = truncate_tool_result(content, max_chars);
		}
	}
}

/// Find a valid drain boundary that does not orphan ToolResult messages.
///
/// `split` is the initial candidate for the first retained message index.
/// If `messages[split]` is a `ToolResult`, scan backwards to find the
/// originating Assistant message so the retained tail starts cleanly.
/// Returns the adjusted split index (always >= 1).
fn adjust_split_for_tool_pairs(messages: &[Message], mut split: usize) -> usize {
	while split > 1 && split < messages.len() {
		if matches!(messages[split], Message::ToolResult { .. }) {
			split -= 1;
		} else {
			break;
		}
	}
	split
}

/// Compact conversation messages by replacing old messages with a summary.
///
/// Preserves the first message (system context / initial user message) and the
/// most recent `retain_tail` messages. Messages in between are replaced with a
/// single summary User message.
pub fn compact_messages(messages: &mut Vec<Message>, retain_tail: usize) {
	if messages.len() <= retain_tail + 1 {
		return;
	}
	let split = messages.len() - retain_tail;
	if split <= 1 {
		return;
	}
	let split = adjust_split_for_tool_pairs(messages, split);
	if split <= 1 {
		return;
	}
	let discarded: Vec<_> = messages.drain(1..split).collect();
	let summary = summarize_discarded_messages(&discarded);
	messages.insert(1, Message::User { content: summary });
}

/// Compact with LLM-assisted summarization of discarded messages.
/// Returns `(compacted, prompt_tokens_used, output_tokens_used)` so callers
/// can include compaction LLM cost in their token usage totals.
pub async fn compact_messages_with_llm(
	messages: &mut Vec<Message>,
	retain_tail: usize,
	router: &LlmRouter,
) -> (bool, u64, u64) {
	if messages.len() <= retain_tail + 1 {
		return (false, 0, 0);
	}
	let split = messages.len() - retain_tail;
	if split <= 1 {
		return (false, 0, 0);
	}
	let split = adjust_split_for_tool_pairs(messages, split);
	if split <= 1 {
		return (false, 0, 0);
	}
	let discarded: Vec<_> = messages.drain(1..split).collect();
	let mechanical = summarize_discarded_messages(&discarded);

	let prompt = format!(
		"Summarize this conversation excerpt concisely. Preserve key facts, \
		 tool results, and decisions. Output plain text only.\n\n{}",
		mechanical
	);
	let result = tokio::time::timeout(
		COMPACT_LLM_TIMEOUT,
		router.generate(&GenerationRequest {
			system_prompt: Some("You summarize agent conversation history.".to_string()),
			prompt,
			messages: None,
			expected_output_tokens: 512,
			risk_tier: RiskTier::Low,
			preferred_provider: None,
			budget_tokens_remaining: 10_000,
			budget_cost_remaining_usd: 0.50,
			tools: None,
			model_override: None,
			thinking_effort: None,
		}),
	)
	.await;

	let (summary, prompt_tokens, output_tokens) = match result {
		Ok(Ok(r)) if !r.output.trim().is_empty() => (r.output, r.prompt_tokens, r.output_tokens),
		_ => (mechanical, 0, 0),
	};
	messages.insert(
		1,
		Message::User {
			content: format!("[Conversation summary]\n{summary}"),
		},
	);
	(true, prompt_tokens, output_tokens)
}

fn summarize_discarded_messages(messages: &[Message]) -> String {
	let mut lines = vec![format!("[{} messages compacted]", messages.len())];
	for msg in messages {
		match msg {
			Message::User { content } => {
				lines.push(format!("User: {}", truncate(content, 120)));
			}
			Message::Assistant { text, tool_calls } => {
				if !text.is_empty() {
					lines.push(format!("Assistant: {}", truncate(text, 120)));
				}
				for tc in tool_calls {
					lines.push(format!("  → tool_use: {}", tc.name));
				}
			}
			Message::ToolResult {
				tool_use_id,
				content,
				is_error,
			} => {
				let status = if *is_error { "error" } else { "ok" };
				lines.push(format!(
					"ToolResult({}): {} — {}",
					tool_use_id,
					status,
					truncate(content, 100)
				));
			}
		}
	}
	lines.join("\n")
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
	use crate::runtime_config::LoopRuntimeConfig;
	use crate::runtime_loop::observation::ToolObservation;
	use crate::runtime_loop::state_update::InterpretedObservation;
	use crate::runtime_loop::step_record::StepRecord;
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
			visible_tools: vec!["inventory.describe".to_string()],
			bound_resources: Vec::new(),
			history: Vec::new(),
			last_observation: None,
			awaiting_user: None,
			latest_explicit_grounding_fingerprint: String::new(),
			ambiguity_stagnation: None,
			sub_agent_depth: 0,
		}
	}

	fn sample_observation() -> ToolObservation {
		ToolObservation {
			ok: true,
			tool_name: "inventory.describe".to_string(),
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
			visible_tools: vec!["inventory.describe".to_string()],
		};
		StepRecord::tool_call(
			index,
			NextStepDecision {
				action: NextStepAction::CallTool,
				tool_name: Some("inventory.describe".to_string()),
				arguments: Some(json!({"command": "echo hello"})),
				tool_calls: None,
				reason: "Execute the command.".to_string(),
				final_message: None,
			},
			vec!["inventory.describe".to_string()],
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

	// --- US-001: History truncation ---

	#[test]
	fn compact_truncates_10_step_history_to_tail_plus_boundary() {
		let mut state = minimal_loop_state();
		for i in 1..=10 {
			state.record_step(sample_step(i, 10 - i));
		}
		assert_eq!(state.history.len(), 10);

		let config = CompactConfig {
			retain_tail_steps: 4,
			..CompactConfig::default()
		};
		compact_history(&mut state, &config);

		// 4 retained + 1 boundary = 5
		assert_eq!(state.history.len(), 5, "expected boundary + 4 retained");
		assert_eq!(
			state.history[0].action,
			crate::runtime_loop::StepAction::CompactBoundary
		);
		assert_eq!(state.history[1].step_index, 7);
		assert_eq!(state.history[4].step_index, 10);
	}

	#[test]
	fn compact_is_noop_when_history_within_retain_limit() {
		let mut state = minimal_loop_state();
		for i in 1..=3 {
			state.record_step(sample_step(i, 10 - i));
		}
		let before = state.history.len();
		compact_history(&mut state, &CompactConfig::default());
		assert_eq!(state.history.len(), before, "should not truncate");
		assert!(
			state.working_summary.is_empty(),
			"working_summary should stay empty"
		);
	}

	// --- US-002: Working summary population ---

	#[test]
	fn summarize_discarded_steps_format() {
		let steps: Vec<_> = (1..=6).map(|i| sample_step(i, 10 - i)).collect();
		let summary = summarize_discarded_steps(&steps);

		assert!(summary.starts_with("[Compact summary — 6 steps discarded]"));
		assert!(summary.contains("Step 1:"));
		assert!(summary.contains("Step 6:"));
		assert!(summary.contains("inventory.describe"));
		assert!(summary.contains("ok"));
	}

	#[test]
	fn compact_populates_working_summary() {
		let mut state = minimal_loop_state();
		for i in 1..=10 {
			state.record_step(sample_step(i, 10 - i));
		}
		let config = CompactConfig {
			retain_tail_steps: 4,
			..CompactConfig::default()
		};
		compact_history(&mut state, &config);

		assert!(
			state.working_summary.contains("[Compact summary"),
			"working_summary should contain compact digest"
		);
		assert!(
			state.working_summary.contains("6 steps discarded"),
			"should record 6 discarded steps"
		);
	}

	#[test]
	fn compact_prepends_to_existing_working_summary() {
		let mut state = minimal_loop_state();
		state.working_summary = "Previous context from earlier compact.".to_string();
		for i in 1..=10 {
			state.record_step(sample_step(i, 10 - i));
		}
		compact_history(&mut state, &CompactConfig::default());

		assert!(
			state
				.working_summary
				.ends_with("Previous context from earlier compact."),
			"old summary should be preserved at end"
		);
		assert!(
			state.working_summary.starts_with("[Compact summary"),
			"new summary should be prepended"
		);
	}

	#[test]
	fn compact_caps_working_summary_length() {
		let mut state = minimal_loop_state();
		state.working_summary = "X".repeat(3_900);
		for i in 1..=10 {
			state.record_step(sample_step(i, 10 - i));
		}
		let config = CompactConfig {
			retain_tail_steps: 4,
			working_summary_max_chars: 4_000,
			..Default::default()
		};
		compact_history(&mut state, &config);

		assert!(
			state.working_summary.len() <= 4_000,
			"working_summary should be capped at 4000 chars, got {}",
			state.working_summary.len()
		);
	}

	// --- US-003: Compact boundary record ---

	#[test]
	fn compact_boundary_is_first_in_retained_history() {
		let mut state = minimal_loop_state();
		for i in 1..=8 {
			state.record_step(sample_step(i, 10 - i));
		}
		let config = CompactConfig {
			retain_tail_steps: 4,
			..CompactConfig::default()
		};
		compact_history(&mut state, &config);

		let boundary = &state.history[0];
		assert_eq!(
			boundary.action,
			crate::runtime_loop::StepAction::CompactBoundary
		);
		assert_eq!(
			boundary.decision_reason, "4",
			"boundary decision_reason should be the discarded count"
		);
		let raw = boundary.raw_tool_output.as_ref().unwrap();
		assert_eq!(raw["discarded_count"], 4);
	}

	// --- Integration: compact reduces estimated tokens ---

	#[test]
	fn compact_reduces_estimated_tokens_below_threshold() {
		let mut state = minimal_loop_state();
		// Add steps with large tool output to blow up token count
		for i in 1..=8 {
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
				remaining_step_budget: 10 - i,
				remaining_recovery_budget: 2,
				new_working_directory: None,
				visible_tools: vec!["inventory.describe".to_string()],
			};
			let step = StepRecord::tool_call(
				i,
				NextStepDecision {
					action: NextStepAction::CallTool,
					tool_name: Some("inventory.describe".to_string()),
					arguments: Some(json!({"command": "echo hello"})),
					tool_calls: None,
					reason: "Execute.".to_string(),
					final_message: None,
				},
				vec!["inventory.describe".to_string()],
				Vec::new(),
				json!({"ok": true, "output": "X".repeat(100_000)}),
				StepObservation::Tool(observation),
				interpreted,
				Some(50),
				10 - i,
				2,
				"/workspace",
			);
			state.record_step(step);
		}

		let before = estimate_context_tokens(&state);
		let config = CompactConfig {
			retain_tail_steps: 4,
			..CompactConfig::default()
		};
		compact_history(&mut state, &config);
		let after = estimate_context_tokens(&state);

		assert!(
			after < before,
			"compact should reduce token count: {after} < {before}"
		);
	}

	// --- PRD-08 US-004: Multi-compact loop continuity ---

	#[test]
	fn multi_compact_loop_continuity() {
		let mut state = minimal_loop_state();
		state.remaining_step_budget = 20;

		// Use small window so compact triggers easily
		let config = CompactConfig {
			retain_tail_steps: 3,
			working_summary_max_chars: 4_000,
			..Default::default()
		};

		// Phase 1: add 6 steps → compact
		for i in 1..=6 {
			state.record_step(sample_step(i, 20 - i));
		}
		compact_history(&mut state, &config);

		assert_eq!(state.history.len(), 4); // boundary + 3 retained
		assert!(state.working_summary.contains("3 steps discarded"));

		// Phase 2: add 4 more steps → compact again
		for i in 7..=10 {
			state.record_step(sample_step(i, 20 - i));
		}
		let budget_before_compact = state.remaining_step_budget;
		compact_history(&mut state, &config);

		assert_eq!(state.history.len(), 4); // new boundary + 3 retained
		assert_eq!(
			state.remaining_step_budget, budget_before_compact,
			"compact must not alter step budget"
		);

		// Verify both compacts contributed to working_summary
		let compact_count = state.working_summary.matches("[Compact summary").count();
		assert!(
			compact_count >= 2,
			"working_summary should contain summaries from both compacts, found {compact_count}"
		);

		// Verify loop can still continue (step budget > 0)
		assert!(state.remaining_step_budget > 0);
	}

	// --- Message-level compaction tests ---

	#[test]
	fn truncate_tool_result_preserves_short_content() {
		let short = "Hello world";
		assert_eq!(truncate_tool_result(short, 100), short);
	}

	#[test]
	fn truncate_tool_result_keeps_head_and_tail() {
		let long = "A".repeat(1000);
		let result = truncate_tool_result(&long, 200);
		assert!(result.len() < 1000);
		assert!(result.contains("[..."));
		assert!(result.contains("bytes omitted...]"));
		assert!(result.starts_with("AAA"));
		assert!(result.ends_with("AAA"));
	}

	#[test]
	fn truncate_large_tool_results_only_affects_oversized() {
		use roku_plugin_llm::Message;
		let mut messages = vec![
			Message::User {
				content: "hello".to_string(),
			},
			Message::ToolResult {
				tool_use_id: "t1".to_string(),
				content: "short".to_string(),
				is_error: false,
			},
			Message::ToolResult {
				tool_use_id: "t2".to_string(),
				content: "X".repeat(500),
				is_error: false,
			},
		];
		truncate_large_tool_results(&mut messages, 100);
		// Short result unchanged
		if let Message::ToolResult { content, .. } = &messages[1] {
			assert_eq!(content, "short");
		}
		// Long result truncated
		if let Message::ToolResult { content, .. } = &messages[2] {
			assert!(content.len() < 500);
			assert!(content.contains("bytes omitted"));
		}
	}

	#[test]
	fn estimate_message_tokens_nonzero() {
		use roku_plugin_llm::Message;
		let messages = vec![
			Message::User {
				content: "What is 2+2?".to_string(),
			},
			Message::Assistant {
				text: "The answer is 4.".to_string(),
				tool_calls: vec![],
			},
		];
		let tokens = estimate_message_tokens(&messages);
		assert!(tokens > 0);
	}

	#[test]
	fn compact_messages_preserves_recent() {
		use roku_plugin_llm::{Message, ToolCallBlock};
		let mut messages: Vec<Message> = vec![Message::User {
			content: "initial goal".to_string(),
		}];
		// Add 10 more messages
		for i in 0..10 {
			messages.push(Message::Assistant {
				text: format!("response {i}"),
				tool_calls: vec![ToolCallBlock {
					id: format!("tc-{i}"),
					name: "test_tool".to_string(),
					arguments: json!({}),
				}],
			});
			messages.push(Message::ToolResult {
				tool_use_id: format!("tc-{i}"),
				content: format!("result {i}"),
				is_error: false,
			});
		}
		assert_eq!(messages.len(), 21); // 1 initial + 20 (10 pairs)

		compact_messages(&mut messages, 5);

		// The naive split (21 - 5 = 16) lands on a ToolResult, so the boundary
		// adjustment pulls it back to index 15 (the owning Assistant), yielding
		// 6 retained messages instead of 5.
		// Should have: initial + summary + 6 recent = 8
		assert_eq!(messages.len(), 8);
		// First is still the initial message
		if let Message::User { content } = &messages[0] {
			assert_eq!(content, "initial goal");
		}
		// Second is the summary
		if let Message::User { content } = &messages[1] {
			assert!(content.contains("messages compacted"));
		}
		// Third (first retained) must not be a ToolResult
		assert!(
			!matches!(&messages[2], Message::ToolResult { .. }),
			"first retained message must not be a ToolResult"
		);
	}

	#[test]
	fn compact_messages_noop_when_short() {
		use roku_plugin_llm::Message;
		let mut messages = vec![
			Message::User {
				content: "hello".to_string(),
			},
			Message::Assistant {
				text: "hi".to_string(),
				tool_calls: vec![],
			},
		];
		compact_messages(&mut messages, 5);
		assert_eq!(messages.len(), 2); // Unchanged
	}

	#[test]
	fn compact_messages_retain_tail_zero_does_not_panic() {
		use roku_plugin_llm::Message;
		let mut messages = vec![
			Message::User {
				content: "goal".to_string(),
			},
			Message::Assistant {
				text: "answer".to_string(),
				tool_calls: vec![],
			},
			Message::User {
				content: "follow-up".to_string(),
			},
		];
		// retain_tail=0 → split = len = 3. Must not panic.
		compact_messages(&mut messages, 0);
		// All non-anchor messages compacted: anchor + summary.
		assert_eq!(messages.len(), 2);
	}

	#[test]
	fn compact_messages_does_not_orphan_tool_results() {
		use roku_plugin_llm::{Message, ToolCallBlock};
		// Build a message list where the naive split lands on a ToolResult:
		//   [0] User (anchor)
		//   [1] Assistant (old, no tool calls)
		//   [2] User (old reply)
		//   [3] Assistant (with tool_calls) ← must NOT be separated from its results
		//   [4] ToolResult(tc-1)
		//   [5] ToolResult(tc-2)
		//   [6] Assistant (final text)
		//   [7] User (follow-up)
		//
		// With retain_tail = 4: naive split = 8 - 4 = 4.
		// messages[4] is a ToolResult → adjustment walks back to index 3
		// (the owning Assistant) → drain 1..3 (indices 1 and 2).
		// Retained tail starts at index 3 (Assistant with tool_calls) — not a ToolResult.
		let mut messages = vec![
			Message::User {
				content: "initial goal".to_string(),
			},
			Message::Assistant {
				text: "some earlier text".to_string(),
				tool_calls: vec![],
			},
			Message::User {
				content: "old reply".to_string(),
			},
			Message::Assistant {
				text: String::new(),
				tool_calls: vec![
					ToolCallBlock {
						id: "tc-1".to_string(),
						name: "tool_a".to_string(),
						arguments: json!({}),
					},
					ToolCallBlock {
						id: "tc-2".to_string(),
						name: "tool_b".to_string(),
						arguments: json!({}),
					},
				],
			},
			Message::ToolResult {
				tool_use_id: "tc-1".to_string(),
				content: "result a".to_string(),
				is_error: false,
			},
			Message::ToolResult {
				tool_use_id: "tc-2".to_string(),
				content: "result b".to_string(),
				is_error: false,
			},
			Message::Assistant {
				text: "Final answer.".to_string(),
				tool_calls: vec![],
			},
			Message::User {
				content: "follow-up".to_string(),
			},
		];
		assert_eq!(messages.len(), 8);

		compact_messages(&mut messages, 4);

		// Drain was 1..3 (2 messages), then summary inserted at index 1.
		// Layout: [0]=anchor, [1]=summary, [2..6]=retained(5 msgs) → total 7
		assert_eq!(
			messages.len(),
			7,
			"expected anchor + summary + 5 retained messages"
		);

		// The first retained message (index 2) must not be a ToolResult.
		assert!(
			!matches!(&messages[2], Message::ToolResult { .. }),
			"retained tail must not start with a ToolResult; first retained = {:?}",
			&messages[2]
		);
	}
}
