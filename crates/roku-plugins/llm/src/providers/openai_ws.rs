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

//! Session-level state management for OpenAI Responses API delta mode.
//!
//! This module tracks `previous_response_id` across turns so that the REST
//! adapter can include it in the request body. Including `previous_response_id`
//! tells the Responses API to treat the new request as an incremental delta
//! from the prior response, reducing upstream payload size by omitting already-
//! processed context.
//!
//! Actual WebSocket transport is **not** implemented here. The `websocket`
//! feature flag gates the `tokio-tungstenite` dependency for future WS
//! transport; this module is usable without that feature.

use std::sync::Arc;

use tokio::sync::Mutex;

/// Session-level state for OpenAI Responses API delta requests.
///
/// Tracks the last response ID for `previous_response_id` delta requests and
/// the connection reuse count. Thread-safe behind [`SharedWsSession`].
#[derive(Debug, Default)]
pub struct OpenAiWsSession {
	/// The last response_id from the server, used for delta requests.
	last_response_id: Option<String>,
	/// Number of turns that reused this session's connection.
	reuse_count: u32,
	/// Whether WebSocket / delta mode is active for this session.
	enabled: bool,
}

impl OpenAiWsSession {
	pub fn new(enabled: bool) -> Self {
		Self {
			last_response_id: None,
			reuse_count: 0,
			enabled,
		}
	}

	/// Record a successful response, storing its ID for next turn's delta.
	pub fn record_response(&mut self, response_id: String) {
		self.last_response_id = Some(response_id);
		self.reuse_count += 1;
	}

	/// Get the `previous_response_id` for a delta request, if available.
	pub fn previous_response_id(&self) -> Option<&str> {
		self.last_response_id.as_deref()
	}

	/// Get the reuse count for trace reporting.
	pub fn reuse_count(&self) -> u32 {
		self.reuse_count
	}

	/// Whether delta mode is enabled for this session.
	pub fn is_enabled(&self) -> bool {
		self.enabled
	}

	/// Reset session state (e.g., on error or session end).
	pub fn reset(&mut self) {
		self.last_response_id = None;
		self.reuse_count = 0;
	}
}

/// Thread-safe wrapper for session state.
pub type SharedWsSession = Arc<Mutex<OpenAiWsSession>>;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_ws_session_state_management() {
		let mut session = OpenAiWsSession::new(true);
		assert!(session.is_enabled());
		assert_eq!(session.previous_response_id(), None);
		assert_eq!(session.reuse_count(), 0);

		session.record_response("resp_abc123".to_string());
		assert_eq!(session.previous_response_id(), Some("resp_abc123"));
		assert_eq!(session.reuse_count(), 1);

		session.record_response("resp_def456".to_string());
		assert_eq!(session.previous_response_id(), Some("resp_def456"));
		assert_eq!(session.reuse_count(), 2);

		session.reset();
		assert_eq!(session.previous_response_id(), None);
		assert_eq!(session.reuse_count(), 0);
		// enabled is not reset — it is a session-level config, not turn state
		assert!(session.is_enabled());
	}

	#[test]
	fn disabled_session_records_but_signals_disabled() {
		let mut session = OpenAiWsSession::new(false);
		assert!(!session.is_enabled());

		// record_response still stores the ID even if disabled,
		// so the adapter can decide whether to use it based on is_enabled()
		session.record_response("resp_xyz".to_string());
		assert_eq!(session.previous_response_id(), Some("resp_xyz"));
		assert_eq!(session.reuse_count(), 1);
	}
}
