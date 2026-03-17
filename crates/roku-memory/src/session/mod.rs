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

//! Roku-owned session-state namespace.
//!
//! Transport-specific persistence remains adapter work; the session model
//! itself belongs to Roku's memory subsystem.

use std::collections::HashMap;

use roku_common_types::SessionPreferences;
use thiserror::Error;

/// Session-scoped transport/runtime continuity state.
///
/// This currently reuses [`SessionPreferences`] for wire compatibility while
/// the provider-neutral ownership moves into `roku-memory`.
pub type SessionState = SessionPreferences;

/// Error returned by session-state backends.
#[derive(Debug, Error)]
pub enum SessionStateError {
	#[error("session-state backend failed: {0}")]
	Backend(String),
}

/// Provider-neutral session-state contract.
pub trait SessionStateBackend: Send {
	fn save_session_state(
		&mut self,
		session_id: &str,
		state: SessionState,
	) -> Result<(), SessionStateError>;

	fn load_session_state(
		&self,
		session_id: &str,
	) -> Result<Option<SessionState>, SessionStateError>;

	fn delete_session_state(&mut self, session_id: &str) -> Result<(), SessionStateError>;
}

/// Disabled session-state backend used when transport/session persistence is unavailable.
#[derive(Debug, Default)]
pub struct NoopSessionStateBackend;

impl SessionStateBackend for NoopSessionStateBackend {
	fn save_session_state(
		&mut self,
		_session_id: &str,
		_state: SessionState,
	) -> Result<(), SessionStateError> {
		Ok(())
	}

	fn load_session_state(
		&self,
		_session_id: &str,
	) -> Result<Option<SessionState>, SessionStateError> {
		Ok(None)
	}

	fn delete_session_state(&mut self, _session_id: &str) -> Result<(), SessionStateError> {
		Ok(())
	}
}

/// In-memory session-state backend used by core tests and lightweight entry tests.
///
/// This lives in `roku-memory` so test-only session behavior does not need to
/// reach back into transitional persistence crates.
#[derive(Debug, Default)]
pub struct InMemorySessionStateBackend {
	states: HashMap<String, SessionState>,
}

impl SessionStateBackend for InMemorySessionStateBackend {
	fn save_session_state(
		&mut self,
		session_id: &str,
		state: SessionState,
	) -> Result<(), SessionStateError> {
		self.states.insert(session_id.to_string(), state);
		Ok(())
	}

	fn load_session_state(
		&self,
		session_id: &str,
	) -> Result<Option<SessionState>, SessionStateError> {
		Ok(self.states.get(session_id).cloned())
	}

	fn delete_session_state(&mut self, session_id: &str) -> Result<(), SessionStateError> {
		self.states.remove(session_id);
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use roku_common_types::{PendingLoopBinding, PlanningModeHint};

	use super::*;

	#[test]
	fn in_memory_session_state_backend_roundtrips_state() {
		let mut backend = InMemorySessionStateBackend::default();
		let state = SessionState {
			planning_mode: Some(PlanningModeHint::TreeSearch),
			pending_loop: Some(PendingLoopBinding {
				run_id: "loop-1".to_string(),
				loop_state_json: "{\"status\":\"paused\"}".to_string(),
			}),
		};

		backend
			.save_session_state("session-1", state.clone())
			.expect("state should save");
		assert_eq!(
			backend
				.load_session_state("session-1")
				.expect("state should load"),
			Some(state)
		);

		backend
			.delete_session_state("session-1")
			.expect("state should delete");
		assert_eq!(
			backend
				.load_session_state("session-1")
				.expect("deleted state should load"),
			None
		);
	}
}
