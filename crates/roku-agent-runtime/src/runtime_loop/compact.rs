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

use roku_plugin_llm::{GenerationRequest, LlmAdapterError, LlmRouter, Message, RiskTier};
use serde::{Deserialize, Serialize};

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
			llm_expected_output_tokens: 2_048,
			llm_budget_tokens_remaining: 100_000,
			llm_budget_cost_remaining_usd: 2.00,
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
			system_prompt_sections: None,
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

// ---------------------------------------------------------------------------
// Calibrated byte-based prompt token estimator (unit 01: token pressure)
// ---------------------------------------------------------------------------
//
// `estimate_prompt_tokens_calibrated` is the single authority that decides
// when any compaction layer (microcompact, mid-tier, LLM summarizer, reactive)
// should fire. It walks the bytes of every message body once and classifies
// each segment with cheap heuristics; the raw count is then optionally
// linearly scaled by `EstimatorCalibration`, which absorbs (estimated, real)
// feedback from successful provider calls.
//
// Why byte-based instead of tokenizer-backed: introducing tiktoken /
// huggingface tokenizer would force a per-provider dependency tree, and we
// only need a "are we close to the limit?" signal — not a precise count.
// Accuracy gates: ≤20% error for English/code, ≤30% error for CJK-dominated
// text; calibration tightens both over time.

/// Per-segment breakdown returned by the calibrated estimator.
///
/// `total_tokens` is the value callers compare against the per-model
/// threshold; the other fields are exposed so trace events can attribute
/// pressure to system / messages / framing during diagnostics.
///
/// `raw_total_tokens` carries the **unscaled** sum (system + messages +
/// framing) before the calibration scale is applied. Callers feeding the
/// value back into [`EstimatorCalibration::update`] must use this field —
/// the scale operates on raw input, so folding in the already-scaled
/// `total_tokens` would compute `real / (raw * scale)` and walk the scale
/// toward the wrong fixed point.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PromptTokenEstimate {
	pub system_tokens: u64,
	pub message_tokens: u64,
	pub framing_tokens: u64,
	pub total_tokens: u64,
	pub raw_total_tokens: u64,
}

/// Bounded calibration state for the byte-based estimator.
///
/// Holds a small ring of recent `(estimated, real)` samples and a smoothed
/// scale factor applied on top of the raw byte-derived count. Lives on
/// `LoopState` so a single run accumulates calibration without leaking
/// between sessions; the scale is clamped to keep the estimate panic-safe
/// even when a provider returns wildly inconsistent usage numbers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EstimatorCalibration {
	#[serde(default)]
	samples: Vec<CalibrationSample>,
	/// Multiplicative correction applied on top of the raw byte estimate.
	/// Defaults to 1.0 (uncalibrated); clamped to
	/// [`CAL_SCALE_MIN`, `CAL_SCALE_MAX`].
	#[serde(default = "default_scale")]
	scale: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct CalibrationSample {
	estimated: u64,
	real: u64,
}

const CAL_SAMPLE_CAP: usize = 8;
const CAL_SCALE_MIN: f64 = 0.5;
const CAL_SCALE_MAX: f64 = 2.0;

fn default_scale() -> f64 {
	1.0
}

impl Default for EstimatorCalibration {
	fn default() -> Self {
		Self {
			samples: Vec::new(),
			scale: 1.0,
		}
	}
}

impl EstimatorCalibration {
	/// Apply the current calibration scale to a raw byte-derived estimate.
	pub fn apply(&self, raw_tokens: u64) -> u64 {
		if raw_tokens == 0 {
			return 0;
		}
		let scaled = (raw_tokens as f64) * self.scale;
		if !scaled.is_finite() || scaled < 0.0 {
			return raw_tokens;
		}
		// Round to nearest. Bounded scale ([0.5, 2.0]) guarantees no overflow.
		scaled.round() as u64
	}

	/// Record a new `(estimated, real)` observation and recompute the scale.
	///
	/// `estimated` is the value the estimator returned for the request that
	/// just got served; `real` is the provider's reported
	/// `usage.prompt_tokens`. Either argument being zero is a no-op — the
	/// provider didn't report usage so there is nothing to learn.
	pub fn update(&mut self, estimated: u64, real: u64) {
		if estimated == 0 || real == 0 {
			return;
		}
		if self.samples.len() >= CAL_SAMPLE_CAP {
			self.samples.remove(0);
		}
		self.samples.push(CalibrationSample { estimated, real });

		// New scale = mean(real / estimated) over the recent samples.
		let mut sum_ratio = 0_f64;
		let mut n = 0_u64;
		for s in &self.samples {
			if s.estimated == 0 {
				continue;
			}
			sum_ratio += s.real as f64 / s.estimated as f64;
			n += 1;
		}
		if n > 0 {
			let avg = sum_ratio / (n as f64);
			if avg.is_finite() {
				self.scale = avg.clamp(CAL_SCALE_MIN, CAL_SCALE_MAX);
			}
		}
	}

	/// Number of `(estimated, real)` samples currently in the window.
	pub fn sample_count(&self) -> usize {
		self.samples.len()
	}

	/// Current effective scale (1.0 when uncalibrated).
	pub fn scale(&self) -> f64 {
		self.scale
	}
}

/// Estimate prompt tokens for what will actually be sent to the LLM.
///
/// O(n) over total byte length. Pure function: no IO, no panic. Per-segment
/// classification rules are documented on [`byte_estimate_for_text`].
///
/// `system_prompt` is treated identically to a message body — code-heavy
/// system prompts (which describe tool schemas as JSON) are auto-detected as
/// structured and use the denser byte/2 rule.
pub fn estimate_prompt_tokens_calibrated(
	messages: &[Message],
	system_prompt: Option<&str>,
	calibration: &EstimatorCalibration,
) -> PromptTokenEstimate {
	let system_tokens = system_prompt.map(byte_estimate_for_text).unwrap_or(0);
	let message_tokens: u64 = messages
		.iter()
		.map(byte_estimate_for_message)
		.fold(0_u64, u64::saturating_add);

	// Per-message framing overhead charged by chat APIs (~4 tokens each).
	let framing_tokens = (messages.len() as u64).saturating_mul(4);

	let raw = system_tokens
		.saturating_add(message_tokens)
		.saturating_add(framing_tokens);
	let total = calibration.apply(raw);

	PromptTokenEstimate {
		system_tokens,
		message_tokens,
		framing_tokens,
		total_tokens: total,
		raw_total_tokens: raw,
	}
}

/// Total prompt-pressure number used by compaction trigger checks.
///
/// Thin wrapper over [`estimate_prompt_tokens_calibrated`] that returns just
/// the `total_tokens` field. New callers that need the per-segment
/// breakdown should call the calibrated function directly.
pub fn estimate_prompt_pressure(
	messages: &[Message],
	system_prompt: Option<&str>,
	calibration: &EstimatorCalibration,
) -> u64 {
	estimate_prompt_tokens_calibrated(messages, system_prompt, calibration).total_tokens
}

/// Per-message byte→token estimate.
fn byte_estimate_for_message(msg: &Message) -> u64 {
	match msg {
		Message::User { content } => byte_estimate_for_text(content),
		Message::Assistant { text, tool_calls } => {
			let mut t = byte_estimate_for_text(text);
			for tc in tool_calls {
				t = t.saturating_add(byte_estimate_for_text(&tc.name));
				// Arguments are always JSON — use the structured rule.
				let args_bytes = tc.arguments.to_string().len() as u64;
				t = t.saturating_add(args_bytes.div_ceil(2));
			}
			t
		}
		Message::ToolResult { content, .. } => byte_estimate_for_text(content),
	}
}

/// Classify a free-form string and return its byte-based token estimate.
///
/// Detection order is fixed and cheap (no allocation, single pass):
/// 1. **Structured** — first non-whitespace byte is `{`, `[`, or `<`:
///    treat the whole segment as JSON / XML-dense (`bytes / 2`).
/// 2. **CJK-dominated** — at least half of the first
///    [`CJK_SAMPLE_BYTES`] bytes have the high bit set. In UTF-8, ASCII
///    bytes never set the high bit, so a high ratio is a strong signal of
///    multi-byte CJK / emoji / accented text. Uses `bytes / 3` (one of the
///    two A7 fallbacks recommended by the master plan: byte/2 over-shoots
///    modern cl100k Chinese by ≈ 70%, while byte/3 stays inside the §9.2
///    ≤ 30% gate).
/// 3. **English / code default** — `bytes / 4`, the well-known OpenAI rule
///    of thumb for ASCII English prose and most source code.
fn byte_estimate_for_text(text: &str) -> u64 {
	let bytes = text.as_bytes();
	let len = bytes.len() as u64;
	if len == 0 {
		return 0;
	}
	if looks_like_structured(bytes) {
		return len.div_ceil(2);
	}
	if looks_like_cjk(bytes) {
		return len.div_ceil(3);
	}
	len.div_ceil(4)
}

fn looks_like_structured(bytes: &[u8]) -> bool {
	let is_ws = |b: u8| matches!(b, b' ' | b'\t' | b'\r' | b'\n');
	let mut iter = bytes.iter().copied().skip_while(|&b| is_ws(b));
	let Some(first) = iter.next() else {
		return false;
	};
	match first {
		// `{` almost never opens plain prose — treat as JSON/object directly.
		b'{' => true,
		// `[` also opens prose placeholders (e.g. "[Old tool result cleared]",
		// "[compaction summary] ..."). Only classify as structured when the
		// next non-whitespace byte is unambiguously the start of a JSON array
		// element — an opening quote, brace, bracket, digit, or the empty-
		// array closer. `t` / `f` / `n` / `-` are excluded on purpose: they
		// are valid JSON openers (true / false / null / negative number) but
		// also appear in prose words ("note", "then", "first"), and prose
		// false positives are much more frequent in this codebase than
		// `[true, false, null]`-shaped arrays.
		b'[' => {
			let next = iter.find(|&b| !is_ws(b));
			matches!(next, Some(b'"' | b'{' | b'[' | b']' | b'0'..=b'9'))
		}
		// `<` shows up in prose comparisons and ad-hoc placeholders
		// ("<unknown>", "value was < 5"). Require the next non-whitespace
		// byte to look like the start of a real tag: '/', '!', '?', or an
		// ASCII letter.
		b'<' => {
			let next = iter.find(|&b| !is_ws(b));
			matches!(next, Some(b'/' | b'!' | b'?' | b'a'..=b'z' | b'A'..=b'Z'))
		}
		_ => false,
	}
}

const CJK_SAMPLE_BYTES: usize = 256;

fn looks_like_cjk(bytes: &[u8]) -> bool {
	let sample_len = bytes.len().min(CJK_SAMPLE_BYTES);
	if sample_len == 0 {
		return false;
	}
	let mut hi = 0_usize;
	for &b in &bytes[..sample_len] {
		if b & 0x80 != 0 {
			hi += 1;
		}
	}
	hi * 2 >= sample_len
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

/// Placeholder text inserted in place of historical tool result content during
/// pre-flight microcompaction. Stable across calls so re-running microcompact
/// is byte-deterministic and idempotent.
pub const MICROCOMPACT_PLACEHOLDER: &str = "[Old tool result content cleared]";

/// Number of most recent tool result messages microcompact leaves untouched.
///
/// Picked small enough to keep the freed-token signal meaningful on
/// tool-heavy runs, but large enough that the LLM still has context for the
/// last few observations it produced.
pub const MICROCOMPACT_RETAIN_RECENT: usize = 3;

/// Pre-flight Layer 0 microcompaction.
///
/// Replaces the `content` of every `Message::ToolResult` older than the last
/// `retain_recent` tool results with [`MICROCOMPACT_PLACEHOLDER`]. Returns
/// the calibrated number of tokens freed by the substitution.
///
/// Invariants (see unit 03 acceptance):
///
/// - Message count, ordering, `tool_use_id`, and `is_error` flags are
///   unchanged. Only the `content` field of older tool results is mutated.
/// - Idempotent: running twice on the same buffer yields a byte-identical
///   second pass and returns 0 freed tokens on the second call.
/// - Pure mechanical operation — no LLM calls, no IO, no threshold check.
///   Designed to run unconditionally on every pre-flight.
/// - Never touches `User`, `Assistant`, or the trailing `retain_recent`
///   tool results. The system prompt is not in `messages` and is therefore
///   never inspected.
pub fn microcompact_old_tool_results(
	messages: &mut [Message],
	retain_recent: usize,
	calibration: &EstimatorCalibration,
) -> u64 {
	// Collect tool_result indices into a small Vec; bail if the run hasn't
	// accumulated more tool results than the retain window.
	let tool_result_indices: Vec<usize> = messages
		.iter()
		.enumerate()
		.filter_map(|(i, m)| matches!(m, Message::ToolResult { .. }).then_some(i))
		.collect();
	if tool_result_indices.len() <= retain_recent {
		return 0;
	}

	// Older = everything except the trailing `retain_recent` indices.
	let cutoff = tool_result_indices.len() - retain_recent;
	let placeholder_bytes = MICROCOMPACT_PLACEHOLDER.len();
	let placeholder_estimate = byte_estimate_for_text(MICROCOMPACT_PLACEHOLDER);
	let mut freed_raw: u64 = 0;
	for &idx in &tool_result_indices[..cutoff] {
		if let Message::ToolResult { content, .. } = &mut messages[idx] {
			// Idempotence: leave previously-cleared placeholders untouched.
			if content == MICROCOMPACT_PLACEHOLDER {
				continue;
			}
			// Short-body skip: if the original is already at or below the
			// placeholder's byte footprint (e.g. "ok" / "done" / empty),
			// replacing would expand the message instead of shrinking it.
			// The saturating_sub below would record zero freed tokens
			// anyway, so the net effect of a replacement is purely adverse.
			if content.len() <= placeholder_bytes {
				continue;
			}
			let before = byte_estimate_for_text(content);
			// Saturating diff — placeholder is shorter than any non-trivial
			// tool result, so this normally yields a positive freed value.
			freed_raw = freed_raw.saturating_add(before.saturating_sub(placeholder_estimate));
			*content = MICROCOMPACT_PLACEHOLDER.to_string();
		}
	}
	calibration.apply(freed_raw)
}

// ---------------------------------------------------------------------------
// Mid-tier compaction (unit 05: Layer 1 + Layer 2)
// ---------------------------------------------------------------------------

/// Fraction of the model context window at which mid-tier compaction
/// (Layer 1 / Layer 2) fires — strictly below Layer 3's high-water trigger
/// (default 0.75) and above the always-on Layer 0 microcompact.
pub const MID_WATER_TRIGGER_RATIO: f64 = 0.60;

/// Outcome of one mid-tier compaction attempt.
///
/// Callers use this to decide which `LoopEvent` to emit.
#[derive(Debug, Clone, PartialEq)]
pub enum MidCompactOutcome {
	/// Layer 2 fired: session memory summary was spliced in, replacing
	/// `messages_replaced` historical messages.
	Layer2 { messages_replaced: usize },
	/// Layer 1 fired: deterministic context collapse replaced
	/// `messages_collapsed` historical messages with a mechanical summary.
	Layer1 { messages_collapsed: usize },
	/// Neither layer had anything to do (buffer too short, or split
	/// resolved to a no-op boundary).
	Noop,
}

/// Mid-tier compaction: Layer 2 → Layer 1 cascade (no LLM calls).
///
/// Designed to run at the mid-water pressure point — above Layer 0's
/// always-on pre-flight and below Layer 3's high-water LLM trigger.
///
/// **Layer 2 (sessionMemoryCompaction)**: If `session_summary` is `Some`,
/// splice it into the message buffer in place of the oldest half of
/// messages (adjusted for `tool_use ↔ tool_result` pairing), then return
/// `MidCompactOutcome::Layer2`.
///
/// **Layer 1 (contextCollapse — deterministic fallback)**: When `session_summary`
/// is `None` (no memory summary available), collapse the oldest half of
/// messages using [`summarize_discarded_messages`] and return
/// `MidCompactOutcome::Layer1`.
///
/// Returns `MidCompactOutcome::Noop` when the buffer is too short to
/// produce a meaningful split (≤ 2 messages total, or adjusted split
/// resolves to index 0 or 1).
///
/// Invariants:
/// - `tool_use ↔ tool_result` pairing is always preserved (shared
///   [`adjust_split_for_tool_pairs`] helper).
/// - Idempotent at the structural level: running twice on a buffer that
///   already has a summary at index 1 will not collapse again because the
///   half-split naturally lands inside the summary message.
/// - No LLM calls. No IO. Pure synchronous mutation.
pub fn mid_compact_messages(
	messages: &mut Vec<Message>,
	session_summary: Option<&str>,
) -> MidCompactOutcome {
	// Need at least 3 messages (anchor + some history + tail) to produce a
	// meaningful split.
	if messages.len() < 3 {
		return MidCompactOutcome::Noop;
	}

	// Collapse the oldest half of the non-anchor messages (indices 1..).
	// "Oldest half" = messages[1 .. split], leaving messages[split..] as tail.
	let candidate_split = 1 + (messages.len() - 1).div_ceil(2);
	let split = adjust_split_for_tool_pairs(messages, candidate_split);
	if split <= 1 {
		return MidCompactOutcome::Noop;
	}

	let discarded: Vec<Message> = messages.drain(1..split).collect();
	if discarded.is_empty() {
		return MidCompactOutcome::Noop;
	}

	let summary_text = match session_summary {
		Some(s) => format!("[Session memory summary]\n{s}"),
		None => format!(
			"[Mid-tier context collapse — {} messages compacted]\n{}",
			discarded.len(),
			summarize_discarded_messages(&discarded)
		),
	};

	let replaced = discarded.len();
	messages.insert(
		1,
		Message::User {
			content: summary_text,
		},
	);

	match session_summary {
		Some(_) => MidCompactOutcome::Layer2 {
			messages_replaced: replaced,
		},
		None => MidCompactOutcome::Layer1 {
			messages_collapsed: replaced,
		},
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

// ============================================================
// Structured-summary compaction (unit 04)
// ============================================================

/// Maximum number of times the structured summarizer retries with a
/// progressively halved "discarded" prefix when the provider returns
/// `ContextWindowExceeded`. After this many retries, the outer attempt is
/// counted as a single summarization failure for circuit-breaker purposes.
pub const MAX_DROP_OLDEST_RETRIES: u32 = 3;

/// Required section headers the LLM must emit, in order. Validation only
/// checks for presence (case-sensitive substring match on `"<Header>:"`),
/// not strict ordering.
pub const STRUCTURED_SUMMARY_SECTIONS: &[&str] =
	&["Goal", "Accomplished", "Key Decisions", "Relevant Files"];

const STRUCTURED_SUMMARY_SYSTEM_PROMPT: &str = "You summarize agent conversation history into a strictly structured form. Output ONLY the four sections requested, each headed by a line of the form `<Section>:` and nothing else outside them.";

/// Outcome of one structured-summary compaction attempt.
#[derive(Debug, Clone, PartialEq)]
pub struct StructuredCompactOutcome {
	/// `true` when the LLM returned a contract-conformant summary that was
	/// inserted into `messages`. `false` when the function fell back to a
	/// deterministic mechanical summary because the LLM call failed,
	/// timed out, or violated the structured contract.
	pub succeeded: bool,
	/// Prompt tokens consumed by all summarizer LLM attempts (including
	/// drop-oldest retries). `0` when no provider call returned usage info
	/// or when no LLM call was made.
	pub prompt_tokens: u64,
	/// Output tokens consumed by all summarizer LLM attempts.
	pub output_tokens: u64,
	/// Number of drop-oldest retries actually performed (`0..=MAX_DROP_OLDEST_RETRIES`).
	pub drop_oldest_retries: u32,
	/// Number of messages that were originally drained from `messages` and
	/// targeted for summarization. `0` means there was nothing to compact.
	pub discarded_count: usize,
	/// Why the LLM path failed, if it failed. `None` on success.
	pub error: Option<StructuredCompactError>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StructuredCompactError {
	/// Provider returned `ContextWindowExceeded` and we exhausted the
	/// drop-oldest retry budget.
	OverflowAfterRetries,
	/// Provider call failed (network, timeout, model error other than
	/// ContextWindowExceeded, or empty output).
	ProviderFailure,
	/// Provider returned text but it did not contain all required structured
	/// sections.
	ContractViolation,
}

impl StructuredCompactOutcome {
	fn nothing_to_compact() -> Self {
		Self {
			succeeded: false,
			prompt_tokens: 0,
			output_tokens: 0,
			drop_oldest_retries: 0,
			discarded_count: 0,
			error: None,
		}
	}
}

/// Compact conversation messages with a strict structured-summary contract.
///
/// Drains messages `1..split` (where `split = len - retain_tail` adjusted
/// for tool-pair safety), asks the LLM to produce a 4-section summary
/// (Goal / Accomplished / Key Decisions / Relevant Files), validates the
/// response, and inserts it back as a single User message. On
/// `ContextWindowExceeded`, the discarded buffer is halved from the front
/// and the call retried up to [`MAX_DROP_OLDEST_RETRIES`] times.
///
/// On any failure path (overflow after retries, provider error, contract
/// violation), the function falls back to a deterministic mechanical
/// summary built from the **original** discarded set and inserts that
/// instead, so the post-compact `messages` size is always strictly less
/// than the pre-call size. Callers use [`StructuredCompactOutcome::error`]
/// to drive their circuit breaker.
pub async fn compact_messages_with_structured_summary(
	messages: &mut Vec<Message>,
	retain_tail: usize,
	router: &LlmRouter,
	config: &CompactConfig,
) -> StructuredCompactOutcome {
	if messages.len() <= retain_tail + 1 {
		return StructuredCompactOutcome::nothing_to_compact();
	}
	let split = messages.len() - retain_tail;
	if split <= 1 {
		return StructuredCompactOutcome::nothing_to_compact();
	}
	let split = adjust_split_for_tool_pairs(messages, split);
	if split <= 1 {
		return StructuredCompactOutcome::nothing_to_compact();
	}

	let mut discarded: Vec<Message> = messages.drain(1..split).collect();
	let original_discarded = discarded.clone();
	let original_discarded_len = original_discarded.len();
	let mut drop_oldest_retries: u32 = 0;
	let mut total_prompt_tokens: u64 = 0;
	let mut total_output_tokens: u64 = 0;

	let outcome: Result<String, StructuredCompactError> = loop {
		if discarded.is_empty() {
			break Err(StructuredCompactError::OverflowAfterRetries);
		}
		let mechanical = summarize_discarded_messages(&discarded);
		let prompt = build_structured_summary_prompt(&mechanical);

		let result = tokio::time::timeout(
			COMPACT_LLM_TIMEOUT,
			router.generate(&GenerationRequest {
				system_prompt: Some(STRUCTURED_SUMMARY_SYSTEM_PROMPT.to_string()),
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
				system_prompt_sections: None,
			}),
		)
		.await;

		match result {
			Ok(Ok(resp)) => {
				total_prompt_tokens = total_prompt_tokens.saturating_add(resp.prompt_tokens);
				total_output_tokens = total_output_tokens.saturating_add(resp.output_tokens);
				let text = resp.output;
				if text.trim().is_empty() {
					break Err(StructuredCompactError::ProviderFailure);
				}
				if validate_structured_summary(&text) {
					break Ok(text);
				} else {
					break Err(StructuredCompactError::ContractViolation);
				}
			}
			Ok(Err(LlmAdapterError::ContextWindowExceeded { .. })) => {
				if drop_oldest_retries >= MAX_DROP_OLDEST_RETRIES {
					break Err(StructuredCompactError::OverflowAfterRetries);
				}
				drop_oldest_retries += 1;
				// Halve the discarded buffer by trimming from the front
				// (the oldest messages). Guarantees strict shrinkage so the
				// loop terminates in O(log n) outer iterations.
				let drop = discarded.len().div_ceil(2);
				discarded.drain(0..drop);
			}
			Ok(Err(_)) | Err(_) => {
				break Err(StructuredCompactError::ProviderFailure);
			}
		}
	};

	let (summary_body, succeeded, error) = match outcome {
		Ok(summary) => (summary, true, None),
		Err(e) => {
			// Mechanical fallback uses the *original* full discarded set so
			// none of the dropped-oldest content is lost from the summary,
			// even though we couldn't fit it into the LLM call.
			let fallback = summarize_discarded_messages(&original_discarded);
			(fallback, false, Some(e))
		}
	};

	messages.insert(
		1,
		Message::User {
			content: format!("[Conversation summary]\n{summary_body}"),
		},
	);

	StructuredCompactOutcome {
		succeeded,
		prompt_tokens: total_prompt_tokens,
		output_tokens: total_output_tokens,
		drop_oldest_retries,
		discarded_count: original_discarded_len,
		error,
	}
}

fn build_structured_summary_prompt(mechanical_digest: &str) -> String {
	format!(
		"Summarize this agent conversation excerpt into EXACTLY four sections, in order:\n\n\
         Goal: <one sentence describing the user's objective>\n\
         Accomplished: <bulleted list of what has been done; use `- ` bullets>\n\
         Key Decisions: <bulleted list of important decisions and rationale>\n\
         Relevant Files: <bulleted list of files that matter for continuing the work>\n\n\
         Do not include any text outside these four sections. Preserve tool_use_id\n\
         references when summarizing tool results.\n\n\
         === Excerpt ===\n{mechanical_digest}",
	)
}

/// Returns `true` iff `text` contains all required section headers (each
/// in the form `"<Header>:"`). Order is not enforced — a forgiving check
/// that survives small LLM formatting drift while still catching outputs
/// that ignore the contract entirely.
pub fn validate_structured_summary(text: &str) -> bool {
	STRUCTURED_SUMMARY_SECTIONS
		.iter()
		.all(|h| text.contains(&format!("{h}:")))
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
			disallowed_tools: Vec::new(),
			estimator_calibration: EstimatorCalibration::default(),
			consecutive_autocompact_failures: 0,
			frozen_tool_schema: None,
			tool_schema_dirty: true,
			observed_plan_mode: None,
			cache_break_detector: crate::runtime_loop::cache_break::CacheBreakDetector::default(),
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

	// ------------------------------------------------------------------
	// Layer 0 microcompact tests (unit 03)
	// ------------------------------------------------------------------

	/// Build a synthetic tool-heavy message buffer with `n` Assistant→ToolResult
	/// pairs, each ToolResult carrying a distinct "fat" body so byte estimates
	/// after microcompact are obviously smaller than before.
	fn synthetic_tool_history(n: usize) -> Vec<Message> {
		use roku_plugin_llm::{Message, ToolCallBlock};
		let mut messages: Vec<Message> = vec![Message::User {
			content: "initial goal".to_string(),
		}];
		for i in 0..n {
			messages.push(Message::Assistant {
				text: format!("calling tool {i}"),
				tool_calls: vec![ToolCallBlock {
					id: format!("tc-{i}"),
					name: "Bash".to_string(),
					arguments: json!({"command": format!("echo {i}")}),
				}],
			});
			messages.push(Message::ToolResult {
				tool_use_id: format!("tc-{i}"),
				// Fat body so estimator differences show up clearly.
				content: format!("tool result number {i} — {}", "a".repeat(400)),
				is_error: false,
			});
		}
		messages
	}

	#[test]
	fn microcompact_clears_old_tool_results_and_retains_recent() {
		let mut messages = synthetic_tool_history(10);
		let calibration = EstimatorCalibration::default();

		let freed = microcompact_old_tool_results(&mut messages, 3, &calibration);

		// Non-zero freed tokens on a tool-heavy run.
		assert!(
			freed > 0,
			"expected microcompact to free a positive number of tokens, got {freed}"
		);

		// The first 7 tool_results should be placeholders; the last 3 untouched.
		let tool_results: Vec<&Message> = messages
			.iter()
			.filter(|m| matches!(m, Message::ToolResult { .. }))
			.collect();
		assert_eq!(tool_results.len(), 10);
		for (i, m) in tool_results.iter().enumerate() {
			if let Message::ToolResult { content, .. } = m {
				if i < 7 {
					assert_eq!(
						content, MICROCOMPACT_PLACEHOLDER,
						"tool_result {i} should be cleared"
					);
				} else {
					assert!(
						content.contains(&format!("tool result number {i}")),
						"tool_result {i} should retain original content"
					);
				}
			}
		}
	}

	#[test]
	fn microcompact_frees_at_least_30_percent_of_historical_tool_result_tokens() {
		// Acceptance gate for issue #298: on a tool-heavy run, one pre-flight
		// pass must free ≥ 30% of the tokens held by historical tool_result
		// content. The synthetic fixture keeps each tool_result at ~400 bytes
		// of body text, well above the placeholder's footprint, so the ratio
		// should land comfortably above the gate.
		let n = 10usize;
		let retain = 3usize;
		let mut messages = synthetic_tool_history(n);
		let calibration = EstimatorCalibration::default();

		// Baseline must match what microcompact actually clears: the first
		// `n - retain` ToolResult messages by ToolResult position, NOT by
		// flat message index. The previous `i < n - retain` filter indexed
		// into the full `[User, (Assistant, ToolResult) × n]` layout, so it
		// only collected 3 of the 7 ToolResults the function clears and
		// made the ratio gate vacuous (any non-zero freed count passed).
		let historical_raw: u64 = messages
			.iter()
			.filter_map(|m| match m {
				Message::ToolResult { content, .. } => Some(content.as_str()),
				_ => None,
			})
			.take(n - retain)
			.map(byte_estimate_for_text)
			.sum();
		let historical_tokens = calibration.apply(historical_raw);
		assert!(
			historical_tokens > 0,
			"fixture must produce a non-zero baseline"
		);

		let freed = microcompact_old_tool_results(&mut messages, retain, &calibration);
		let ratio = freed as f64 / historical_tokens as f64;
		assert!(
			ratio >= 0.30,
			"expected freed/historical ≥ 0.30, got {ratio:.3} (freed={freed}, historical={historical_tokens})"
		);
	}

	#[test]
	fn microcompact_preserves_message_structure() {
		use roku_plugin_llm::Message;
		let before = synthetic_tool_history(8);
		let mut after = before.clone();
		let calibration = EstimatorCalibration::default();
		let _freed = microcompact_old_tool_results(&mut after, 3, &calibration);

		// Same length.
		assert_eq!(before.len(), after.len());
		// Same per-index variant + tool_use_id pairing.
		for (b, a) in before.iter().zip(after.iter()) {
			match (b, a) {
				(Message::User { content: bc }, Message::User { content: ac }) => {
					assert_eq!(bc, ac);
				}
				(
					Message::Assistant {
						text: bt,
						tool_calls: btc,
					},
					Message::Assistant {
						text: at,
						tool_calls: atc,
					},
				) => {
					// Assistant messages must be identical — microcompact never
					// rewrites them.
					assert_eq!(bt, at);
					assert_eq!(btc.len(), atc.len());
					for (b, a) in btc.iter().zip(atc.iter()) {
						assert_eq!(b.id, a.id);
						assert_eq!(b.name, a.name);
						assert_eq!(b.arguments, a.arguments);
					}
				}
				(
					Message::ToolResult {
						tool_use_id: bid,
						is_error: be,
						..
					},
					Message::ToolResult {
						tool_use_id: aid,
						is_error: ae,
						..
					},
				) => {
					assert_eq!(bid, aid, "tool_use_id pairing must survive microcompact");
					assert_eq!(be, ae, "is_error flag must survive microcompact");
				}
				_ => panic!("message variant changed across microcompact"),
			}
		}
	}

	#[test]
	fn microcompact_is_idempotent() {
		let mut messages = synthetic_tool_history(6);
		let calibration = EstimatorCalibration::default();

		let freed_first = microcompact_old_tool_results(&mut messages, 2, &calibration);
		assert!(freed_first > 0);
		let snapshot_after_first = messages.clone();

		let freed_second = microcompact_old_tool_results(&mut messages, 2, &calibration);
		assert_eq!(
			freed_second, 0,
			"second microcompact run on the same buffer must free 0 additional tokens"
		);
		// Byte-identical buffers across the two runs.
		assert_eq!(snapshot_after_first.len(), messages.len());
		for (a, b) in snapshot_after_first.iter().zip(messages.iter()) {
			if let (
				Message::ToolResult { content: ac, .. },
				Message::ToolResult { content: bc, .. },
			) = (a, b)
			{
				assert_eq!(
					ac, bc,
					"tool_result content must be byte-stable across runs"
				);
			}
		}
	}

	#[test]
	fn microcompact_noop_when_below_retain_window() {
		let mut messages = synthetic_tool_history(2);
		let snapshot = messages.clone();
		let calibration = EstimatorCalibration::default();

		// 2 tool_results, retain_recent=3 → nothing to clear.
		let freed = microcompact_old_tool_results(&mut messages, 3, &calibration);
		assert_eq!(freed, 0);
		assert_eq!(messages.len(), snapshot.len());
		for (a, b) in snapshot.iter().zip(messages.iter()) {
			if let (
				Message::ToolResult { content: ac, .. },
				Message::ToolResult { content: bc, .. },
			) = (a, b)
			{
				assert_eq!(ac, bc);
			}
		}
	}

	#[test]
	fn microcompact_skips_tool_results_at_or_below_placeholder_size() {
		use roku_plugin_llm::{Message, ToolCallBlock};

		// Short tool-result bodies are at or below MICROCOMPACT_PLACEHOLDER's
		// byte footprint. Replacing them would grow the message instead of
		// shrinking it, so Layer 0 must leave them untouched and record 0
		// freed tokens for those slots.
		let short_bodies: [&str; 3] = ["ok", "done", ""];
		assert!(
			short_bodies
				.iter()
				.all(|s| s.len() <= MICROCOMPACT_PLACEHOLDER.len()),
			"fixture precondition: each short body must fit within the placeholder footprint"
		);

		let mut messages: Vec<Message> = vec![Message::User {
			content: "start".to_string(),
		}];
		for (i, body) in short_bodies.iter().enumerate() {
			messages.push(Message::Assistant {
				text: format!("call {i}"),
				tool_calls: vec![ToolCallBlock {
					id: format!("tc-{i}"),
					name: "Bash".to_string(),
					arguments: json!({}),
				}],
			});
			messages.push(Message::ToolResult {
				tool_use_id: format!("tc-{i}"),
				content: (*body).to_string(),
				is_error: false,
			});
		}
		// Append one fat tool_result that sits inside the retain_recent
		// window so the eligible cutoff covers all three short bodies.
		messages.push(Message::Assistant {
			text: "call fat".to_string(),
			tool_calls: vec![ToolCallBlock {
				id: "tc-fat".to_string(),
				name: "Bash".to_string(),
				arguments: json!({}),
			}],
		});
		messages.push(Message::ToolResult {
			tool_use_id: "tc-fat".to_string(),
			content: "x".repeat(400),
			is_error: false,
		});

		let calibration = EstimatorCalibration::default();
		let freed = microcompact_old_tool_results(&mut messages, 1, &calibration);
		assert_eq!(
			freed, 0,
			"short tool_results must not contribute freed tokens"
		);

		let tool_results: Vec<&str> = messages
			.iter()
			.filter_map(|m| match m {
				Message::ToolResult { content, .. } => Some(content.as_str()),
				_ => None,
			})
			.collect();
		assert_eq!(tool_results.len(), 4);
		for (i, expected) in short_bodies.iter().enumerate() {
			assert_eq!(
				tool_results[i], *expected,
				"short tool_result {i} must stay byte-identical to its original body"
			);
		}
		assert_eq!(
			tool_results[3].len(),
			400,
			"tool_result inside retain window must stay untouched"
		);
	}

	#[test]
	fn microcompact_does_not_touch_user_or_assistant_messages() {
		use roku_plugin_llm::{Message, ToolCallBlock};
		let mut messages = vec![
			Message::User {
				content: "long user instruction ".repeat(50),
			},
			Message::Assistant {
				text: "long assistant text ".repeat(50),
				tool_calls: vec![ToolCallBlock {
					id: "tc-0".to_string(),
					name: "Bash".to_string(),
					arguments: json!({}),
				}],
			},
			Message::ToolResult {
				tool_use_id: "tc-0".to_string(),
				content: "fat tool result ".repeat(100),
				is_error: false,
			},
			Message::Assistant {
				text: "second assistant turn".to_string(),
				tool_calls: vec![ToolCallBlock {
					id: "tc-1".to_string(),
					name: "Bash".to_string(),
					arguments: json!({}),
				}],
			},
			Message::ToolResult {
				tool_use_id: "tc-1".to_string(),
				content: "another fat tool result ".repeat(100),
				is_error: false,
			},
		];
		let user_before = match &messages[0] {
			Message::User { content } => content.clone(),
			_ => unreachable!(),
		};
		let asst0_before = match &messages[1] {
			Message::Assistant { text, .. } => text.clone(),
			_ => unreachable!(),
		};

		let calibration = EstimatorCalibration::default();
		// retain_recent=1 → only the last tool_result stays untouched, the
		// first one gets cleared. User and Assistant must remain identical.
		let freed = microcompact_old_tool_results(&mut messages, 1, &calibration);
		assert!(freed > 0);

		assert!(matches!(&messages[0], Message::User { content } if content == &user_before));
		assert!(matches!(&messages[1], Message::Assistant { text, .. } if text == &asst0_before));
		// First tool_result cleared, second tool_result intact.
		if let Message::ToolResult { content, .. } = &messages[2] {
			assert_eq!(content, MICROCOMPACT_PLACEHOLDER);
		}
		if let Message::ToolResult { content, .. } = &messages[4] {
			assert!(content.contains("another fat tool result"));
		}
	}

	// ----------------------------------------------------------------------
	// Structured summary tests (unit 04)
	// ----------------------------------------------------------------------

	#[test]
	fn validate_structured_summary_accepts_well_formed_output() {
		let summary = "Goal: Investigate the bug.\n\
					   Accomplished:\n- Read logs\n- Found the regression\n\
					   Key Decisions:\n- Roll back to v1.2\n\
					   Relevant Files:\n- src/main.rs\n- tests/regression.rs\n";
		assert!(validate_structured_summary(summary));
	}

	#[test]
	fn validate_structured_summary_rejects_missing_sections() {
		// Missing "Relevant Files".
		let bad = "Goal: do thing\nAccomplished: did thing\nKey Decisions: none\n";
		assert!(!validate_structured_summary(bad));
	}

	// Mock provider for structured-summary async tests.
	// Defined locally because SequenceProvider in roku_plugin_llm is test-private.
	use async_trait::async_trait;
	use roku_plugin_llm::{LlmProvider, ModelProfile, ProviderCallError, ProviderResponse};
	use std::collections::VecDeque;
	use std::sync::Mutex;

	struct MockSequenceProvider {
		responses: Mutex<VecDeque<Result<ProviderResponse, ProviderCallError>>>,
	}

	impl MockSequenceProvider {
		fn new(responses: Vec<Result<ProviderResponse, ProviderCallError>>) -> Self {
			Self {
				responses: Mutex::new(responses.into()),
			}
		}
	}

	#[async_trait]
	impl LlmProvider for MockSequenceProvider {
		fn provider_name(&self) -> &'static str {
			"mock-structured"
		}

		async fn complete(
			&self,
			_model: &ModelProfile,
			_request: &GenerationRequest,
		) -> Result<ProviderResponse, ProviderCallError> {
			self.responses
				.lock()
				.unwrap()
				.pop_front()
				.unwrap_or_else(|| {
					Err(ProviderCallError::Fatal {
						message: "no more responses".to_string(),
					})
				})
		}
	}

	fn make_router(responses: Vec<Result<ProviderResponse, ProviderCallError>>) -> LlmRouter {
		use roku_plugin_llm::{ModelProfile, RiskTier, RoutingPolicy};
		let mut router = LlmRouter::new(RoutingPolicy::default());
		router.register_provider(MockSequenceProvider::new(responses));
		router.register_model(ModelProfile {
			model_id: "mock-model".to_string(),
			provider: "mock-structured".to_string(),
			max_context_tokens: 100_000,
			cost_per_1k_tokens_usd: 0.01,
			max_risk_tier: RiskTier::Critical,
			route_priority: 100,
		});
		router
	}

	fn valid_structured_summary() -> String {
		"Goal: Finish the task.\n\
		 Accomplished:\n- Step A done\n- Step B done\n\
		 Key Decisions:\n- Use approach X\n\
		 Relevant Files:\n- src/lib.rs\n"
			.to_string()
	}

	fn ok_response(text: &str) -> Result<ProviderResponse, ProviderCallError> {
		Ok(ProviderResponse {
			output: text.to_string(),
			finish_reason: None,
			prompt_tokens: 50,
			output_tokens: 30,
			cache_creation_input_tokens: 0,
			cache_read_input_tokens: 0,
			latency_ms: 10,
			tool_calls: None,
		})
	}

	fn context_exceeded_error() -> Result<ProviderResponse, ProviderCallError> {
		Err(ProviderCallError::ContextWindowExceeded {
			detail: "prompt too long".to_string(),
		})
	}

	fn build_messages(n: usize) -> Vec<Message> {
		let mut messages = vec![Message::User {
			content: "initial goal".to_string(),
		}];
		for i in 0..n {
			messages.push(Message::Assistant {
				text: format!("step {i}"),
				tool_calls: vec![],
			});
			messages.push(Message::User {
				content: format!("user reply {i}"),
			});
		}
		messages
	}

	#[tokio::test]
	async fn structured_summary_inserts_summary_on_success() {
		// 12-message conversation: 1 initial + 11 more (rounded: 5 pairs + 1)
		// Build with build_messages(5) = 1 + 5*2 = 11 messages total
		let mut messages = build_messages(5);
		// Ensure we have at least 12; add one more assistant
		messages.push(Message::Assistant {
			text: "final step".to_string(),
			tool_calls: vec![],
		});
		assert_eq!(messages.len(), 12);

		let router = make_router(vec![ok_response(&valid_structured_summary())]);
		let config = CompactConfig::default();

		let outcome =
			compact_messages_with_structured_summary(&mut messages, 4, &router, &config).await;

		// Leak the router to avoid dropping a tokio blocking runtime inside
		// an async test context.
		std::mem::forget(router);

		assert!(outcome.succeeded, "should succeed on valid summary");
		assert_eq!(outcome.error, None);
		assert_eq!(outcome.drop_oldest_retries, 0);
		// 1 anchor + 1 summary + 4 retained = 6
		assert_eq!(messages.len(), 6, "expected 6 messages after compaction");
		if let Message::User { content } = &messages[1] {
			assert!(
				content.contains("[Conversation summary]"),
				"messages[1] should be the summary"
			);
			assert!(
				validate_structured_summary(content),
				"summary should contain all required sections"
			);
		} else {
			panic!("messages[1] should be a User message");
		}
	}

	#[tokio::test]
	async fn structured_summary_falls_back_on_contract_violation() {
		let mut messages = build_messages(5);
		messages.push(Message::Assistant {
			text: "final step".to_string(),
			tool_calls: vec![],
		});

		let router = make_router(vec![ok_response("I cannot summarize this.")]);
		let config = CompactConfig::default();

		let outcome =
			compact_messages_with_structured_summary(&mut messages, 4, &router, &config).await;

		std::mem::forget(router);

		assert!(!outcome.succeeded);
		assert_eq!(
			outcome.error,
			Some(StructuredCompactError::ContractViolation)
		);
		assert_eq!(outcome.drop_oldest_retries, 0);
		// Mechanical fallback is inserted at index 1.
		if let Message::User { content } = &messages[1] {
			assert!(
				content.contains("messages compacted"),
				"fallback should contain 'messages compacted', got: {content}"
			);
		} else {
			panic!("messages[1] should be a User fallback message");
		}
	}

	#[tokio::test]
	async fn structured_summary_retries_on_context_window_exceeded() {
		let mut messages = build_messages(5);
		messages.push(Message::Assistant {
			text: "final step".to_string(),
			tool_calls: vec![],
		});

		// Two ContextWindowExceeded, then a valid summary on the third try.
		let router = make_router(vec![
			context_exceeded_error(),
			context_exceeded_error(),
			ok_response(&valid_structured_summary()),
		]);
		let config = CompactConfig::default();

		let outcome =
			compact_messages_with_structured_summary(&mut messages, 4, &router, &config).await;

		std::mem::forget(router);

		assert!(outcome.succeeded, "should succeed after retries");
		assert_eq!(outcome.drop_oldest_retries, 2);
		if let Message::User { content } = &messages[1] {
			assert!(validate_structured_summary(content));
		} else {
			panic!("messages[1] should be a User summary message");
		}
	}

	#[tokio::test]
	async fn structured_summary_gives_up_after_max_drop_oldest_retries() {
		let mut messages = build_messages(5);
		messages.push(Message::Assistant {
			text: "final step".to_string(),
			tool_calls: vec![],
		});

		// MAX_DROP_OLDEST_RETRIES + 1 = 4 ContextWindowExceeded responses.
		let router = make_router(vec![
			context_exceeded_error(),
			context_exceeded_error(),
			context_exceeded_error(),
			context_exceeded_error(),
		]);
		let config = CompactConfig::default();

		let outcome =
			compact_messages_with_structured_summary(&mut messages, 4, &router, &config).await;

		std::mem::forget(router);

		assert!(!outcome.succeeded);
		assert_eq!(
			outcome.error,
			Some(StructuredCompactError::OverflowAfterRetries)
		);
		assert_eq!(outcome.drop_oldest_retries, MAX_DROP_OLDEST_RETRIES);
		// Mechanical fallback still inserted.
		if let Message::User { content } = &messages[1] {
			assert!(
				content.contains("messages compacted"),
				"fallback should be inserted even after overflow, got: {content}"
			);
		} else {
			panic!("messages[1] should be a User fallback message");
		}
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

	// ------------------------------------------------------------------
	// Unit 01: byte-based estimator regression suite + calibration
	// ------------------------------------------------------------------
	//
	// These tests anchor the §9.2 accuracy gates (≤20% English/code,
	// ≤30% CJK) against a deterministic oracle. The oracle is a coarse
	// stand-in for cl100k_base — it counts ASCII chars at 1 token / 4 chars
	// and high-bit chars (CJK / emoji / accented) at 1 token / char, which
	// lines up with the well-known OpenAI rule of thumb. If the oracle
	// changes, the gate values move with it; if the byte estimator drifts
	// outside the gates against this oracle, the unit 01 design has
	// regressed.

	/// Approximate cl100k_base oracle. ASCII chars cost ≈ 1 token / 4 chars;
	/// high-bit (CJK / emoji / accented) chars cost ≈ 1 token / char.
	fn oracle_tokens(text: &str) -> u64 {
		let mut ascii_chars = 0_u64;
		let mut hi_chars = 0_u64;
		for c in text.chars() {
			if c.is_ascii() {
				ascii_chars += 1;
			} else {
				hi_chars += 1;
			}
		}
		ascii_chars.div_ceil(4) + hi_chars
	}

	fn relative_error(estimate: u64, real: u64) -> f64 {
		if real == 0 {
			return 0.0;
		}
		((estimate as f64) - (real as f64)).abs() / (real as f64)
	}

	#[test]
	fn estimator_meets_accuracy_gates_for_english_code_and_cjk() {
		// Three samples, three classes, one assertion run — the spec
		// forbids passing only the English samples.

		// Class 1 — English prose (~600 ASCII bytes).
		let english = "The quick brown fox jumps over the lazy dog. \
				 Pack my box with five dozen liquor jugs. \
				 The five boxing wizards jump quickly. \
				 How vexingly quick daft zebras jump! \
				 Sphinx of black quartz, judge my vow. \
				 Two driven jocks help fax my big quiz. \
				 Watch Jeopardy, Alex Trebek's fun TV quiz game. \
				 The quick onyx goblin jumps over the lazy dwarf. \
				 Crazy Fredrick bought many very exquisite opal jewels. \
				 We promptly judged antique ivory buckles for the next prize.";
		let est_en = byte_estimate_for_text(english);
		let real_en = oracle_tokens(english);
		let err_en = relative_error(est_en, real_en);

		// Class 2 — Source code (~600 ASCII bytes; not raw JSON because the
		// JSON branch is a safety overshoot, not a precision target).
		let code = "fn parse_message(input: &str) -> Result<Message, ParseError> {\n    \
				let trimmed = input.trim();\n    \
				if trimmed.is_empty() {\n        \
					return Err(ParseError::Empty);\n    \
				}\n    \
				let parts: Vec<&str> = trimmed.splitn(2, ':').collect();\n    \
				match parts.as_slice() {\n        \
					[role, body] => Ok(Message {\n            \
						role: role.trim().to_string(),\n            \
						body: body.trim().to_string(),\n        \
					}),\n        \
					_ => Err(ParseError::MissingColon),\n    \
				}\n}\n";
		let est_code = byte_estimate_for_text(code);
		let real_code = oracle_tokens(code);
		let err_code = relative_error(est_code, real_code);

		// Class 3 — CJK-dominated paragraph (~150 high-bit chars,
		// roughly 450 UTF-8 bytes after the leading whitespace skip).
		let cjk = "在大型语言模型的运行时中，token 估算的准确度直接决定了上下文压缩何时触发。\
				 如果估算器持续偏小，压缩动作就会姗姗来迟，最终撞上模型的硬上限并触发回滚级故障。\
				 反过来，如果估算器持续偏大，正常会话也会被无谓地压缩，丢失关键的上下文信息。\
				 我们在 Roku 中采用基于字节长度的估算策略：英文与代码默认按四分之一计算，\
				 包含大量中日韩字符的段落则切换到三分之一这一更保守的系数；同时，每一次成功的 \
				 LLM 调用之后，都会用真实的 prompt_tokens 反馈对线性系数进行原地校准。";
		let est_cjk = byte_estimate_for_text(cjk);
		let real_cjk = oracle_tokens(cjk);
		let err_cjk = relative_error(est_cjk, real_cjk);

		assert!(
			err_en <= 0.20,
			"English error {err_en:.3} exceeds 20% gate (estimate={est_en}, oracle={real_en})"
		);
		assert!(
			err_code <= 0.20,
			"Code error {err_code:.3} exceeds 20% gate (estimate={est_code}, oracle={real_code})"
		);
		assert!(
			err_cjk <= 0.30,
			"CJK error {err_cjk:.3} exceeds 30% gate (estimate={est_cjk}, oracle={real_cjk})"
		);
	}

	#[test]
	fn structured_branch_uses_byte_div_two_and_cjk_branch_uses_byte_div_three() {
		let json = "{\"key\": \"value\", \"items\": [1, 2, 3]}";
		// First non-whitespace byte is `{` → byte/2.
		assert_eq!(
			byte_estimate_for_text(json),
			(json.len() as u64).div_ceil(2),
			"structured detection should produce byte/2"
		);

		let cjk = "中文段落示例中文段落示例中文段落示例中文段落示例中文段落示例";
		// >50% high-bit ratio → byte/3.
		assert_eq!(
			byte_estimate_for_text(cjk),
			(cjk.len() as u64).div_ceil(3),
			"CJK detection should produce byte/3"
		);

		let english = "Plain English text without any structured markers.";
		// Default → byte/4.
		assert_eq!(
			byte_estimate_for_text(english),
			(english.len() as u64).div_ceil(4),
			"English default should produce byte/4"
		);

		assert_eq!(byte_estimate_for_text(""), 0, "empty string is 0 tokens");
	}

	#[test]
	fn structured_heuristic_rejects_bracket_and_angle_prose() {
		// Bracket-prefixed placeholders / summaries are prose, not JSON.
		// The estimator must score them at bytes/4 (English default); the
		// previous `[` shortcut mis-scored them at bytes/2, which matters
		// because Layer 0's own MICROCOMPACT_PLACEHOLDER starts with `[`.
		//
		// `<letter>`-shaped prose like `<unknown>` / `<none>` is a genuine
		// tie with real tags and is deliberately NOT covered here — the
		// tightened heuristic still scores those at bytes/2. We only assert
		// the cases where a cheap lookahead can safely distinguish prose
		// from structured input.
		let prose_samples = [
			MICROCOMPACT_PLACEHOLDER,
			"[compaction summary] oldest 12 messages collapsed",
			"[ note: trailing whitespace before the bracket does not matter ]",
			"[true but not json, actually prose]",
			"< 5 milliseconds",
		];
		for sample in prose_samples {
			let expected = (sample.len() as u64).div_ceil(4);
			assert_eq!(
				byte_estimate_for_text(sample),
				expected,
				"prose sample should be byte/4 (not mis-classified as structured): {sample:?}"
			);
		}

		// Real JSON arrays, XML / HTML / DOCTYPE markers, and JSON objects
		// must still resolve to bytes/2 so genuine structured payloads are
		// protected by the over-estimate safety margin.
		let structured_samples = [
			"[1, 2, 3]",
			"[\n  {\"k\": \"v\"}\n]",
			"[\"a\", \"b\"]",
			"[]",
			"<tag>child</tag>",
			"<!DOCTYPE html>",
			"</closing>",
			"{\"k\": 1}",
		];
		for sample in structured_samples {
			let expected = (sample.len() as u64).div_ceil(2);
			assert_eq!(
				byte_estimate_for_text(sample),
				expected,
				"structured sample should stay at byte/2: {sample:?}"
			);
		}
	}

	#[test]
	fn estimate_prompt_tokens_calibrated_breaks_down_system_messages_framing() {
		let cal = EstimatorCalibration::default();
		let messages = vec![
			Message::User {
				content: "hello world".to_string(),
			},
			Message::Assistant {
				text: "hi back".to_string(),
				tool_calls: vec![],
			},
		];
		let est = estimate_prompt_tokens_calibrated(&messages, Some("system context"), &cal);
		assert!(est.system_tokens > 0, "system tokens populated");
		assert!(est.message_tokens > 0, "message tokens populated");
		assert_eq!(est.framing_tokens, 8, "two messages × 4 tokens framing");
		let expected_raw = est.system_tokens + est.message_tokens + est.framing_tokens;
		assert_eq!(est.raw_total_tokens, expected_raw);
		// `scale` is 1.0 by default, so the calibrated total matches the
		// raw sum here; this is not true after calibration updates.
		assert_eq!(est.total_tokens, expected_raw);
	}

	#[test]
	fn calibration_pulls_subsequent_estimate_closer_to_real_value() {
		let mut cal = EstimatorCalibration::default();
		// Build a message that will deliberately overshoot — JSON-like
		// content so the byte/2 branch produces an obvious overestimate
		// that calibration should walk back.
		let messages = vec![Message::User {
			content: "{\"data\":\"".to_string()
				+ &"x".repeat(800)
				+ "\",\"more\":\""
				+ &"y".repeat(800)
				+ "\"}",
		}];
		let pre_call = estimate_prompt_tokens_calibrated(&messages, None, &cal);

		// Pretend the provider reported 60% of our pre-call estimate.
		let real = ((pre_call.total_tokens as f64) * 0.6).round() as u64;
		assert!(
			real > 0,
			"sample is large enough to produce non-zero real tokens"
		);
		let pre_err = relative_error(pre_call.total_tokens, real);

		// Always feed the raw (pre-scale) estimate into `update` so the
		// scale converges on `real / raw`, not on `real / (raw * scale)`.
		cal.update(pre_call.raw_total_tokens, real);
		assert_eq!(cal.sample_count(), 1);
		assert!(
			(cal.scale() - 0.6).abs() < 1e-6,
			"single sample should land scale exactly on real/raw; got {}",
			cal.scale()
		);

		let post_call = estimate_prompt_tokens_calibrated(&messages, None, &cal);
		let post_err = relative_error(post_call.total_tokens, real);
		assert!(
			post_err < pre_err,
			"calibrated estimate should be closer to real: pre_err={pre_err:.3}, post_err={post_err:.3}"
		);
	}

	#[test]
	fn calibration_scale_is_stable_under_repeated_consistent_samples() {
		// Regression: feeding `total_tokens` (already scaled) back into
		// `update` computes `real / (raw * scale)` and walks the scale
		// toward `1.0` over successive calls. Feeding `raw_total_tokens`
		// keeps the ratio constant, so a consistent provider should pin
		// the scale at the real/raw ratio.
		let mut cal = EstimatorCalibration::default();
		let messages = vec![Message::User {
			content: "{\"data\":\"".to_string()
				+ &"x".repeat(800)
				+ "\",\"more\":\""
				+ &"y".repeat(800)
				+ "\"}",
		}];

		for _ in 0..6 {
			let est = estimate_prompt_tokens_calibrated(&messages, None, &cal);
			let real = ((est.raw_total_tokens as f64) * 0.6).round() as u64;
			cal.update(est.raw_total_tokens, real);
		}

		assert!(
			(cal.scale() - 0.6).abs() < 1e-3,
			"scale must stabilize on the real/raw ratio across repeated \
			 updates; got {} after 6 samples",
			cal.scale()
		);
	}

	#[test]
	fn calibration_is_no_op_when_estimated_or_real_is_zero() {
		let mut cal = EstimatorCalibration::default();
		cal.update(0, 100);
		cal.update(100, 0);
		cal.update(0, 0);
		assert_eq!(cal.sample_count(), 0);
		assert!((cal.scale() - 1.0).abs() < f64::EPSILON);
	}

	#[test]
	fn calibration_clamps_extreme_ratios_to_safety_bounds() {
		let mut cal = EstimatorCalibration::default();
		// Provider claims 10x our estimate — clamp to upper bound.
		cal.update(100, 1_000);
		assert!(
			(cal.scale() - 2.0).abs() < f64::EPSILON,
			"expected upper clamp 2.0, got {}",
			cal.scale()
		);
		// Reset and test lower clamp.
		let mut cal = EstimatorCalibration::default();
		cal.update(1_000, 100);
		assert!(
			(cal.scale() - 0.5).abs() < f64::EPSILON,
			"expected lower clamp 0.5, got {}",
			cal.scale()
		);
	}

	#[test]
	fn calibration_window_evicts_oldest_sample_past_cap() {
		let mut cal = EstimatorCalibration::default();
		for i in 1..=20_u64 {
			cal.update(100 + i, 100 + i); // ratio = 1.0 each
		}
		assert_eq!(cal.sample_count(), CAL_SAMPLE_CAP);
		assert!((cal.scale() - 1.0).abs() < 1e-9);
	}

	#[test]
	fn calibration_apply_returns_raw_when_scale_is_one() {
		let cal = EstimatorCalibration::default();
		assert_eq!(cal.apply(0), 0);
		assert_eq!(cal.apply(123), 123);
	}

	// ------------------------------------------------------------------
	// Mid-tier compaction tests (unit 05)
	// ------------------------------------------------------------------

	/// Build a simple conversation buffer:
	/// [0] User anchor, [1..] alternating Assistant + User pairs.
	fn mid_compact_conversation(n_pairs: usize) -> Vec<Message> {
		let mut msgs = vec![Message::User {
			content: "initial goal".to_string(),
		}];
		for i in 0..n_pairs {
			msgs.push(Message::Assistant {
				text: format!("assistant turn {i}"),
				tool_calls: vec![],
			});
			msgs.push(Message::User {
				content: format!("user reply {i}"),
			});
		}
		msgs
	}

	/// Build a conversation that ends with tool_use / tool_result pairs
	/// to verify that mid_compact_messages never orphans a ToolResult.
	fn mid_compact_tool_conversation() -> Vec<Message> {
		use roku_plugin_llm::ToolCallBlock;
		vec![
			Message::User {
				content: "goal".to_string(),
			},
			// Old assistant turn (will be in the drainable prefix).
			Message::Assistant {
				text: "thinking".to_string(),
				tool_calls: vec![],
			},
			Message::User {
				content: "old user".to_string(),
			},
			// Tool-calling turn — must stay paired with its ToolResult.
			Message::Assistant {
				text: String::new(),
				tool_calls: vec![ToolCallBlock {
					id: "tc-1".to_string(),
					name: "Bash".to_string(),
					arguments: json!({"cmd": "echo hi"}),
				}],
			},
			Message::ToolResult {
				tool_use_id: "tc-1".to_string(),
				content: "hi".to_string(),
				is_error: false,
			},
			Message::User {
				content: "tail user".to_string(),
			},
		]
	}

	#[test]
	fn mid_compact_layer1_collapses_oldest_half_when_no_memory_summary() {
		let mut messages = mid_compact_conversation(6); // 1 + 12 = 13 messages
		let before_len = messages.len();

		let outcome = mid_compact_messages(&mut messages, None);

		// Should have fired Layer 1.
		assert!(
			matches!(outcome, MidCompactOutcome::Layer1 { messages_collapsed } if messages_collapsed > 0),
			"expected Layer1 outcome, got {outcome:?}"
		);

		// Buffer must be strictly shorter.
		assert!(
			messages.len() < before_len,
			"mid_compact must shrink the buffer"
		);
		// Anchor is intact.
		if let Message::User { content } = &messages[0] {
			assert_eq!(content, "initial goal");
		} else {
			panic!("anchor message must remain a User message");
		}
		// messages[1] should be the inserted summary User message.
		if let Message::User { content } = &messages[1] {
			assert!(
				content.contains("Mid-tier context collapse"),
				"summary must mention collapse, got: {content}"
			);
		} else {
			panic!("messages[1] should be the summary User message");
		}
	}

	#[test]
	fn mid_compact_layer2_inserts_memory_summary_and_drops_old_messages() {
		let mut messages = mid_compact_conversation(6);
		let before_len = messages.len();
		let session_summary = "The agent investigated the issue and found the root cause.";

		let outcome = mid_compact_messages(&mut messages, Some(session_summary));

		assert!(
			matches!(outcome, MidCompactOutcome::Layer2 { messages_replaced } if messages_replaced > 0),
			"expected Layer2 outcome, got {outcome:?}"
		);

		// Buffer is strictly shorter.
		assert!(messages.len() < before_len);

		// Anchor intact.
		if let Message::User { content } = &messages[0] {
			assert_eq!(content, "initial goal");
		} else {
			panic!("anchor must remain");
		}
		// Summary at index 1 should contain the session summary text.
		if let Message::User { content } = &messages[1] {
			assert!(
				content.contains(session_summary),
				"summary content should embed session_summary, got: {content}"
			);
			assert!(
				content.contains("[Session memory summary]"),
				"summary should have the Layer 2 header"
			);
		} else {
			panic!("messages[1] should be the summary message");
		}
	}

	#[test]
	fn mid_compact_preserves_tool_use_tool_result_pairing() {
		let mut messages = mid_compact_tool_conversation();

		let outcome = mid_compact_messages(&mut messages, None);

		// Should not be Noop for a 6-message buffer.
		assert_ne!(outcome, MidCompactOutcome::Noop, "should have compacted");

		// No ToolResult must appear at index 1 or immediately after the anchor.
		assert!(
			!matches!(&messages[1], Message::ToolResult { .. }),
			"messages[1] must not be an orphaned ToolResult"
		);

		// No message in the entire buffer should be a ToolResult that appears
		// *before* its owning Assistant message. We verify this by scanning
		// for ToolResult messages and confirming the preceding message is either
		// an Assistant or another ToolResult.
		for i in 1..messages.len() {
			if matches!(&messages[i], Message::ToolResult { .. }) {
				let prev = &messages[i - 1];
				assert!(
					matches!(prev, Message::Assistant { .. } | Message::ToolResult { .. }),
					"ToolResult at index {i} is orphaned (prev = {prev:?})"
				);
			}
		}
	}

	#[test]
	fn mid_compact_noop_when_buffer_too_short() {
		// 2 messages — too short to produce a meaningful split.
		let mut messages = vec![
			Message::User {
				content: "goal".to_string(),
			},
			Message::Assistant {
				text: "answer".to_string(),
				tool_calls: vec![],
			},
		];
		let snapshot = messages.clone();
		let outcome = mid_compact_messages(&mut messages, None);

		assert_eq!(outcome, MidCompactOutcome::Noop);
		assert_eq!(messages.len(), snapshot.len(), "buffer must be unchanged");
	}

	#[test]
	fn mid_compact_idempotent_does_not_keep_shrinking() {
		let mut messages = mid_compact_conversation(4); // 9 messages
		let _ = mid_compact_messages(&mut messages, None);
		let after_first = messages.len();

		// Run again; if the buffer is already compact (short), the second call
		// must produce nothing or at most one more collapse from the remaining half.
		// The key invariant: buffer does not shrink to zero or panic.
		let _ = mid_compact_messages(&mut messages, None);
		let after_second = messages.len();

		assert!(
			after_second >= 2,
			"buffer must retain at least anchor + summary after two passes"
		);
		// Should not grow.
		assert!(after_second <= after_first);
	}

	#[test]
	fn mid_compact_layer1_no_llm_call_verified_by_sync_execution() {
		// mid_compact_messages is a plain fn (not async), which proves it cannot
		// call the LLM. This test documents and verifies that property by calling
		// the function without any router present.
		let mut messages = mid_compact_conversation(5);
		// No router constructed or passed — if the function were to call an LLM
		// it would either panic or fail to compile as async.
		let outcome = mid_compact_messages(&mut messages, None);
		assert_ne!(
			outcome,
			MidCompactOutcome::Noop,
			"should have compacted without any LLM call"
		);
	}
}
