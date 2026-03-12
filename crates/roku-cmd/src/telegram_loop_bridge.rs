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

use roku_agent_runtime::LoopState;
use roku_common_types::{PendingLoopBinding, RuntimeError};
use roku_runtime_service::RuntimeService;

use crate::bot::TelegramSessionState;

pub(crate) fn restore_pending_loop_from_session(
	service: &RuntimeService,
	session_state: &TelegramSessionState,
	session_id: &str,
) -> Result<(), RuntimeError> {
	let mut preferences = session_state.load_preferences_or_default(session_id)?;
	let Some(binding) = preferences.pending_loop.clone() else {
		return Ok(());
	};
	let loop_state = match serde_json::from_str::<LoopState>(&binding.loop_state_json) {
		Ok(loop_state) => loop_state,
		Err(_) => {
			preferences.pending_loop = None;
			session_state.save_preferences(session_id, preferences)?;
			return Ok(());
		}
	};
	service.restore_pending_loop(loop_state)
}

pub(crate) fn sync_pending_loop_to_session(
	service: &RuntimeService,
	session_state: &TelegramSessionState,
	session_id: &str,
) -> Result<(), RuntimeError> {
	let mut preferences = session_state.load_preferences_or_default(session_id)?;
	preferences.pending_loop = match service.pending_loop(session_id)? {
		Some(loop_state) => Some(PendingLoopBinding {
			run_id: loop_state.run_id.clone(),
			loop_state_json: serde_json::to_string(&loop_state).map_err(|error| {
				RuntimeError::new(format!("failed to encode pending runtime loop: {error}"))
			})?,
		}),
		None => None,
	};
	session_state.save_preferences(session_id, preferences)
}
