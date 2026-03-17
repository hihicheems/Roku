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

//! SQLite implementations of Roku-owned memory contracts.

use std::sync::Arc;
use std::sync::Mutex;

use roku_memory::{
	DisabledMemoryLifecyclePolicy, MemoryAdapterAvailability, MemoryBackendId,
	MemorySubsystemRegistration, NoopLongTermMemoryBackend, PendingLoopSnapshot,
	PendingLoopSnapshotBackend, PendingLoopSnapshotError, ResolvedMemorySubsystem, SessionState,
	SessionStateBackend, SessionStateError, ShortTermContinuityBackend, ShortTermContinuityError,
};
use roku_state_store::{
	SqliteConversationRepository, SqliteSessionPreferenceRepository, SqliteStoreConfig,
};
use thiserror::Error;

use crate::SqliteMemoryConfig;

/// Connection or resolution failures for SQLite memory adapters.
#[derive(Debug, Error)]
pub enum SqliteMemoryAdapterError {
	#[error("failed to resolve sqlite memory adapter: {0}")]
	Resolution(String),
}

/// Provider-specific SQLite adapter bundle used by registry-backed entry resolution.
pub struct SqliteMemoryAdapters {
	pub session_state: SqliteSessionStateAdapter,
	pub short_term: SqliteShortTermContinuityAdapter,
	pub pending_loop: SqlitePendingLoopSnapshotAdapter,
}

impl SqliteMemoryAdapters {
	/// Connects all SQLite-backed memory adapters against one SQLite database.
	pub fn connect(config: SqliteMemoryConfig) -> Result<Self, SqliteMemoryAdapterError> {
		let store_config = SqliteStoreConfig::new(config.path);
		Ok(Self {
			session_state: SqliteSessionStateAdapter::connect(store_config.clone())?,
			short_term: SqliteShortTermContinuityAdapter::connect(store_config.clone())?,
			pending_loop: SqlitePendingLoopSnapshotAdapter::connect(store_config)?,
		})
	}
}

/// Registration surface for the SQLite memory adapter.
pub struct SqliteMemoryRegistration;

impl SqliteMemoryRegistration {
	/// Advertises the currently implemented SQLite memory capabilities.
	pub const fn availability() -> MemoryAdapterAvailability {
		MemoryAdapterAvailability {
			backend: MemoryBackendId::Sqlite,
			long_term: false,
			short_term: true,
			session_state: true,
			pending_loop: true,
		}
	}

	/// Resolves SQLite-backed continuity/session adapters into a provider-neutral bundle.
	pub fn resolve_subsystem(
		config: SqliteMemoryConfig,
	) -> Result<ResolvedMemorySubsystem, SqliteMemoryAdapterError> {
		let adapters = SqliteMemoryAdapters::connect(config)?;
		Ok(ResolvedMemorySubsystem::with_parts(
			Arc::new(NoopLongTermMemoryBackend),
			Box::new(adapters.short_term),
			Box::new(adapters.session_state),
			Box::new(adapters.pending_loop),
			Arc::new(DisabledMemoryLifecyclePolicy),
		))
	}
}

/// Config-bound SQLite registration consumed by the Roku entry registry.
#[derive(Debug, Clone)]
pub struct SqliteMemorySubsystemRegistration {
	config: SqliteMemoryConfig,
}

impl SqliteMemorySubsystemRegistration {
	pub fn new(config: SqliteMemoryConfig) -> Self {
		Self { config }
	}
}

impl MemorySubsystemRegistration for SqliteMemorySubsystemRegistration {
	fn availability(&self) -> MemoryAdapterAvailability {
		SqliteMemoryRegistration::availability()
	}

	fn resolve_subsystem(&self) -> Result<ResolvedMemorySubsystem, String> {
		SqliteMemoryRegistration::resolve_subsystem(self.config.clone())
			.map_err(|error| error.to_string())
	}
}

/// SQLite implementation of [`SessionStateBackend`].
#[derive(Debug, Clone)]
pub struct SqliteSessionStateAdapter {
	inner: SqliteSessionPreferenceRepository,
}

impl SqliteSessionStateAdapter {
	pub fn connect(config: SqliteStoreConfig) -> Result<Self, SqliteMemoryAdapterError> {
		let inner = SqliteSessionPreferenceRepository::connect(config)
			.map_err(|error| SqliteMemoryAdapterError::Resolution(error.to_string()))?;
		Ok(Self { inner })
	}
}

impl SessionStateBackend for SqliteSessionStateAdapter {
	fn save_session_state(
		&mut self,
		session_id: &str,
		state: SessionState,
	) -> Result<(), SessionStateError> {
		self.inner
			.save_preferences(session_id, state)
			.map_err(|error| SessionStateError::Backend(error.to_string()))
	}

	fn load_session_state(
		&self,
		session_id: &str,
	) -> Result<Option<SessionState>, SessionStateError> {
		self.inner
			.load_preferences(session_id)
			.map_err(|error| SessionStateError::Backend(error.to_string()))
	}

	fn delete_session_state(&mut self, session_id: &str) -> Result<(), SessionStateError> {
		self.inner
			.delete_preferences(session_id)
			.map_err(|error| SessionStateError::Backend(error.to_string()))
	}
}

/// SQLite implementation of [`ShortTermContinuityBackend`].
#[derive(Debug, Clone)]
pub struct SqliteShortTermContinuityAdapter {
	inner: SqliteConversationRepository,
}

impl SqliteShortTermContinuityAdapter {
	pub fn connect(config: SqliteStoreConfig) -> Result<Self, SqliteMemoryAdapterError> {
		let inner = SqliteConversationRepository::connect(config)
			.map_err(|error| SqliteMemoryAdapterError::Resolution(error.to_string()))?;
		Ok(Self { inner })
	}
}

impl ShortTermContinuityBackend for SqliteShortTermContinuityAdapter {
	fn append_continuity_turn(
		&mut self,
		session_id: &str,
		turn: roku_common_types::ConversationTurn,
	) -> Result<(), ShortTermContinuityError> {
		self.inner
			.append_turn(session_id, turn)
			.map_err(|error| ShortTermContinuityError::Backend(error.to_string()))
	}

	fn load_short_term_continuity(
		&self,
		session_id: &str,
		limit: usize,
	) -> Result<Vec<roku_common_types::ConversationTurn>, ShortTermContinuityError> {
		self.inner
			.load_recent_turns(session_id, limit)
			.map_err(|error| ShortTermContinuityError::Backend(error.to_string()))
	}

	fn delete_continuity(&mut self, session_id: &str) -> Result<(), ShortTermContinuityError> {
		self.inner
			.delete_conversation(session_id)
			.map_err(|error| ShortTermContinuityError::Backend(error.to_string()))
	}
}

/// SQLite implementation of [`PendingLoopSnapshotBackend`].
#[derive(Debug)]
pub struct SqlitePendingLoopSnapshotAdapter {
	inner: Mutex<SqliteSessionPreferenceRepository>,
}

impl SqlitePendingLoopSnapshotAdapter {
	pub fn connect(config: SqliteStoreConfig) -> Result<Self, SqliteMemoryAdapterError> {
		let inner = SqliteSessionPreferenceRepository::connect(config)
			.map_err(|error| SqliteMemoryAdapterError::Resolution(error.to_string()))?;
		Ok(Self {
			inner: Mutex::new(inner),
		})
	}
}

impl PendingLoopSnapshotBackend for SqlitePendingLoopSnapshotAdapter {
	fn load_pending_loop_snapshot(
		&self,
		session_id: &str,
	) -> Result<Option<PendingLoopSnapshot>, PendingLoopSnapshotError> {
		let store = self.inner.lock().map_err(|_| {
			PendingLoopSnapshotError::Backend("sqlite pending-loop store is poisoned".to_string())
		})?;
		Ok(store
			.load_preferences(session_id)
			.map_err(|error| PendingLoopSnapshotError::Backend(error.to_string()))?
			.and_then(|state| state.pending_loop))
	}

	fn save_pending_loop_snapshot(
		&self,
		session_id: &str,
		snapshot: Option<PendingLoopSnapshot>,
	) -> Result<(), PendingLoopSnapshotError> {
		let mut store = self.inner.lock().map_err(|_| {
			PendingLoopSnapshotError::Backend("sqlite pending-loop store is poisoned".to_string())
		})?;
		let mut state = store
			.load_preferences(session_id)
			.map_err(|error| PendingLoopSnapshotError::Backend(error.to_string()))?
			.unwrap_or_default();
		state.pending_loop = snapshot;
		store
			.save_preferences(session_id, state)
			.map_err(|error| PendingLoopSnapshotError::Backend(error.to_string()))
	}
}

#[cfg(test)]
mod tests {
	use roku_common_types::{ConversationRole, ConversationTurn};

	use super::*;

	#[test]
	fn sqlite_memory_adapters_cover_short_term_session_and_pending_loop() {
		let tempdir = tempfile::tempdir().expect("tempdir should exist");
		let adapters = SqliteMemoryAdapters::connect(SqliteMemoryConfig {
			path: tempdir.path().join("memory.db"),
		})
		.expect("sqlite adapters should connect");

		let SqliteMemoryAdapters {
			mut session_state,
			mut short_term,
			pending_loop,
		} = adapters;

		short_term
			.append_continuity_turn(
				"session-1",
				ConversationTurn {
					role: ConversationRole::User,
					content: "hello".to_string(),
					created_at_unix_ms: 1,
				},
			)
			.expect("continuity turn should save");
		assert_eq!(
			short_term
				.load_short_term_continuity("session-1", 8)
				.expect("continuity should load")
				.len(),
			1
		);

		let state = SessionState {
			planning_mode: None,
			..SessionState::default()
		};
		session_state
			.save_session_state("session-1", state)
			.expect("session state should save");
		assert!(
			session_state
				.load_session_state("session-1")
				.expect("session state should load")
				.is_some()
		);

		let snapshot = PendingLoopSnapshot {
			run_id: "run-1".to_string(),
			loop_state_json: "{\"status\":\"waiting\"}".to_string(),
		};
		pending_loop
			.save_pending_loop_snapshot("session-1", Some(snapshot.clone()))
			.expect("pending loop should save");
		assert_eq!(
			pending_loop
				.load_pending_loop_snapshot("session-1")
				.expect("pending loop should load"),
			Some(snapshot)
		);
	}
}
