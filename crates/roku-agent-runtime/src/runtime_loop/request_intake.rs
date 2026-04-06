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

use roku_common_types::{PlanningModeHint, RequestEnvelope, RequestId};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoopRequest {
	pub request_id: RequestId,
	pub session_id: String,
	pub goal: String,
	/// Kept for serde backwards compatibility with serialized loop snapshots.
	/// No longer populated; always deserialized as None from new snapshots.
	#[serde(default)]
	pub planning_mode_hint: Option<PlanningModeHint>,
}

pub(crate) fn intake_request(request: &RequestEnvelope) -> LoopRequest {
	LoopRequest {
		request_id: request.request_id.clone(),
		session_id: request.session_id.clone(),
		goal: request.goal.clone(),
		planning_mode_hint: None,
	}
}
