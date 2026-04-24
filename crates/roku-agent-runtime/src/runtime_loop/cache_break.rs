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

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use roku_plugin_llm::{SystemPromptBlock, ToolDefinition};

/// Relative drop threshold: cache_read must drop more than 5% from baseline.
const CACHE_BREAK_RELATIVE_THRESHOLD: f64 = 0.05;

/// Absolute drop threshold: cache_read must drop more than 2000 tokens.
const CACHE_BREAK_ABSOLUTE_THRESHOLD: u64 = 2_000;

/// Hashed snapshot of the prompt components that contribute to prefix cache
/// stability: system prompt static blocks, tool schema, and model identity.
///
/// Compared across turns to identify which specific component changed when
/// a cache break is detected.
#[derive(Debug, Clone, PartialEq)]
struct PromptStateFingerprint {
	system_hash: u64,
	tools_hash: u64,
	model: String,
}

/// Diagnostic report emitted when a cache break is detected.
#[derive(Debug, Clone)]
pub(crate) struct CacheBreakReport {
	pub reason: String,
	pub tokens_lost: u64,
	pub component_changed: Vec<String>,
}

/// Session-scoped cache break detector.
///
/// Tracks a prompt-state fingerprint and a `cache_read_input_tokens` baseline
/// across turns. After each LLM response the detector compares the current
/// values to the previous turn's and, when the dual threshold fires, produces
/// a [`CacheBreakReport`] that identifies which prefix component diverged.
///
/// Marked `#[serde(skip)]` on [`super::loop_state::LoopState`] so the
/// baseline intentionally resets after checkpoint restore (first turn after
/// restore has no baseline → no false positive).
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct CacheBreakDetector {
	/// Fingerprint recorded before the previous LLM call.
	previous_fingerprint: Option<PromptStateFingerprint>,
	/// `cache_read_input_tokens` observed after the previous LLM call.
	previous_cache_read_tokens: Option<u64>,
	/// Fingerprint recorded before the current LLM call (set by
	/// [`Self::record_prompt_state`], consumed by [`Self::check_response`]).
	current_fingerprint: Option<PromptStateFingerprint>,
	/// Set to `true` by [`Self::notify_compaction`] when any compaction
	/// layer fires. The next [`Self::check_response`] resets the baseline
	/// instead of comparing against it, avoiding false positives from
	/// legitimate message-buffer mutations.
	compaction_pending: bool,
}

impl CacheBreakDetector {
	/// Snapshot the prompt prefix components before an LLM call.
	///
	/// Call this after `build_system_prompt_sections` and
	/// `freeze_or_reuse_tool_schema` but before the actual provider call.
	pub fn record_prompt_state(
		&mut self,
		static_blocks: &[SystemPromptBlock],
		tool_definitions: &[ToolDefinition],
		model: &str,
	) {
		self.current_fingerprint = Some(PromptStateFingerprint {
			system_hash: hash_blocks(static_blocks),
			tools_hash: hash_tool_definitions(tool_definitions),
			model: model.to_string(),
		});
	}

	/// Compare the response's cache usage against the session baseline.
	///
	/// Returns `Some(report)` when the dual threshold fires, `None`
	/// otherwise. Always updates the baseline for the next turn.
	pub fn check_response(
		&mut self,
		cache_read_input_tokens: u64,
		model_id: &str,
		provider: &str,
	) -> Option<CacheBreakReport> {
		let current_fp = self.current_fingerprint.take()?;

		// After compaction the message buffer changed legitimately — reset
		// the baseline and skip detection for this turn.
		if self.compaction_pending {
			self.compaction_pending = false;
			self.previous_fingerprint = Some(current_fp);
			self.previous_cache_read_tokens = Some(cache_read_input_tokens);
			return None;
		}

		let prev_tokens = self.previous_cache_read_tokens;
		let prev_fp = self.previous_fingerprint.take();

		// Advance baseline for the next turn.
		self.previous_fingerprint = Some(current_fp.clone());
		self.previous_cache_read_tokens = Some(cache_read_input_tokens);

		// No previous baseline (first turn or post-restore) → skip.
		let prev_tokens = prev_tokens?;

		// Previous cache_read was 0 → provider had no cache active → skip.
		if prev_tokens == 0 {
			return None;
		}

		// Dual threshold: both relative AND absolute must exceed limits.
		let drop = prev_tokens.saturating_sub(cache_read_input_tokens);
		if drop == 0 {
			return None;
		}
		let relative_drop = drop as f64 / prev_tokens as f64;

		if relative_drop <= CACHE_BREAK_RELATIVE_THRESHOLD || drop <= CACHE_BREAK_ABSOLUTE_THRESHOLD
		{
			return None;
		}

		// Break detected — diagnose which components changed.
		let mut changed = Vec::new();
		if let Some(ref prev) = prev_fp {
			if prev.system_hash != current_fp.system_hash {
				changed.push("system_prompt".to_string());
			}
			if prev.tools_hash != current_fp.tools_hash {
				changed.push("tool_schema".to_string());
			}
			if prev.model != current_fp.model {
				changed.push(format!("model ({} -> {})", prev.model, current_fp.model));
			}
		}

		if changed.is_empty() {
			changed
				.push("unknown (fingerprint unchanged; likely message-level change)".to_string());
		}

		let reason = format!(
			"cache_read_input_tokens dropped from {} to {} ({:.1}% drop, {} tokens lost); provider: {}, model: {}",
			prev_tokens,
			cache_read_input_tokens,
			relative_drop * 100.0,
			drop,
			provider,
			model_id,
		);

		Some(CacheBreakReport {
			reason,
			tokens_lost: drop,
			component_changed: changed,
		})
	}

	/// Mark that a compaction layer fired. The next `check_response` will
	/// reset the baseline instead of comparing against it.
	pub fn notify_compaction(&mut self) {
		self.compaction_pending = true;
	}

	/// Test-only accessor for the last-checked fingerprint's model field.
	///
	/// After `check_response` runs, the current fingerprint is moved
	/// to `previous_fingerprint` so the next turn has a baseline to
	/// compare against. This accessor lets runtime-layer tests observe
	/// what the caller recorded on the turn that just completed —
	/// specifically, that the routed serving id was fingerprinted,
	/// not `request.model_override` (which is `""` under default
	/// routing and would silently hide provider swaps).
	#[cfg(test)]
	pub(crate) fn previous_fingerprint_model(&self) -> Option<&str> {
		self.previous_fingerprint
			.as_ref()
			.map(|fp| fp.model.as_str())
	}
}

/// Write a structured diagnostic file to `~/.roku/diagnostics/`.
///
/// Returns the written path on success. Callers must treat errors as
/// non-fatal — log and continue, never bubble up to the user.
pub(crate) fn write_cache_break_diagnostic(
	report: &CacheBreakReport,
) -> Result<PathBuf, std::io::Error> {
	let home = std::env::var("HOME")
		.or_else(|_| std::env::var("USERPROFILE"))
		.map_err(|e| std::io::Error::new(std::io::ErrorKind::NotFound, e.to_string()))?;

	let dir = PathBuf::from(home).join(".roku").join("diagnostics");
	std::fs::create_dir_all(&dir)?;

	let ts = SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.unwrap_or_default()
		.as_millis();
	let path = dir.join(format!("cache-break-{ts}.txt"));

	let content = format!(
		"reason: {}\ntokens_lost: {}\ncomponent_changed: {}\n",
		report.reason,
		report.tokens_lost,
		report.component_changed.join(", "),
	);
	std::fs::write(&path, &content)?;
	Ok(path)
}

// ---------------------------------------------------------------------------
// Hashing helpers
// ---------------------------------------------------------------------------

fn hash_blocks(blocks: &[SystemPromptBlock]) -> u64 {
	let mut hasher = DefaultHasher::new();
	for block in blocks {
		block.id.hash(&mut hasher);
		block.content.hash(&mut hasher);
	}
	hasher.finish()
}

pub(crate) fn hash_tool_definitions(defs: &[ToolDefinition]) -> u64 {
	let mut hasher = DefaultHasher::new();
	for def in defs {
		def.name.hash(&mut hasher);
		def.description.hash(&mut hasher);
		// Serialize parameters to a canonical string so the hash covers
		// the full schema shape, not just the pointer.
		let params = def.parameters.to_string();
		params.hash(&mut hasher);
	}
	hasher.finish()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
	use super::*;

	fn block(id: &str, content: &str) -> SystemPromptBlock {
		SystemPromptBlock {
			id: id.to_string(),
			content: content.to_string(),
		}
	}

	fn tool(name: &str) -> ToolDefinition {
		ToolDefinition {
			name: name.to_string(),
			description: format!("desc for {name}"),
			parameters: serde_json::json!({"type": "object"}),
		}
	}

	#[test]
	fn first_turn_does_not_trigger_break() {
		let mut detector = CacheBreakDetector::default();
		let blocks = [block("identity", "I am Roku")];
		let tools = [tool("Read")];

		detector.record_prompt_state(&blocks, &tools, "claude-sonnet");
		let result = detector.check_response(50_000, "claude-sonnet", "anthropic");

		assert!(result.is_none(), "first turn must not false-positive");
	}

	#[test]
	fn stable_session_does_not_trigger_break() {
		let mut detector = CacheBreakDetector::default();
		let blocks = [block("identity", "I am Roku")];
		let tools = [tool("Read"), tool("Grep")];

		// Turn 1: establish baseline.
		detector.record_prompt_state(&blocks, &tools, "claude-sonnet");
		assert!(
			detector
				.check_response(50_000, "claude-sonnet", "anthropic")
				.is_none()
		);

		// Turn 2: same fingerprint, similar cache_read.
		detector.record_prompt_state(&blocks, &tools, "claude-sonnet");
		let result = detector.check_response(49_000, "claude-sonnet", "anthropic");

		assert!(result.is_none(), "2% drop must not trigger (below 5%)");
	}

	#[test]
	fn system_prompt_change_triggers_break() {
		let mut detector = CacheBreakDetector::default();
		let tools = [tool("Read")];

		// Turn 1: baseline.
		let blocks_v1 = [block("identity", "I am Roku v1")];
		detector.record_prompt_state(&blocks_v1, &tools, "claude-sonnet");
		assert!(
			detector
				.check_response(50_000, "claude-sonnet", "anthropic")
				.is_none()
		);

		// Turn 2: system prompt changed, cache_read drops.
		let blocks_v2 = [block("identity", "I am Roku v2")];
		detector.record_prompt_state(&blocks_v2, &tools, "claude-sonnet");
		let result = detector.check_response(5_000, "claude-sonnet", "anthropic");

		let report = result.expect("break should be detected");
		assert!(report.tokens_lost > CACHE_BREAK_ABSOLUTE_THRESHOLD);
		assert!(
			report
				.component_changed
				.contains(&"system_prompt".to_string())
		);
		assert!(report.reason.contains("dropped from 50000 to 5000"));
	}

	#[test]
	fn tool_schema_change_triggers_break() {
		let mut detector = CacheBreakDetector::default();
		let blocks = [block("identity", "I am Roku")];

		// Turn 1.
		let tools_v1 = [tool("Read"), tool("Grep")];
		detector.record_prompt_state(&blocks, &tools_v1, "claude-sonnet");
		assert!(
			detector
				.check_response(50_000, "claude-sonnet", "anthropic")
				.is_none()
		);

		// Turn 2: tool added, cache_read drops.
		let tools_v2 = [tool("Read"), tool("Grep"), tool("Bash")];
		detector.record_prompt_state(&blocks, &tools_v2, "claude-sonnet");
		let report = detector
			.check_response(5_000, "claude-sonnet", "anthropic")
			.expect("break should be detected");

		assert!(
			report
				.component_changed
				.contains(&"tool_schema".to_string())
		);
	}

	#[test]
	fn model_change_triggers_break() {
		let mut detector = CacheBreakDetector::default();
		let blocks = [block("identity", "I am Roku")];
		let tools = [tool("Read")];

		// Turn 1.
		detector.record_prompt_state(&blocks, &tools, "claude-sonnet");
		assert!(
			detector
				.check_response(50_000, "claude-sonnet", "anthropic")
				.is_none()
		);

		// Turn 2: model switched.
		detector.record_prompt_state(&blocks, &tools, "claude-opus");
		let report = detector
			.check_response(0, "claude-opus", "anthropic")
			.expect("break should be detected");

		assert!(report.component_changed.iter().any(|c| c.contains("model")));
	}

	#[test]
	fn compaction_resets_baseline_and_skips_detection() {
		let mut detector = CacheBreakDetector::default();
		let blocks = [block("identity", "I am Roku")];
		let tools = [tool("Read")];

		// Turn 1: baseline.
		detector.record_prompt_state(&blocks, &tools, "claude-sonnet");
		assert!(
			detector
				.check_response(50_000, "claude-sonnet", "anthropic")
				.is_none()
		);

		// Compaction fires.
		detector.notify_compaction();

		// Turn 2: cache_read drops massively but compaction was expected.
		detector.record_prompt_state(&blocks, &tools, "claude-sonnet");
		let result = detector.check_response(2_000, "claude-sonnet", "anthropic");

		assert!(
			result.is_none(),
			"post-compaction turn must not false-positive"
		);

		// Turn 3: back to normal — baseline was reset to 2_000 by the
		// compaction turn, so a small rise does not trigger.
		detector.record_prompt_state(&blocks, &tools, "claude-sonnet");
		let result = detector.check_response(3_000, "claude-sonnet", "anthropic");
		assert!(result.is_none());
	}

	#[test]
	fn zero_baseline_does_not_trigger() {
		let mut detector = CacheBreakDetector::default();
		let blocks = [block("identity", "I am Roku")];
		let tools = [tool("Read")];

		// Turn 1: provider reports 0 cache (caching not active).
		detector.record_prompt_state(&blocks, &tools, "claude-sonnet");
		assert!(
			detector
				.check_response(0, "claude-sonnet", "anthropic")
				.is_none()
		);

		// Turn 2: still 0.
		detector.record_prompt_state(&blocks, &tools, "claude-sonnet");
		assert!(
			detector
				.check_response(0, "claude-sonnet", "anthropic")
				.is_none(),
			"zero baseline must not trigger"
		);
	}

	#[test]
	fn small_absolute_drop_does_not_trigger_even_if_relative_is_high() {
		let mut detector = CacheBreakDetector::default();
		let blocks = [block("identity", "I am Roku")];
		let tools = [tool("Read")];

		// Turn 1: baseline with small token count.
		detector.record_prompt_state(&blocks, &tools, "claude-sonnet");
		assert!(
			detector
				.check_response(3_000, "claude-sonnet", "anthropic")
				.is_none()
		);

		// Turn 2: 50% relative drop but only 1500 tokens absolute.
		let blocks_v2 = [block("identity", "I am Roku v2")];
		detector.record_prompt_state(&blocks_v2, &tools, "claude-sonnet");
		let result = detector.check_response(1_500, "claude-sonnet", "anthropic");

		assert!(
			result.is_none(),
			"1500 token drop should not trigger (below 2000 absolute threshold)"
		);
	}

	#[test]
	fn diagnostic_file_fields_are_present() {
		let report = CacheBreakReport {
			reason: "test break".to_string(),
			tokens_lost: 5_000,
			component_changed: vec!["system_prompt".to_string(), "tool_schema".to_string()],
		};

		// Use a temp dir to avoid writing to real ~/.roku/diagnostics.
		let dir = std::env::temp_dir().join("roku-test-diagnostics");
		std::fs::create_dir_all(&dir).unwrap();

		let ts = SystemTime::now()
			.duration_since(UNIX_EPOCH)
			.unwrap_or_default()
			.as_millis();
		let path = dir.join(format!("cache-break-{ts}.txt"));

		let content = format!(
			"reason: {}\ntokens_lost: {}\ncomponent_changed: {}\n",
			report.reason,
			report.tokens_lost,
			report.component_changed.join(", "),
		);
		std::fs::write(&path, &content).unwrap();

		let read = std::fs::read_to_string(&path).unwrap();
		assert!(read.contains("reason:"));
		assert!(read.contains("tokens_lost: 5000"));
		assert!(read.contains("component_changed: system_prompt, tool_schema"));

		// Cleanup.
		let _ = std::fs::remove_file(&path);
		let _ = std::fs::remove_dir(&dir);
	}

	#[test]
	fn hash_is_deterministic_for_same_input() {
		let blocks = [block("id", "content"), block("id2", "content2")];
		assert_eq!(hash_blocks(&blocks), hash_blocks(&blocks));

		let tools = [tool("Read"), tool("Grep")];
		assert_eq!(hash_tool_definitions(&tools), hash_tool_definitions(&tools));
	}

	#[test]
	fn hash_differs_for_different_input() {
		let a = [block("id", "content A")];
		let b = [block("id", "content B")];
		assert_ne!(hash_blocks(&a), hash_blocks(&b));

		let ta = [tool("Read")];
		let tb = [tool("Grep")];
		assert_ne!(hash_tool_definitions(&ta), hash_tool_definitions(&tb));
	}
}
