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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskUserPayload {
	pub final_message: String,
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
