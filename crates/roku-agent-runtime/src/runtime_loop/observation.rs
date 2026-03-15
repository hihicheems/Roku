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

use roku_common_types::ToolOutputEnvelope;
use roku_plugin_host::{ToolExecutionResult, ToolRuntimeError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Grounded fact returned from a tool invocation or translated tool runtime error.
///
/// ## Why this exists
/// The runtime must preserve a strict boundary between tool truth and runtime interpretation.
/// `ToolObservation` stores the tool-facing facts that later loop logic may interpret.
///
/// ## Fields
/// - `ok`: Whether the tool invocation succeeded according to the tool contract.
/// - `tool_name`: Name of the tool that produced this observation.
/// - `error_type`: Runtime-normalized error type for failed observations, if any.
/// - `terminal`: Whether the tool contract explicitly says the loop should stop on this result.
/// - `data`: Structured tool payload retained for downstream interpretation and replay.
/// - `message`: User-facing or diagnostic summary supplied by the tool contract.
///
/// ## Invariants
/// - `terminal` is tool-contract truth, not a runtime-generated final answer.
/// - `data` remains structured; it is not replaced by a summarized prompt digest.
///
/// ## Non-Goals
/// - `ToolObservation` does not choose `final_answer`, `ask_user`, or `fail`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolObservation {
	pub ok: bool,
	pub tool_name: String,
	pub error_type: Option<String>,
	pub terminal: bool,
	pub data: Value,
	pub message: String,
}

/// Step-level observation snapshot stored inside `StepRecord`.
///
/// ## Why this exists
/// A loop step may record a tool observation or a synthetic terminal message. `StepObservation`
/// keeps those cases explicit without collapsing them into one lossy string.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StepObservation {
	Tool(ToolObservation),
	AskUser { final_message: String },
	FinalMessage { final_message: String },
}

impl ToolObservation {
	pub fn from_output_value(tool_name: &str, output: &Value) -> Self {
		if let Ok(envelope) = serde_json::from_value::<ToolOutputEnvelope>(output.clone()) {
			return Self {
				ok: envelope.ok,
				tool_name: tool_name.to_string(),
				error_type: envelope.error_type,
				terminal: envelope.terminal,
				data: envelope.data,
				message: envelope.message,
			};
		}
		if let Some(observation) = Self::from_migration_legacy_output(tool_name, output) {
			return observation;
		}

		let message = output
			.get("message")
			.and_then(Value::as_str)
			.unwrap_or("tool invocation completed")
			.to_string();
		Self {
			ok: true,
			tool_name: tool_name.to_string(),
			error_type: None,
			terminal: false,
			data: output.clone(),
			message,
		}
	}

	/// Temporary migration-only compatibility for tools that have not yet adopted
	/// `ToolOutputEnvelope`. Remove this fallback after the remaining builtin and skill-backed
	/// tools have moved onto the shared contract.
	fn from_migration_legacy_output(tool_name: &str, output: &Value) -> Option<Self> {
		let ok = output.get("ok").and_then(Value::as_bool)?;
		let error_type = output
			.get("error_type")
			.and_then(Value::as_str)
			.map(str::to_string);
		let terminal = output
			.get("terminal")
			.and_then(Value::as_bool)
			.unwrap_or(false);
		let data = output
			.get("data")
			.cloned()
			.unwrap_or_else(|| output.clone());
		let message = output
			.get("message")
			.and_then(Value::as_str)
			.unwrap_or("tool invocation completed")
			.to_string();
		Some(Self {
			ok,
			tool_name: tool_name.to_string(),
			error_type,
			terminal,
			data,
			message,
		})
	}

	pub fn from_result_payload(tool_name: &str, payload: &Value) -> Self {
		payload
			.get("output")
			.map(|output| Self::from_output_value(tool_name, output))
			.unwrap_or_else(|| Self::from_output_value(tool_name, payload))
	}

	pub fn from_error_payload(tool_name: &str, payload: &Value) -> Self {
		let message = payload
			.get("message")
			.and_then(Value::as_str)
			.unwrap_or("tool invocation failed")
			.to_string();
		let error_code = payload
			.get("error_code")
			.and_then(Value::as_str)
			.unwrap_or("execution_failed");
		let (error_type, terminal) = classify_tool_error(tool_name, error_code, &message);
		Self {
			ok: false,
			tool_name: tool_name.to_string(),
			error_type: Some(error_type),
			terminal,
			data: json!({
				"tool_name": tool_name,
				"error_code": error_code,
				"message": message,
			}),
			message,
		}
	}

	pub fn from_execution_result(execution: &ToolExecutionResult) -> Self {
		Self::from_output_value(&execution.tool_name, &execution.output)
	}

	pub fn from_runtime_error(tool_name: &str, error: &ToolRuntimeError) -> Self {
		let message = error.to_string();
		let error_code = tool_error_code(error);
		let (error_type, terminal) = classify_tool_error(tool_name, error_code, &message);
		Self {
			ok: false,
			tool_name: tool_name.to_string(),
			error_type: Some(error_type),
			terminal,
			data: json!({
				"tool_name": tool_name,
				"error_code": error_code,
				"error": message,
			}),
			message,
		}
	}
}

fn tool_error_code(error: &ToolRuntimeError) -> &'static str {
	match error {
		ToolRuntimeError::ToolNotFound(_) => "tool_not_found",
		ToolRuntimeError::ToolAlreadyRegistered(_) => "tool_already_registered",
		ToolRuntimeError::InvalidDescriptor(_) => "invalid_descriptor",
		ToolRuntimeError::InputSchemaViolation { .. } => "input_schema_violation",
		ToolRuntimeError::CapabilityDenied { .. } => "capability_denied",
		ToolRuntimeError::Timeout { .. } => "timeout",
		ToolRuntimeError::ExecutionFailed { retriable, .. } => {
			if *retriable {
				"retriable_execution_failed"
			} else {
				"execution_failed"
			}
		}
	}
}

fn classify_tool_error(tool_name: &str, error_code: &str, message: &str) -> (String, bool) {
	if tool_name.starts_with("fs.") {
		let lower = message.to_ascii_lowercase();
		if lower.contains("outside the allowed read roots") {
			return ("workspace_violation".to_string(), true);
		}
		if lower.contains("ambiguous under the allowed read roots") {
			return ("multiple_candidates".to_string(), false);
		}
		if lower.contains("is not a directory") {
			return ("not_directory".to_string(), false);
		}
		if lower.contains("is a directory") {
			return ("not_file".to_string(), false);
		}
		if lower.contains("not found") || lower.contains("failed to resolve") {
			return ("path_not_found".to_string(), false);
		}
		if lower.contains("permission denied") {
			return ("permission_denied".to_string(), true);
		}
	}
	match error_code {
		"timeout" => ("tool_timeout".to_string(), true),
		"capability_denied" => ("permission_denied".to_string(), true),
		"input_schema_violation" => ("invalid_argument".to_string(), true),
		"tool_not_found" => ("tool_not_found".to_string(), true),
		other => (other.to_string(), false),
	}
}
