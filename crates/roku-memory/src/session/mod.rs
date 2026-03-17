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
//! This module is reserved for provider-neutral session-state contracts and
//! types. Transport-specific implementations remain adapters and do not own the
//! session model.

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
