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

//! Roku-owned short-term continuity namespace.
//!
//! Phase 2 moved the provider-neutral short-term continuity contract here.
//! Concrete transcript stores remain adapter implementations; they no longer
//! define the continuity semantics themselves.

use std::collections::HashMap;

use roku_common_types::ConversationTurn;
use thiserror::Error;

/// Error returned by short-term continuity backends.
#[derive(Debug, Error)]
pub enum ShortTermContinuityError {
	#[error("short-term continuity backend failed: {0}")]
	Backend(String),
}

/// Provider-neutral short-term continuity contract.
///
/// Implementations own recent transcript storage only. They do not define
/// recall policy, long-term memory semantics, or entry-specific workflow.
pub trait ShortTermContinuityBackend: Send {
	fn append_continuity_turn(
		&mut self,
		session_id: &str,
		turn: ConversationTurn,
	) -> Result<(), ShortTermContinuityError>;

	fn load_short_term_continuity(
		&self,
		session_id: &str,
		limit: usize,
	) -> Result<Vec<ConversationTurn>, ShortTermContinuityError>;

	fn delete_continuity(&mut self, session_id: &str) -> Result<(), ShortTermContinuityError>;
}

/// Disabled short-term continuity backend used when continuity is intentionally unavailable.
#[derive(Debug, Default)]
pub struct NoopShortTermContinuityBackend;

impl ShortTermContinuityBackend for NoopShortTermContinuityBackend {
	fn append_continuity_turn(
		&mut self,
		_session_id: &str,
		_turn: ConversationTurn,
	) -> Result<(), ShortTermContinuityError> {
		Ok(())
	}

	fn load_short_term_continuity(
		&self,
		_session_id: &str,
		_limit: usize,
	) -> Result<Vec<ConversationTurn>, ShortTermContinuityError> {
		Ok(Vec::new())
	}

	fn delete_continuity(&mut self, _session_id: &str) -> Result<(), ShortTermContinuityError> {
		Ok(())
	}
}

/// In-memory short-term backend used by core/runtime tests.
///
/// Keeping this test double in `roku-memory` avoids leaking continuity
/// semantics back into transitional persistence crates.
#[derive(Debug, Default)]
pub struct InMemoryShortTermContinuityBackend {
	turns_by_session: HashMap<String, Vec<ConversationTurn>>,
}

impl ShortTermContinuityBackend for InMemoryShortTermContinuityBackend {
	fn append_continuity_turn(
		&mut self,
		session_id: &str,
		turn: ConversationTurn,
	) -> Result<(), ShortTermContinuityError> {
		self.turns_by_session
			.entry(session_id.to_string())
			.or_default()
			.push(turn);
		Ok(())
	}

	fn load_short_term_continuity(
		&self,
		session_id: &str,
		limit: usize,
	) -> Result<Vec<ConversationTurn>, ShortTermContinuityError> {
		let Some(turns) = self.turns_by_session.get(session_id) else {
			return Ok(Vec::new());
		};
		let start = turns.len().saturating_sub(limit);
		Ok(turns[start..].to_vec())
	}

	fn delete_continuity(&mut self, session_id: &str) -> Result<(), ShortTermContinuityError> {
		self.turns_by_session.remove(session_id);
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use roku_common_types::ConversationRole;

	use super::*;

	#[test]
	fn in_memory_short_term_backend_roundtrips_recent_turns() {
		let mut backend = InMemoryShortTermContinuityBackend::default();
		backend
			.append_continuity_turn(
				"session-1",
				ConversationTurn {
					role: ConversationRole::User,
					content: "hello".to_string(),
					created_at_unix_ms: 1,
				},
			)
			.expect("first turn should save");
		backend
			.append_continuity_turn(
				"session-1",
				ConversationTurn {
					role: ConversationRole::Assistant,
					content: "world".to_string(),
					created_at_unix_ms: 2,
				},
			)
			.expect("second turn should save");

		assert_eq!(
			backend
				.load_short_term_continuity("session-1", 1)
				.expect("recent turns should load"),
			vec![ConversationTurn {
				role: ConversationRole::Assistant,
				content: "world".to_string(),
				created_at_unix_ms: 2,
			}]
		);

		backend
			.delete_continuity("session-1")
			.expect("turns should delete");
		assert!(
			backend
				.load_short_term_continuity("session-1", 4)
				.expect("deleted turns should load")
				.is_empty()
		);
	}
}
