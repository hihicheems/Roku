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

/// Runtime-layer events emitted by `execute_tool_loop` via an optional event sender.
///
/// All variants are `Send + 'static` so the sender can cross async task boundaries.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum LoopEvent {
	/// A tool invocation is about to begin.
	ToolStart {
		/// Step index (1-based, matches `StepRecord::step_index`).
		step: u32,
		tool_name: String,
		/// Human-readable summary of the tool arguments (≤80 chars).
		#[serde(skip_serializing_if = "Option::is_none")]
		args_summary: Option<String>,
		/// Identifies the agent that emitted this event. `None` = top-level.
		#[serde(skip_serializing_if = "Option::is_none")]
		agent_id: Option<String>,
	},
	/// A tool invocation completed (successfully or with an error).
	ToolEnd {
		step: u32,
		tool_name: String,
		/// Wall-clock duration in milliseconds, if available.
		elapsed_ms: Option<u64>,
		/// Human-readable summary of the tool result (≤80 chars).
		#[serde(skip_serializing_if = "Option::is_none")]
		result_summary: Option<String>,
		/// Identifies the agent that emitted this event. `None` = top-level.
		#[serde(skip_serializing_if = "Option::is_none")]
		agent_id: Option<String>,
	},
	/// Context compaction was triggered after this step.
	CompactTriggered {
		step: u32,
		/// Estimated token count that caused the trigger.
		estimated_tokens: u64,
	},
	/// Context compaction completed.
	CompactComplete {
		step: u32,
		/// Whether LLM-assisted summarization succeeded (`false` = mechanical fallback).
		llm_succeeded: bool,
		/// Wall-clock duration of the compaction in milliseconds.
		elapsed_ms: u64,
	},
	/// Reactive compaction was triggered because the provider returned
	/// `context_window_exceeded`. Emitted once per turn before the retry.
	ReactiveCompactTriggered {
		step: u32,
		/// Provider-reported detail (e.g. "prompt is too long: 215321 tokens > 200000").
		detail: String,
	},
	/// Layer 0 microcompaction ran during pre-flight before this LLM call.
	///
	/// `freed_tokens` is the calibrated estimate of tokens released by
	/// replacing historical tool result content with the placeholder. A value
	/// of `0` means there were no eligible historical tool results to clear.
	/// Schema is frozen once introduced — additive fields only.
	MicrocompactRan { step: u32, freed_tokens: u64 },
	/// Layer 2 mid-tier compaction consumed a pre-existing session memory
	/// summary.
	///
	/// Emitted when the mid-water threshold is crossed and the caller supplied
	/// a summary that was spliced into the message buffer, replacing the
	/// historical segment it covers. `messages_replaced` is the number of
	/// messages drained from the buffer. The event variant itself discriminates
	/// Layer 2 from Layer 1; the source-of-truth for which subsystem produced
	/// the summary is the call site, not this event (additive fields only if
	/// that needs to change).
	MidCompactLayer2Ran { step: u32, messages_replaced: usize },
	/// Layer 1 mid-tier compaction ran a deterministic context collapse.
	///
	/// Emitted when the mid-water threshold is crossed and Layer 2 produced
	/// nothing (no memory summary available). `messages_collapsed` is the
	/// number of messages replaced by the mechanical summary insertion.
	/// Schema is stable — additive fields only.
	MidCompactLayer1Ran {
		step: u32,
		messages_collapsed: usize,
	},
	/// The structured LLM summarizer (Layer 3) was invoked for auto-compaction.
	///
	/// Emitted whether the call succeeded or failed; `succeeded=false` means
	/// the runtime fell back to mechanical compaction for this step.
	/// `drop_oldest_retries` reports how many times the summarizer had to
	/// discard older history before the provider accepted the prompt.
	AutoCompactSummarizerCalled {
		step: u32,
		prompt_tokens: u64,
		output_tokens: u64,
		succeeded: bool,
		drop_oldest_retries: u32,
	},
	/// The auto-compact circuit breaker tripped after consecutive failures.
	///
	/// After this event, further auto-compaction attempts are suppressed for
	/// the remainder of the run and the runtime falls back to mechanical
	/// compaction only.
	AutoCompactCircuitBreakerTripped {
		step: u32,
		consecutive_failures: u32,
	},
	/// Incremental text from the LLM during the decision phase.
	LlmTextDelta {
		step: u32,
		text: String,
		/// Identifies the agent that emitted this event. `None` = top-level.
		#[serde(skip_serializing_if = "Option::is_none")]
		agent_id: Option<String>,
	},
	/// The LLM finished producing its decision for this step.
	LlmDecisionComplete { step: u32 },
	/// One full loop iteration (decide + optional tool execution) is complete.
	StepComplete { step: u32 },
	/// Token usage summary emitted at the end of each tool loop execution.
	TokenUsage {
		step: u32,
		prompt_tokens: u64,
		output_tokens: u64,
		total_tokens: u64,
		estimated_cost_usd: f64,
		/// The model that served this request. For OpenRouter this is the
		/// actually-served model, which may differ from the configured primary.
		#[serde(skip_serializing_if = "Option::is_none")]
		model_id: Option<String>,
		/// Input tokens newly written to the provider's prompt cache this
		/// turn. Always `0` for OpenAI (no write counter); populated from
		/// Anthropic's `cache_creation_input_tokens`. Defaults to `0` when
		/// legacy emitters omit the field.
		#[serde(default)]
		cache_creation_input_tokens: u64,
		/// Input tokens served from the provider's prompt cache this turn.
		/// Populated from Anthropic's `cache_read_input_tokens` and OpenAI's
		/// `input_tokens_details.cached_tokens` / `prompt_tokens_details.cached_tokens`.
		/// Defaults to `0` when the provider did not report any cache hit.
		#[serde(default)]
		cache_read_input_tokens: u64,
	},
	/// Calibration sample emitted after each successful LLM call.
	///
	/// Pairs the byte-based estimator's pre-call output with the provider's
	/// reported `usage.prompt_tokens`. Surfaces `estimated_prompt_tokens`
	/// alongside `prompt_tokens` so trace consumers can verify the unit 01
	/// accuracy gates (≤20% error for English/code, ≤30% for CJK).
	EstimatorCalibrated {
		step: u32,
		estimated_prompt_tokens: u64,
		prompt_tokens: u64,
		/// Effective scale factor in use after this sample is folded in.
		scale: f64,
	},
	/// A cache break was detected: `cache_read_input_tokens` dropped
	/// significantly relative to the session baseline.
	///
	/// Emitted alongside a diagnostic file written to
	/// `~/.roku/diagnostics/cache-break-<ts>.txt`. The `component_changed`
	/// field identifies which prefix component diverged from the previous
	/// turn's fingerprint (e.g. `"system_prompt"`, `"tool_schema"`,
	/// `"model"`). Schema is frozen once introduced — additive fields only.
	CacheBreakDetected {
		step: u32,
		/// Human-readable explanation of the break.
		reason: String,
		/// `cache_read_input_tokens` drop relative to session baseline.
		tokens_lost: u64,
		/// Which fingerprint components changed (may be empty if the
		/// fingerprint was identical and the break is message-level).
		component_changed: Vec<String>,
		/// Path to the diagnostic file, if it was written successfully.
		#[serde(skip_serializing_if = "Option::is_none")]
		diagnostic_path: Option<String>,
	},
}

/// Convenience alias for the sending half of a `LoopEvent` channel.
pub type LoopEventSender = tokio::sync::mpsc::UnboundedSender<LoopEvent>;

// ---------------------------------------------------------------------------
// Tool summary helpers
// ---------------------------------------------------------------------------

/// Extract a human-readable summary (≤80 chars) from tool arguments JSON.
pub fn summarize_tool_args(tool_name: &str, args: &serde_json::Value) -> Option<String> {
	let s = match tool_name {
		"Read" => {
			let path = json_str(args, "path").unwrap_or_default();
			short_path(path)
		}
		"Edit" => {
			let path = json_str(args, "file_path").unwrap_or_default();
			short_path(path)
		}
		"Write" => {
			let path = json_str(args, "file_path").unwrap_or_default();
			short_path(path)
		}
		"Bash" => {
			let cmd = json_str(args, "command").unwrap_or_default();
			truncate(cmd, 72)
		}
		"Grep" => {
			let pattern = json_str(args, "pattern").unwrap_or_default();
			let path = json_str(args, "path").unwrap_or(".");
			format!("\"{}\" in {}", truncate(pattern, 40), short_path(path))
		}
		"Glob" => {
			let pattern = json_str(args, "pattern").unwrap_or_default();
			truncate(pattern, 72)
		}
		"Agent" => {
			let desc = json_str(args, "description").unwrap_or_default();
			truncate(desc, 72)
		}
		_ => return None,
	};
	if s.is_empty() { None } else { Some(s) }
}

/// Extract a human-readable summary (≤80 chars) from a tool observation.
pub fn summarize_tool_result(
	tool_name: &str,
	ok: bool,
	data: &serde_json::Value,
) -> Option<String> {
	if !ok {
		let msg = json_str(data, "error")
			.or_else(|| json_str(data, "message"))
			.unwrap_or("failed");
		return Some(format!("error: {}", truncate(msg, 72)));
	}
	let s = match tool_name {
		"Read" => {
			let size = data.get("size").and_then(|v| v.as_u64());
			let lines = data
				.get("content")
				.and_then(|v| v.as_str())
				.map(|c| c.lines().count());
			match (lines, size) {
				(Some(l), Some(sz)) => format!("{l} lines, {}KB", sz / 1024),
				(Some(l), None) => format!("{l} lines"),
				_ => String::new(),
			}
		}
		"Bash" => {
			let exit = data.get("exit_code").and_then(|v| v.as_i64()).unwrap_or(-1);
			let stdout = data.get("stdout").and_then(|v| v.as_str()).unwrap_or("");
			let lines = stdout.lines().count();
			format!("exit {exit} ({lines} lines)")
		}
		"Grep" => {
			let count = data
				.get("match_count")
				.and_then(|v| v.as_u64())
				.unwrap_or(0);
			let files = data
				.get("matches")
				.and_then(|v| v.as_array())
				.map(|arr| {
					let mut seen = std::collections::HashSet::new();
					for m in arr {
						if let Some(f) = m.get("file_path").and_then(|v| v.as_str()) {
							seen.insert(f);
						}
					}
					seen.len()
				})
				.unwrap_or(0);
			format!("{count} matches in {files} files")
		}
		"Edit" => {
			let mc = data.get("match_count").and_then(|v| v.as_u64());
			match mc {
				Some(n) => format!("{n} replacement(s)"),
				None => "applied".to_string(),
			}
		}
		"Write" => "written".to_string(),
		_ => return None,
	};
	if s.is_empty() { None } else { Some(s) }
}

fn json_str<'a>(v: &'a serde_json::Value, key: &str) -> Option<&'a str> {
	v.get(key).and_then(|v| v.as_str())
}

fn short_path(path: &str) -> String {
	// Show just the filename (or last 2 components for context).
	let parts: Vec<&str> = path.rsplit('/').take(2).collect();
	match parts.len() {
		2 => format!("{}/{}", parts[1], parts[0]),
		1 => parts[0].to_string(),
		_ => path.to_string(),
	}
}

fn truncate(s: &str, max: usize) -> String {
	if s.len() <= max {
		return s.to_string();
	}
	// Find the last char boundary at or before `max` to avoid panicking
	// on multi-byte UTF-8 (CJK, emoji, accented chars).
	let end = s
		.char_indices()
		.map(|(i, _)| i)
		.take_while(|&i| i <= max)
		.last()
		.unwrap_or(0);
	format!("{}…", &s[..end])
}
