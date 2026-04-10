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

use roku_common_types::GeneralExecuteCompletion;
use serde::{Deserialize, Serialize};

use crate::runtime_loop::{ToolObservation, grounding::reply_selects_candidate};

/// Explicit resume contract for a paused `ask_user` loop.
///
/// ## Why this exists
/// The runtime must pause and resume loops through an explicit contract instead of inferring
/// intent-family-specific rules. This enum captures only the minimum reply shape required to
/// continue the current loop.
///
/// ## Variants
/// - `NoAutomaticResume`: The next user reply should be treated as a fresh intake unless a
///   runtime-owned resume gate explicitly chooses to continue the paused loop.
/// - `CandidateSelection`: The reply must select one of the presented grounded candidates.
/// - `MissingRequiredInput`: The loop is paused until the user supplies one or more missing
///   required input fields for the current request.
///
/// ## Invariants
/// - This contract only describes resume eligibility for the current pause.
/// - It does not choose the next tool or encode a future plan.
///
/// ## Non-Goals
/// - This enum is not a planner.
/// - This enum does not replace tool grounding or route classification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AskUserResumeContract {
	NoAutomaticResume,
	CandidateSelection { candidates: Vec<String> },
	MissingRequiredInput { fields: Vec<String> },
}

/// Optional deterministic resume directive attached to a paused `ask_user` contract.
///
/// ## Why this exists
/// Some `ask_user` pauses gather one missing grounded value for an already selected tool. When
/// that is true, the runtime may safely resume by replaying the same tool with the clarified
/// argument instead of inventing a new semantic route.
///
/// ## Invariants
/// - This directive may only resume the already selected tool.
/// - It cannot switch tool family or choose a brand-new follow-up action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AskUserResumeDirective {
	RepeatToolWithSelectedCandidate {
		tool_name: String,
		argument_key: String,
	},
}

/// User-facing payload emitted when the loop must pause for clarification.
///
/// ## Why this exists
/// The runtime needs a small, explicit contract for pausing execution and asking the user for
/// missing information while preserving the existing `LoopState` for resume.
///
/// ## Fields
/// - `final_message`: The exact clarification message that should be surfaced to the user.
/// - `resume_contract`: Explicit contract that decides whether a future user reply may resume
///   this paused loop.
/// - `resume_directive`: Optional deterministic replay instruction for the already selected tool
///   once the required clarification arrives.
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
	pub resume_contract: AskUserResumeContract,
	pub resume_directive: Option<AskUserResumeDirective>,
}

impl AskUserPayload {
	pub fn freeform(final_message: impl Into<String>) -> Self {
		Self {
			final_message: final_message.into(),
			resume_contract: AskUserResumeContract::NoAutomaticResume,
			resume_directive: None,
		}
	}

	pub fn missing_required_input(final_message: impl Into<String>, fields: Vec<String>) -> Self {
		Self {
			final_message: final_message.into(),
			resume_contract: AskUserResumeContract::MissingRequiredInput { fields },
			resume_directive: None,
		}
	}

	pub(crate) fn candidate_selection(
		final_message: impl Into<String>,
		candidates: Vec<String>,
		resume_directive: Option<AskUserResumeDirective>,
	) -> Self {
		Self {
			final_message: final_message.into(),
			resume_contract: AskUserResumeContract::CandidateSelection { candidates },
			resume_directive,
		}
	}

	pub fn can_resume(&self, user_input: &str) -> bool {
		let trimmed = user_input.trim();
		if trimmed.is_empty() {
			return false;
		}
		match &self.resume_contract {
			AskUserResumeContract::NoAutomaticResume => false,
			AskUserResumeContract::CandidateSelection { candidates } => {
				reply_selects_candidate(trimmed, candidates).is_some()
			}
			AskUserResumeContract::MissingRequiredInput { .. } => false,
		}
	}
}

pub(crate) fn effective_ask_user_payload(
	goal: &str,
	last_observation: Option<&ToolObservation>,
	proposed_payload: Option<AskUserPayload>,
) -> AskUserPayload {
	if let Some(observation) = last_observation {
		return ask_user_from_observation(goal, observation);
	}
	proposed_payload.unwrap_or_else(|| {
		AskUserPayload::freeform("I need more information before I can continue.")
	})
}

pub(crate) fn ask_user_from_observation(
	goal: &str,
	observation: &ToolObservation,
) -> AskUserPayload {
	let matches = observation
		.data
		.get("matches")
		.and_then(serde_json::Value::as_array)
		.map(|values| {
			values
				.iter()
				.filter_map(serde_json::Value::as_str)
				.map(str::to_string)
				.collect::<Vec<_>>()
		})
		.unwrap_or_default();

	match observation.error_type.as_deref() {
		Some("needs_more_information") => {
			let missing_information = missing_information_fields(observation);
			if missing_information.is_empty() {
				AskUserPayload::freeform(observation.message.clone())
			} else {
				AskUserPayload::missing_required_input(
					observation.message.clone(),
					missing_information,
				)
			}
		}
		Some("multiple_candidates") => {
			let final_message = if !goal.is_ascii() {
				if matches.is_empty() {
					"我找到了多个候选路径。请告诉我你想看哪一个更具体的路径。".to_string()
				} else {
					format!(
						"我找到了多个候选路径：{}。你想看哪一个？",
						matches.join("、")
					)
				}
			} else if matches.is_empty() {
				"I found multiple candidate paths. Please tell me which one you want.".to_string()
			} else {
				format!(
					"I found multiple candidate paths: {}. Which one do you want?",
					matches.join(", ")
				)
			};
			AskUserPayload::candidate_selection(
				final_message,
				matches,
				resume_directive_for_observation(observation),
			)
		}
		_ => AskUserPayload::freeform(observation.message.clone()),
	}
}

fn missing_information_fields(observation: &ToolObservation) -> Vec<String> {
	observation
		.data
		.get("completion")
		.cloned()
		.and_then(|value| serde_json::from_value::<GeneralExecuteCompletion>(value).ok())
		.map(|completion| completion.missing_information)
		.unwrap_or_default()
}

fn resume_directive_for_observation(
	observation: &ToolObservation,
) -> Option<AskUserResumeDirective> {
	let argument_key = match observation.tool_name.as_str() {
		"fs.exists" | "fs.inspect" | "fs.list_dir" | "fs.read_text" => "path",
		"table.inspect" | "table.list_sheets" | "table.preview" | "table.schema" => "path",
		"fs.glob" => "pattern",
		_ => return None,
	};
	Some(AskUserResumeDirective::RepeatToolWithSelectedCandidate {
		tool_name: observation.tool_name.clone(),
		argument_key: argument_key.to_string(),
	})
}

#[cfg(test)]
mod tests {
	use serde_json::json;

	use super::{
		AskUserPayload, AskUserResumeContract, ask_user_from_observation,
		effective_ask_user_payload,
	};
	use crate::runtime_loop::ToolObservation;

	#[test]
	fn effective_ask_user_payload_prefers_structured_observation_over_model_text() {
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
		let actual = effective_ask_user_payload(
			"帮我看下 lib.rs 里有啥",
			Some(&observation),
			Some(AskUserPayload::freeform("请看 agent-runtime 那个")),
		);

		assert_eq!(actual.final_message, expected);
	}

	#[test]
	fn candidate_selection_requires_an_explicit_match() {
		let payload = AskUserPayload {
			final_message: "pick one".to_string(),
			resume_contract: AskUserResumeContract::CandidateSelection {
				candidates: vec![
					"/workspace/Cargo.toml".to_string(),
					"/workspace/crates/core/Cargo.toml".to_string(),
				],
			},
			resume_directive: None,
		};

		assert!(payload.can_resume("/workspace/Cargo.toml"));
		assert!(!payload.can_resume("随便那个"));
	}

	#[test]
	fn freeform_payload_does_not_auto_resume() {
		let payload = AskUserPayload::freeform("请补充更多上下文");
		assert!(!payload.can_resume("项目里有几行代码？"));
	}

	#[test]
	fn needs_more_information_observation_creates_missing_input_contract() {
		let observation = ToolObservation {
			ok: false,
			tool_name: "inventory.describe".to_string(),
			error_type: Some("needs_more_information".to_string()),
			terminal: false,
			data: json!({
				"completion": {
					"final_message": "请提供 project_path。",
					"completion_kind": "needs_more_information",
					"evidence_status": "missing_required_input",
					"missing_information": ["project_path"]
				}
			}),
			message: "请提供 project_path。".to_string(),
		};

		let payload = ask_user_from_observation("统计代码行数", &observation);
		assert_eq!(
			payload.resume_contract,
			AskUserResumeContract::MissingRequiredInput {
				fields: vec!["project_path".to_string()]
			}
		);
	}
}
