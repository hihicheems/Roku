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

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NextStepAction {
	CallTool,
	CallTools,
	AskUser,
	FinalAnswer,
	Fail,
}

/// A single tool invocation entry within a batch `call_tools` decision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallEntry {
	pub tool_name: String,
	#[serde(default)]
	pub arguments: Option<Value>,
}

/// One-round decision emitted by the generic ReAct loop.
///
/// ## Why this exists
/// The runtime needs a small, explicit contract for "what should happen next" without embedding a
/// hidden planner or long-horizon state machine into the decision itself.
///
/// ## Fields
/// - `action`: The current round action to execute.
/// - `tool_name`: Selected tool for `call_tool`, otherwise `None`.
/// - `arguments`: Structured arguments for the selected tool when applicable.
/// - `reason`: Short explanation for why this action was chosen.
/// - `final_message`: Optional terminal message for `ask_user`, `final_answer`, or `fail`.
///
/// ## Invariants
/// - `NextStepDecision` only describes the current round.
/// - `call_tool` is the only action allowed to set `tool_name`.
/// - `final_message` is terminal metadata, not a multi-step plan.
///
/// ## Non-Goals
/// - This struct does not encode a plan for later rounds.
/// - This struct does not replace `LoopState`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NextStepDecision {
	pub action: NextStepAction,
	pub tool_name: Option<String>,
	pub arguments: Option<Value>,
	/// Batch tool calls for the `CallTools` action.
	#[serde(default)]
	pub tool_calls: Option<Vec<ToolCallEntry>>,
	pub reason: String,
	pub final_message: Option<String>,
}

impl NextStepDecision {
	pub fn from_json_value(value: &Value) -> Result<Self, NextStepDecisionSchemaError> {
		let object = value
			.as_object()
			.ok_or(NextStepDecisionSchemaError::RootNotObject)?;

		let action_value = object
			.get("action")
			.ok_or(NextStepDecisionSchemaError::MissingRequiredKey { key: "action" })?;
		let action_raw = action_value.as_str().unwrap_or_default();
		let action = parse_action(action_value)?;
		let reason = object
			.get("reason")
			.and_then(Value::as_str)
			.ok_or(NextStepDecisionSchemaError::MissingRequiredKey { key: "reason" })?
			.to_string();
		// If the model placed a tool name in the action field (e.g. "web.fetch"),
		// use it as tool_name when tool_name is not explicitly set.
		let explicit_tool_name = object
			.get("tool_name")
			.and_then(Value::as_str)
			.map(str::to_string);
		let tool_name = explicit_tool_name.or_else(|| {
			if action == NextStepAction::CallTool && action_raw.contains('.') {
				Some(action_raw.to_string())
			} else {
				None
			}
		});
		let arguments = object
			.get("arguments")
			.cloned()
			.filter(|value| !value.is_null());
		let tool_calls = object
			.get("tool_calls")
			.and_then(Value::as_array)
			.map(|arr| {
				arr.iter()
					.filter_map(|v| {
						let name = v.get("tool_name").and_then(Value::as_str)?;
						let args = v.get("arguments").cloned().filter(|a| !a.is_null());
						Some(ToolCallEntry {
							tool_name: name.to_string(),
							arguments: args,
						})
					})
					.collect::<Vec<_>>()
			})
			.filter(|v| !v.is_empty());
		let final_message = object
			.get("final_message")
			.and_then(Value::as_str)
			.map(str::to_string);

		let decision = Self {
			action,
			tool_name,
			arguments,
			tool_calls,
			reason,
			final_message,
		};
		decision.validate()?;
		Ok(decision)
	}

	pub fn validate(&self) -> Result<(), NextStepDecisionSchemaError> {
		match self.action {
			NextStepAction::CallTool => {
				if self.tool_name.is_none() {
					return Err(NextStepDecisionSchemaError::InvalidActionShape {
						action: self.action,
						message: "call_tool requires tool_name".to_string(),
					});
				}
			}
			NextStepAction::CallTools => {
				if self.tool_calls.is_none() {
					return Err(NextStepDecisionSchemaError::InvalidActionShape {
						action: self.action,
						message: "call_tools requires tool_calls array".to_string(),
					});
				}
			}
			NextStepAction::AskUser | NextStepAction::FinalAnswer | NextStepAction::Fail => {
				if self.tool_name.is_some() {
					return Err(NextStepDecisionSchemaError::InvalidActionShape {
						action: self.action,
						message: "non-call_tool actions must not set tool_name".to_string(),
					});
				}
			}
		}
		Ok(())
	}
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum NextStepDecisionSchemaError {
	#[error("next-step decision root must be a json object")]
	RootNotObject,
	#[error("next-step decision is missing required key `{key}`")]
	MissingRequiredKey { key: &'static str },
	#[error("field `{field}` has invalid type; expected {expected}")]
	InvalidType {
		field: &'static str,
		expected: &'static str,
	},
	#[error("unknown action value `{value}`")]
	UnknownAction { value: String },
	#[error("invalid next-step shape for action `{action:?}`: {message}")]
	InvalidActionShape {
		action: NextStepAction,
		message: String,
	},
}

fn parse_action(value: &Value) -> Result<NextStepAction, NextStepDecisionSchemaError> {
	let raw = value
		.as_str()
		.ok_or(NextStepDecisionSchemaError::InvalidType {
			field: "action",
			expected: "string",
		})?;
	match raw {
		"call_tool" => Ok(NextStepAction::CallTool),
		"call_tools" => Ok(NextStepAction::CallTools),
		"ask_user" => Ok(NextStepAction::AskUser),
		"final_answer" => Ok(NextStepAction::FinalAnswer),
		"fail" => Ok(NextStepAction::Fail),
		// Models sometimes put the tool name directly in the action field
		// (e.g. "web.fetch" instead of "call_tool"). Treat any value
		// containing a dot as an implicit call_tool — the caller will
		// use the action value as tool_name if tool_name is not set.
		other if other.contains('.') => Ok(NextStepAction::CallTool),
		_ => Err(NextStepDecisionSchemaError::UnknownAction {
			value: raw.to_string(),
		}),
	}
}

#[cfg(test)]
mod tests {
	use serde_json::json;

	use super::{NextStepAction, NextStepDecision, NextStepDecisionSchemaError};

	#[test]
	fn parses_call_tool_decision_with_tool_name() {
		let decision = NextStepDecision::from_json_value(&json!({
			"action": "call_tool",
			"tool_name": "fs.read_text",
			"arguments": { "path": "Cargo.toml" },
			"reason": "Read a file",
			"final_message": null
		}))
		.expect("decision should parse");

		assert_eq!(decision.action, NextStepAction::CallTool);
		assert_eq!(decision.tool_name.as_deref(), Some("fs.read_text"));
	}

	#[test]
	fn rejects_non_call_tool_with_tool_name() {
		let error = NextStepDecision::from_json_value(&json!({
			"action": "final_answer",
			"tool_name": "fs.read_text",
			"arguments": null,
			"reason": "Done",
			"final_message": "Here is the answer"
		}))
		.expect_err("decision should be rejected");

		assert!(matches!(
			error,
			NextStepDecisionSchemaError::InvalidActionShape { .. }
		));
	}
}
