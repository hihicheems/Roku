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

use crate::runtime_loop::{LoopState, LoopStatus};

pub fn should_resume_awaiting_user(loop_state: &LoopState, user_input: &str) -> bool {
	if loop_state.status != LoopStatus::AwaitingUser {
		return false;
	}
	if user_input.trim().is_empty() {
		return false;
	}
	loop_state
		.awaiting_user
		.as_ref()
		.is_some_and(|payload| payload.can_resume(user_input))
}
