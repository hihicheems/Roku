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

use roku_plugin_llm::ToolCallBlock;
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
	/// Construct from native tool_use response blocks.
	///
	/// Returns `None` if `tool_calls` is empty.
	pub fn from_tool_calls(tool_calls: &[ToolCallBlock]) -> Option<Self> {
		if tool_calls.is_empty() {
			return None;
		}

		let decision = if tool_calls.len() == 1 {
			let call = &tool_calls[0];
			let arguments = &call.arguments;

			match call.name.as_str() {
				"final_answer" => {
					let message = arguments
						.get("message")
						.and_then(Value::as_str)
						.unwrap_or("")
						.to_string();
					Self {
						action: NextStepAction::FinalAnswer,
						tool_name: None,
						arguments: None,
						tool_calls: None,
						reason: "native tool_use: final_answer".to_string(),
						final_message: Some(message),
					}
				}
				"ask_user" => {
					let message = arguments
						.get("question")
						.or_else(|| arguments.get("message"))
						.and_then(Value::as_str)
						.unwrap_or("")
						.to_string();
					Self {
						action: NextStepAction::AskUser,
						tool_name: None,
						arguments: None,
						tool_calls: None,
						reason: "native tool_use: ask_user".to_string(),
						final_message: Some(message),
					}
				}
				"fail" => {
					let reason = arguments
						.get("reason")
						.and_then(Value::as_str)
						.unwrap_or("")
						.to_string();
					Self {
						action: NextStepAction::Fail,
						tool_name: None,
						arguments: None,
						tool_calls: None,
						reason: "native tool_use: fail".to_string(),
						final_message: Some(reason),
					}
				}
				name => {
					let reason = arguments
						.get("reason")
						.and_then(Value::as_str)
						.unwrap_or("native tool_use call")
						.to_string();
					Self {
						action: NextStepAction::CallTool,
						tool_name: Some(name.to_string()),
						arguments: Some(arguments.clone()),
						tool_calls: None,
						reason,
						final_message: None,
					}
				}
			}
		} else {
			let entries = tool_calls
				.iter()
				.map(|call| ToolCallEntry {
					tool_name: call.name.clone(),
					arguments: Some(call.arguments.clone()),
				})
				.collect::<Vec<_>>();
			Self {
				action: NextStepAction::CallTools,
				tool_name: None,
				arguments: None,
				tool_calls: Some(entries),
				reason: "native tool_use: multiple parallel calls".to_string(),
				final_message: None,
			}
		};

		decision.validate().ok()?;
		Some(decision)
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
	#[error("invalid next-step shape for action `{action:?}`: {message}")]
	InvalidActionShape {
		action: NextStepAction,
		message: String,
	},
}

#[cfg(test)]
mod tests {
	use serde_json::json;

	use super::{NextStepAction, NextStepDecision};
	use roku_plugin_llm::ToolCallBlock;

	#[test]
	fn from_tool_calls_returns_none_for_empty_slice() {
		assert!(NextStepDecision::from_tool_calls(&[]).is_none());
	}

	#[test]
	fn from_tool_calls_final_answer() {
		let calls = vec![ToolCallBlock {
			id: "call_1".to_string(),
			name: "final_answer".to_string(),
			arguments: json!({ "message": "Task complete." }),
		}];
		let decision = NextStepDecision::from_tool_calls(&calls).expect("should produce decision");
		assert_eq!(decision.action, NextStepAction::FinalAnswer);
		assert_eq!(decision.final_message.as_deref(), Some("Task complete."));
		assert!(decision.tool_name.is_none());
	}

	#[test]
	fn from_tool_calls_ask_user_with_question_field() {
		let calls = vec![ToolCallBlock {
			id: "call_2".to_string(),
			name: "ask_user".to_string(),
			arguments: json!({ "question": "Which file?" }),
		}];
		let decision = NextStepDecision::from_tool_calls(&calls).expect("should produce decision");
		assert_eq!(decision.action, NextStepAction::AskUser);
		assert_eq!(decision.final_message.as_deref(), Some("Which file?"));
	}

	#[test]
	fn from_tool_calls_ask_user_with_message_field() {
		let calls = vec![ToolCallBlock {
			id: "call_3".to_string(),
			name: "ask_user".to_string(),
			arguments: json!({ "message": "Which file?" }),
		}];
		let decision = NextStepDecision::from_tool_calls(&calls).expect("should produce decision");
		assert_eq!(decision.action, NextStepAction::AskUser);
		assert_eq!(decision.final_message.as_deref(), Some("Which file?"));
	}

	#[test]
	fn from_tool_calls_fail() {
		let calls = vec![ToolCallBlock {
			id: "call_4".to_string(),
			name: "fail".to_string(),
			arguments: json!({ "reason": "File not found" }),
		}];
		let decision = NextStepDecision::from_tool_calls(&calls).expect("should produce decision");
		assert_eq!(decision.action, NextStepAction::Fail);
		assert_eq!(decision.final_message.as_deref(), Some("File not found"));
	}

	#[test]
	fn from_tool_calls_regular_tool() {
		let calls = vec![ToolCallBlock {
			id: "call_5".to_string(),
			name: "Read".to_string(),
			arguments: json!({ "path": "Cargo.toml" }),
		}];
		let decision = NextStepDecision::from_tool_calls(&calls).expect("should produce decision");
		assert_eq!(decision.action, NextStepAction::CallTool);
		assert_eq!(decision.tool_name.as_deref(), Some("Read"));
		assert_eq!(decision.arguments, Some(json!({ "path": "Cargo.toml" })));
	}

	#[test]
	fn from_tool_calls_regular_tool_uses_reason_from_arguments() {
		let calls = vec![ToolCallBlock {
			id: "call_6".to_string(),
			name: "Read".to_string(),
			arguments: json!({ "path": "Cargo.toml", "reason": "need manifest" }),
		}];
		let decision = NextStepDecision::from_tool_calls(&calls).expect("should produce decision");
		assert_eq!(decision.reason, "need manifest");
	}

	#[test]
	fn from_tool_calls_regular_tool_default_reason() {
		let calls = vec![ToolCallBlock {
			id: "call_7".to_string(),
			name: "Write".to_string(),
			arguments: json!({ "path": "out.txt", "content": "hello" }),
		}];
		let decision = NextStepDecision::from_tool_calls(&calls).expect("should produce decision");
		assert_eq!(decision.reason, "native tool_use call");
	}

	#[test]
	fn from_tool_calls_multiple_calls_produces_call_tools() {
		let calls = vec![
			ToolCallBlock {
				id: "call_8a".to_string(),
				name: "Read".to_string(),
				arguments: json!({ "path": "a.txt" }),
			},
			ToolCallBlock {
				id: "call_8b".to_string(),
				name: "Read".to_string(),
				arguments: json!({ "path": "b.txt" }),
			},
		];
		let decision = NextStepDecision::from_tool_calls(&calls).expect("should produce decision");
		assert_eq!(decision.action, NextStepAction::CallTools);
		let entries = decision.tool_calls.expect("tool_calls should be set");
		assert_eq!(entries.len(), 2);
		assert_eq!(entries[0].tool_name, "Read");
		assert_eq!(entries[1].tool_name, "Read");
		assert_eq!(entries[0].arguments, Some(json!({ "path": "a.txt" })));
		assert_eq!(entries[1].arguments, Some(json!({ "path": "b.txt" })));
	}
}
