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

use crate::router::IntentFamily;
use crate::runtime_loop::grounding::{
	extract_explicit_python_code, extract_path_candidates, extract_table_path, extract_web_query,
	reply_selects_candidate,
};
use crate::runtime_loop::{LoopState, LoopStatus};

pub fn should_resume_awaiting_user(loop_state: &LoopState, user_input: &str) -> bool {
	if loop_state.status != LoopStatus::AwaitingUser {
		return false;
	}
	if user_input.trim().is_empty() {
		return false;
	}
	match loop_state.route_decision.intent_family {
		IntentFamily::FilesystemRead => filesystem_reply_can_resume(loop_state, user_input),
		IntentFamily::TableRead => extract_table_path(user_input).is_some(),
		IntentFamily::CodeExec => extract_explicit_python_code(user_input).is_some(),
		IntentFamily::WebLookup => extract_web_query(user_input).is_some(),
		IntentFamily::Chat
		| IntentFamily::TextTransform
		| IntentFamily::MultiStep
		| IntentFamily::Unknown => true,
	}
}

fn filesystem_reply_can_resume(loop_state: &LoopState, user_input: &str) -> bool {
	let explicit_paths = extract_path_candidates(user_input);
	if !explicit_paths.is_empty() {
		return true;
	}
	let Some(observation) = loop_state.last_observation.as_ref() else {
		return false;
	};
	if observation.error_type.as_deref() != Some("multiple_candidates") {
		return false;
	}
	let candidates = observation
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
	reply_selects_candidate(user_input, &candidates).is_some()
}
