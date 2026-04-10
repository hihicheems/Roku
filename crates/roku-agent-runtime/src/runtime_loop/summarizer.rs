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

/// User-visible final-answer payload derived from grounded runtime observation.
///
/// ## Why this exists
/// The ReAct runtime ends each successful loop through a normalized completion contract. This
/// payload carries only the user-facing answer text produced from the latest grounded
/// observation.
///
/// ## Fields
/// - `final_message`: User-visible message derived from the latest grounded observation.
///
/// ## Invariants
/// - This payload only carries user-facing completion text.
/// - This payload is derived from grounded observation data, not from hidden planner state.
///
/// ## Non-Goals
/// - This payload does not carry history, audit, or replay metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinalAnswerPayload {
	pub final_message: String,
}
