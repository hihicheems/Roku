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

//! Bridge between runtime pending-loop state and Telegram session persistence.
//!
//! Telegram stores pause/resume bindings in chat-scoped session state, while the runtime
//! service owns the in-memory loop state used to continue execution. This module is the explicit
//! adapter between those two representations.

use roku_agent_runtime::LoopState;
use roku_common_types::{PendingLoopBinding, RuntimeError};
use roku_runtime_service::RuntimeService;

pub(crate) trait PendingLoopStore {
	fn load_pending_loop_binding(
		&self,
		session_id: &str,
	) -> Result<Option<PendingLoopBinding>, RuntimeError>;

	fn save_pending_loop_binding(
		&self,
		session_id: &str,
		binding: Option<PendingLoopBinding>,
	) -> Result<(), RuntimeError>;

	fn clear_pending_loop_binding(&self, session_id: &str) -> Result<(), RuntimeError> {
		self.save_pending_loop_binding(session_id, None)
	}
}

/// Restores a persisted Telegram pending-loop binding into the live runtime service.
///
/// Corrupt serialized loop state is treated as stale session residue: the broken binding is
/// dropped instead of failing the whole request path.
pub(crate) fn restore_pending_loop_from_session(
	service: &RuntimeService,
	pending_loop_store: &dyn PendingLoopStore,
	session_id: &str,
) -> Result<(), RuntimeError> {
	let Some(binding) = pending_loop_store.load_pending_loop_binding(session_id)? else {
		return Ok(());
	};
	let loop_state = match serde_json::from_str::<LoopState>(&binding.loop_state_json) {
		Ok(loop_state) => loop_state,
		Err(_) => {
			pending_loop_store.clear_pending_loop_binding(session_id)?;
			return Ok(());
		}
	};
	service.restore_pending_loop(loop_state)
}

/// Persists the runtime service's current pending-loop state back into Telegram session storage.
///
/// This keeps Telegram control commands and resume handling keyed off the same loop snapshot the
/// runtime most recently exposed.
pub(crate) fn sync_pending_loop_to_session(
	service: &RuntimeService,
	pending_loop_store: &dyn PendingLoopStore,
	session_id: &str,
) -> Result<(), RuntimeError> {
	let binding = match service.pending_loop(session_id)? {
		Some(loop_state) => Some(PendingLoopBinding {
			run_id: loop_state.run_id.clone(),
			loop_state_json: serde_json::to_string(&loop_state).map_err(|error| {
				RuntimeError::new(format!("failed to encode pending runtime loop: {error}"))
			})?,
		}),
		None => None,
	};
	pending_loop_store.save_pending_loop_binding(session_id, binding)
}
