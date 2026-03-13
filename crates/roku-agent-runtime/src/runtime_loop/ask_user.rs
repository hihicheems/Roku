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

use crate::runtime_loop::ToolObservation;

/// User-facing payload emitted when the loop must pause for clarification.
///
/// ## Why this exists
/// The runtime needs a small, explicit contract for pausing execution and asking the user for
/// missing information while preserving the existing `LoopState` for resume.
///
/// ## Fields
/// - `final_message`: The exact clarification message that should be surfaced to the user.
///
/// ## Invariants
/// - This payload does not mutate route classification.
/// - Resume must continue the existing loop state instead of starting a fresh route decision.
///
/// ## Non-Goals
/// - This payload does not encode a future plan.
/// - This payload is not a replay log entry by itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskUserPayload {
	pub final_message: String,
}

pub(crate) fn effective_ask_user_message(
	goal: &str,
	last_observation: Option<&ToolObservation>,
	proposed_message: Option<String>,
) -> String {
	if let Some(observation) = last_observation {
		return ask_user_from_observation(goal, observation).final_message;
	}
	proposed_message.unwrap_or_else(|| "I need more information before I can continue.".to_string())
}

pub(crate) fn ask_user_from_observation(
	goal: &str,
	observation: &ToolObservation,
) -> AskUserPayload {
	let final_message = if !goal.is_ascii() {
		match observation.error_type.as_deref() {
			Some("multiple_candidates") => {
				let matches = observation
					.data
					.get("matches")
					.and_then(serde_json::Value::as_array)
					.map(|values| {
						values
							.iter()
							.filter_map(serde_json::Value::as_str)
							.collect::<Vec<_>>()
					})
					.unwrap_or_default();
				if matches.is_empty() {
					"我找到了多个候选路径。请告诉我你想看哪一个更具体的路径。".to_string()
				} else {
					format!(
						"我找到了多个候选路径：{}。你想看哪一个？",
						matches.join("、")
					)
				}
			}
			_ => observation.message.clone(),
		}
	} else {
		match observation.error_type.as_deref() {
			Some("multiple_candidates") => {
				let matches = observation
					.data
					.get("matches")
					.and_then(serde_json::Value::as_array)
					.map(|values| {
						values
							.iter()
							.filter_map(serde_json::Value::as_str)
							.collect::<Vec<_>>()
					})
					.unwrap_or_default();
				if matches.is_empty() {
					"I found multiple candidate paths. Please tell me which one you want."
						.to_string()
				} else {
					format!(
						"I found multiple candidate paths: {}. Which one do you want?",
						matches.join(", ")
					)
				}
			}
			_ => observation.message.clone(),
		}
	};
	AskUserPayload { final_message }
}

#[cfg(test)]
mod tests {
	use serde_json::json;

	use super::{ask_user_from_observation, effective_ask_user_message};
	use crate::runtime_loop::ToolObservation;

	#[test]
	fn effective_ask_user_message_prefers_structured_observation_over_model_text() {
		let observation = ToolObservation {
			ok: false,
			tool_name: "fs.find".to_string(),
			error_type: Some("multiple_candidates".to_string()),
			terminal: false,
			data: json!({
				"matches": [
					"/Users/jojo/cjj_project/Roku/crates/roku-agent-runtime/src/lib.rs",
					"/Users/jojo/cjj_project/Roku/crates/roku-runtime-service/src/lib.rs"
				]
			}),
			message: "llm wrote something else".to_string(),
		};

		let expected =
			ask_user_from_observation("帮我看下 lib.rs 里有啥", &observation).final_message;
		let actual = effective_ask_user_message(
			"帮我看下 lib.rs 里有啥",
			Some(&observation),
			Some("请看 agent-runtime 那个".to_string()),
		);

		assert_eq!(actual, expected);
	}
}
