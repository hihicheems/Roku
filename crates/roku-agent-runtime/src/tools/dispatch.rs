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

//! Tool execution dispatch helpers.
//!
//! Free functions extracted from runtime.rs for tool invocation,
//! result processing, and observation building.

use roku_common_types::{ResourceSelector, ResultEnvelope, ResultStatus};
use roku_plugin_tools::{ResourceCatalog, ResourceKind};
use serde_json::Value;

use crate::runtime_loop::ToolObservation;

/// Look up a `ResourceSelector` by tool name from the catalog.
pub(crate) fn tool_selector_by_name(
	catalog: &ResourceCatalog,
	tool_name: &str,
) -> Option<ResourceSelector> {
	catalog
		.entries()
		.iter()
		.find(|entry| entry.kind == ResourceKind::Tool && entry.name == tool_name)
		.map(|entry| entry.selector.clone())
}

/// Extract the elapsed_ms field from a result envelope payload.
pub(crate) fn execution_elapsed_ms(result: &ResultEnvelope) -> Option<u64> {
	serde_json::from_str::<Value>(&result.payload)
		.ok()
		.and_then(|payload| payload.get("elapsed_ms").and_then(Value::as_u64))
}

/// Extract the raw tool output from a result envelope.
pub(crate) fn raw_tool_output_from_result(result: &ResultEnvelope) -> Value {
	let payload = serde_json::from_str::<Value>(&result.payload)
		.unwrap_or_else(|_| serde_json::json!({ "message": result.payload.clone() }));
	if result.status == ResultStatus::Ok {
		return payload
			.get("output")
			.cloned()
			.unwrap_or_else(|| payload.clone());
	}
	payload
}

/// Build a `ToolObservation` from a tool execution result.
pub(crate) fn observation_from_execution(
	tool_name: &str,
	result: &ResultEnvelope,
	catalog: &ResourceCatalog,
) -> ToolObservation {
	let payload = serde_json::from_str::<Value>(&result.payload)
		.unwrap_or_else(|_| serde_json::json!({ "message": result.payload.clone() }));
	// general.execute has been removed; observations from all tools are returned as-is.
	if result.status == ResultStatus::Ok {
		ToolObservation::from_result_payload(tool_name, &payload)
	} else {
		ToolObservation::from_error_payload(tool_name, &payload, catalog)
	}
}

/// Per-tool output cap in characters.
pub(crate) fn tool_result_cap(tool_name: &str) -> usize {
	match tool_name {
		"Bash" => 30_000,
		"Grep" => 20_000,
		"Glob" => 10_000, // ~100 file paths
		"WebFetch" => 25_000,
		"WebSearch" => 25_000,
		// Read is handled separately — never truncated, returns error instead
		"Read" => TOOL_CAP_NO_TRUNCATE,
		_ => 50_000, // default for all other tools
	}
}

/// Sentinel value indicating a tool uses error-based overflow handling
/// instead of truncation.
pub(crate) const TOOL_CAP_NO_TRUNCATE: usize = usize::MAX;

/// Maximum characters for a tool result before truncation in the turn loop.
///
/// Deprecated: use `tool_result_cap(tool_name)` for per-tool caps.
#[allow(dead_code)]
pub(crate) const MAX_TOOL_RESULT_CHARS: usize = 80_000;

/// Truncate a tool result to head + tail with a stable middle marker.
pub(crate) fn truncate_tool_result_for_message(content: &str, max_chars: usize) -> String {
	if max_chars == TOOL_CAP_NO_TRUNCATE || content.len() <= max_chars {
		return content.to_string();
	}
	let total_chars = content.chars().count();
	// For content whose byte length exceeds the cap but char count does not
	// (common with multibyte UTF-8), return the full content unmodified.
	if total_chars <= max_chars {
		return content.to_string();
	}
	// Reserve space for the marker (approx 40 chars for the marker text).
	let marker_reserve = 40;
	let available = max_chars.saturating_sub(marker_reserve);
	let head_chars = available / 2;
	let tail_chars = available - head_chars;
	let head_end = content
		.char_indices()
		.nth(head_chars)
		.map(|(i, _)| i)
		.unwrap_or(content.len());
	let tail_start = content
		.char_indices()
		.rev()
		.nth(tail_chars.saturating_sub(1))
		.map(|(i, _)| i)
		.unwrap_or(0);
	// Guard against overlap when head and tail meet or cross (possible when
	// char count barely exceeds available).
	if head_end >= tail_start {
		return content.to_string();
	}
	let truncated_chars = total_chars - head_chars - tail_chars;
	format!(
		"{}\n\n…{} chars truncated…\n\n{}",
		&content[..head_end],
		truncated_chars,
		&content[tail_start..],
	)
}

/// Truncate a raw tool output `Value` so it fits within the given cap.
pub(crate) fn truncate_raw_tool_output(value: Value, max_chars: usize) -> Value {
	if max_chars == TOOL_CAP_NO_TRUNCATE {
		return value;
	}
	match &value {
		Value::String(s) if s.len() > max_chars => {
			Value::String(truncate_tool_result_for_message(s, max_chars))
		}
		Value::Object(_) | Value::Array(_) => {
			let serialized = serde_json::to_string(&value).unwrap_or_default();
			if serialized.len() > max_chars {
				Value::String(truncate_tool_result_for_message(&serialized, max_chars))
			} else {
				value
			}
		}
		_ => value,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn per_tool_caps_are_within_bounds() {
		assert_eq!(tool_result_cap("Bash"), 30_000);
		assert_eq!(tool_result_cap("Grep"), 20_000);
		assert_eq!(tool_result_cap("Glob"), 10_000);
		assert_eq!(tool_result_cap("Read"), TOOL_CAP_NO_TRUNCATE);
		assert_eq!(tool_result_cap("UnknownTool"), 50_000);
	}

	#[test]
	fn truncation_preserves_head_and_tail() {
		let content = "A".repeat(1000);
		let result = truncate_tool_result_for_message(&content, 200);
		assert!(result.contains('…'));
		assert!(result.contains("chars truncated"));
		assert!(result.len() <= 300); // some overhead for marker
		// Verify head and tail are present
		assert!(result.starts_with("AAAA"));
		assert!(result.ends_with("AAAA"));
	}

	#[test]
	fn no_truncation_below_cap() {
		let content = "short content";
		let result = truncate_tool_result_for_message(content, 50_000);
		assert_eq!(result, content);
	}

	#[test]
	fn fileread_cap_is_no_truncate() {
		let long_content = "X".repeat(200_000);
		let result = truncate_tool_result_for_message(&long_content, TOOL_CAP_NO_TRUNCATE);
		assert_eq!(result, long_content);
	}

	#[test]
	fn multibyte_content_does_not_underflow() {
		// 10000 CJK chars = 30000 bytes. Cap is 20000 (bytes > cap but
		// char count < cap). Should return content unmodified.
		let content = "你".repeat(10_000);
		assert_eq!(content.len(), 30_000); // 3 bytes each
		let result = truncate_tool_result_for_message(&content, 20_000);
		assert_eq!(result, content);
	}

	#[test]
	fn multibyte_truncation_produces_valid_marker() {
		// 40000 CJK chars = 120000 bytes. Cap is 20000. Char count (40000)
		// exceeds cap, so truncation should fire and produce a valid marker.
		let content = "你".repeat(40_000);
		let result = truncate_tool_result_for_message(&content, 20_000);
		assert!(result.contains("chars truncated"));
		// Verify no panic occurred and result is valid UTF-8.
		assert!(result.len() < content.len());
	}

	#[test]
	fn truncate_raw_respects_per_tool_cap() {
		let long_string = Value::String("Z".repeat(40_000));
		let result = truncate_raw_tool_output(long_string, 30_000);
		match result {
			Value::String(s) => {
				assert!(s.len() <= 31_000); // cap + marker overhead
				assert!(s.contains("chars truncated"));
			}
			_ => panic!("expected string"),
		}
	}
}
