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

/// Maximum characters for a tool result before truncation in the turn loop.
pub(crate) const MAX_TOOL_RESULT_CHARS: usize = 80_000;

/// Truncate a tool result to head + tail with an informative note.
pub(crate) fn truncate_tool_result_for_message(content: &str, max_chars: usize) -> String {
	if content.len() <= max_chars {
		return content.to_string();
	}
	let head_chars = max_chars * 4 / 5;
	let tail_chars = max_chars / 5;
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
	let total_lines = content.lines().count();
	format!(
		"{}\n\n[Output truncated: showing first and last portions of {} total lines ({} chars). \
         Ask the user or use a more specific query to narrow results.]\n\n{}",
		&content[..head_end],
		total_lines,
		content.len(),
		&content[tail_start..],
	)
}

/// Truncate a raw tool output `Value` so it fits within `MAX_TOOL_RESULT_CHARS`.
pub(crate) fn truncate_raw_tool_output(value: Value) -> Value {
	match &value {
		Value::String(s) if s.len() > MAX_TOOL_RESULT_CHARS => {
			Value::String(truncate_tool_result_for_message(s, MAX_TOOL_RESULT_CHARS))
		}
		Value::Object(_) | Value::Array(_) => {
			let serialized = serde_json::to_string(&value).unwrap_or_default();
			if serialized.len() > MAX_TOOL_RESULT_CHARS {
				Value::String(truncate_tool_result_for_message(
					&serialized,
					MAX_TOOL_RESULT_CHARS,
				))
			} else {
				value
			}
		}
		_ => value,
	}
}
