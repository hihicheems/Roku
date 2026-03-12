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

use roku_plugin_host::{ToolExecutionResult, ToolRuntimeError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolObservation {
	pub ok: bool,
	pub tool_name: String,
	pub error_type: Option<String>,
	pub terminal: bool,
	pub data: Value,
	pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StepObservation {
	Tool(ToolObservation),
	AskUser { final_message: String },
	FinalMessage { final_message: String },
}

impl ToolObservation {
	pub fn from_execution_result(execution: &ToolExecutionResult) -> Self {
		let message = execution
			.output
			.get("message")
			.and_then(Value::as_str)
			.unwrap_or("tool invocation completed")
			.to_string();

		Self {
			ok: true,
			tool_name: execution.tool_name.clone(),
			error_type: None,
			terminal: false,
			data: execution.output.clone(),
			message,
		}
	}

	pub fn from_runtime_error(tool_name: &str, error: &ToolRuntimeError) -> Self {
		Self {
			ok: false,
			tool_name: tool_name.to_string(),
			error_type: Some(tool_error_code(error).to_string()),
			terminal: false,
			data: json!({
				"tool_name": tool_name,
				"error": error.to_string(),
			}),
			message: error.to_string(),
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
