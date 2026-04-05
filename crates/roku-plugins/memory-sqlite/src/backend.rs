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

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use roku_memory::{
	PendingLoopSnapshot, PendingLoopSnapshotBackend, PendingLoopSnapshotError,
	SessionCreateRequest, SessionDeleteMode, SessionDescriptor, SessionManagementBackend,
	SessionManagementError, SessionState, SessionStateBackend, SessionStateError,
	ShortTermContinuityBackend, ShortTermContinuityError, normalize_session_name,
	resolve_session_name,
};
use thiserror::Error;

use crate::SqliteMemoryConfig;
use crate::store::{
	SqliteConversationRepository, SqliteMemoryStoreConfig, SqlitePendingLoopSnapshotRepository,
	SqliteSessionCatalogRepository, SqliteSessionPreferenceRepository,
};

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
	pub session_management: SqliteSessionManagementAdapter,
}

impl SqliteMemoryAdapters {
	/// Connects all SQLite-backed memory adapters against one SQLite database.
	pub fn connect(config: SqliteMemoryConfig) -> Result<Self, SqliteMemoryAdapterError> {
		let store_config = SqliteMemoryStoreConfig::new(config.path);
		Ok(Self {
			session_state: SqliteSessionStateAdapter::connect(store_config.clone())?,
			short_term: SqliteShortTermContinuityAdapter::connect(store_config.clone())?,
			pending_loop: SqlitePendingLoopSnapshotAdapter::connect(store_config.clone())?,
			session_management: SqliteSessionManagementAdapter::connect(store_config)?,
		})
	}
}

/// SQLite implementation of [`SessionStateBackend`].
#[derive(Debug, Clone)]
pub struct SqliteSessionStateAdapter {
	inner: SqliteSessionPreferenceRepository,
}

impl SqliteSessionStateAdapter {
	pub fn connect(config: SqliteMemoryStoreConfig) -> Result<Self, SqliteMemoryAdapterError> {
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
	pub fn connect(config: SqliteMemoryStoreConfig) -> Result<Self, SqliteMemoryAdapterError> {
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
///
/// Stores pending-loop snapshots in a dedicated `pending_loop_snapshots` table.
/// On load, falls back to the legacy `session_preferences.pending_loop` field
/// if the dedicated table has no row, promoting the data on first access.
#[derive(Debug)]
pub struct SqlitePendingLoopSnapshotAdapter {
	inner: SqlitePendingLoopSnapshotRepository,
	legacy: SqliteSessionPreferenceRepository,
}

impl SqlitePendingLoopSnapshotAdapter {
	pub fn connect(config: SqliteMemoryStoreConfig) -> Result<Self, SqliteMemoryAdapterError> {
		let inner = SqlitePendingLoopSnapshotRepository::connect(config.clone())
			.map_err(|error| SqliteMemoryAdapterError::Resolution(error.to_string()))?;
		let legacy = SqliteSessionPreferenceRepository::connect(config)
			.map_err(|error| SqliteMemoryAdapterError::Resolution(error.to_string()))?;
		Ok(Self { inner, legacy })
	}
}

impl PendingLoopSnapshotBackend for SqlitePendingLoopSnapshotAdapter {
	fn load_pending_loop_snapshot(
		&self,
		session_id: &str,
	) -> Result<Option<PendingLoopSnapshot>, PendingLoopSnapshotError> {
		// Try dedicated table first.
		let from_dedicated = self
			.inner
			.load_snapshot(session_id)
			.map(|opt| {
				opt.map(|(run_id, loop_state_json)| PendingLoopSnapshot {
					run_id,
					loop_state_json,
				})
			})
			.map_err(|error| PendingLoopSnapshotError::Backend(error.to_string()))?;
		if from_dedicated.is_some() {
			return Ok(from_dedicated);
		}

		// Fallback: check legacy session_preferences for pre-migration data.
		let legacy_snapshot = self
			.legacy
			.load_preferences(session_id)
			.map_err(|error| PendingLoopSnapshotError::Backend(error.to_string()))?
			.and_then(|prefs| prefs.pending_loop);
		if let Some(ref snapshot) = legacy_snapshot {
			// Lazy promotion: copy to dedicated table so subsequent loads skip the fallback.
			let _ = self.inner.store_snapshot(
				session_id,
				&snapshot.run_id,
				&snapshot.loop_state_json,
			);
		}
		Ok(legacy_snapshot)
	}

	fn save_pending_loop_snapshot(
		&self,
		session_id: &str,
		snapshot: Option<PendingLoopSnapshot>,
	) -> Result<(), PendingLoopSnapshotError> {
		match snapshot {
			Some(snapshot) => self
				.inner
				.store_snapshot(session_id, &snapshot.run_id, &snapshot.loop_state_json)
				.map_err(|error| PendingLoopSnapshotError::Backend(error.to_string())),
			None => self
				.inner
				.delete_snapshot(session_id)
				.map_err(|error| PendingLoopSnapshotError::Backend(error.to_string())),
		}
	}
}

/// SQLite implementation of [`SessionManagementBackend`].
#[derive(Debug, Clone)]
pub struct SqliteSessionManagementAdapter {
	inner: SqliteSessionCatalogRepository,
}

impl SqliteSessionManagementAdapter {
	pub fn connect(config: SqliteMemoryStoreConfig) -> Result<Self, SqliteMemoryAdapterError> {
		let inner = SqliteSessionCatalogRepository::connect(config)
			.map_err(|error| SqliteMemoryAdapterError::Resolution(error.to_string()))?;
		Ok(Self { inner })
	}
}

impl SessionManagementBackend for SqliteSessionManagementAdapter {
	fn create_session(
		&mut self,
		binding_id: &str,
		request: SessionCreateRequest,
	) -> Result<SessionDescriptor, SessionManagementError> {
		let binding_id = normalize_binding_id(binding_id)?;
		let descriptor = loop {
			let session_id = generate_session_id();
			let now = now_unix_ms_i64();
			let descriptor = SessionDescriptor {
				name: resolve_session_name(&request, &session_id)?,
				session_id,
				created_at_unix_ms: now,
				updated_at_unix_ms: now,
			};
			match self.inner.insert_session(&binding_id, &descriptor) {
				Ok(()) => break descriptor,
				Err(crate::store::SqliteMemoryStoreError::Sqlite(
					rusqlite::Error::SqliteFailure(error, _),
				)) if error.code == rusqlite::ErrorCode::ConstraintViolation => continue,
				Err(error) => {
					return Err(SessionManagementError::Backend(error.to_string()));
				}
			}
		};
		Ok(descriptor)
	}

	fn get_session(
		&self,
		binding_id: &str,
		session_id: &str,
	) -> Result<Option<SessionDescriptor>, SessionManagementError> {
		self.inner
			.load_session(
				&normalize_binding_id(binding_id)?,
				&normalize_session_id(session_id)?,
			)
			.map_err(|error| SessionManagementError::Backend(error.to_string()))
	}

	fn list_sessions(
		&self,
		binding_id: &str,
	) -> Result<Vec<roku_memory::SessionSummary>, SessionManagementError> {
		self.inner
			.list_sessions(&normalize_binding_id(binding_id)?)
			.map_err(|error| SessionManagementError::Backend(error.to_string()))
	}

	fn rename_session(
		&mut self,
		binding_id: &str,
		session_id: &str,
		new_name: &str,
	) -> Result<SessionDescriptor, SessionManagementError> {
		let binding_id = normalize_binding_id(binding_id)?;
		let session_id = normalize_session_id(session_id)?;
		let name = normalize_session_name(new_name)?;
		self.inner
			.rename_session(&binding_id, &session_id, &name, now_unix_ms_i64())
			.map_err(|error| SessionManagementError::Backend(error.to_string()))?
			.ok_or(SessionManagementError::NotFound(session_id))
	}

	fn delete_session(
		&mut self,
		binding_id: &str,
		session_id: &str,
		_mode: SessionDeleteMode,
	) -> Result<(), SessionManagementError> {
		let deleted = self
			.inner
			.delete_session(
				&normalize_binding_id(binding_id)?,
				&normalize_session_id(session_id)?,
			)
			.map_err(|error| SessionManagementError::Backend(error.to_string()))?;
		if deleted {
			Ok(())
		} else {
			Err(SessionManagementError::NotFound(session_id.to_string()))
		}
	}

	fn get_active_session(
		&self,
		binding_id: &str,
	) -> Result<Option<SessionDescriptor>, SessionManagementError> {
		let binding_id = normalize_binding_id(binding_id)?;
		let active = self
			.inner
			.load_active_session(&binding_id)
			.map_err(|error| SessionManagementError::Backend(error.to_string()))?;
		if active.is_some() {
			return Ok(active);
		}
		if self
			.inner
			.active_session_exists(&binding_id)
			.map_err(|error| SessionManagementError::Backend(error.to_string()))?
		{
			return Err(SessionManagementError::Backend(format!(
				"active session pointer references missing descriptor for binding_id={binding_id}"
			)));
		}
		Ok(None)
	}

	fn select_active_session(
		&mut self,
		binding_id: &str,
		session_id: &str,
	) -> Result<SessionDescriptor, SessionManagementError> {
		let binding_id = normalize_binding_id(binding_id)?;
		let session_id = normalize_session_id(session_id)?;
		self.inner
			.select_active_session(&binding_id, &session_id, now_unix_ms_i64())
			.map_err(|error| SessionManagementError::Backend(error.to_string()))?
			.ok_or(SessionManagementError::NotFound(session_id))
	}
}

fn normalize_binding_id(binding_id: &str) -> Result<String, SessionManagementError> {
	let binding_id = binding_id.trim();
	if binding_id.is_empty() {
		return Err(SessionManagementError::Validation(
			"binding_id must not be empty".to_string(),
		));
	}
	Ok(binding_id.to_string())
}

fn normalize_session_id(session_id: &str) -> Result<String, SessionManagementError> {
	let session_id = session_id.trim();
	if session_id.is_empty() {
		return Err(SessionManagementError::Validation(
			"session_id must not be empty".to_string(),
		));
	}
	Ok(session_id.to_string())
}

fn generate_session_id() -> String {
	static NEXT_SESSION_COUNTER: AtomicU64 = AtomicU64::new(1);
	let counter = NEXT_SESSION_COUNTER.fetch_add(1, Ordering::Relaxed);
	format!("session-{:020}-{:06}", now_unix_ms_i64().max(0), counter)
}

fn now_unix_ms_i64() -> i64 {
	SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.unwrap_or_default()
		.as_millis()
		.try_into()
		.unwrap_or(i64::MAX)
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
			session_management: _,
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

	#[test]
	fn sqlite_session_management_roundtrips_and_clears_transport_state() {
		let tempdir = tempfile::tempdir().expect("tempdir should exist");
		let mut adapters = SqliteMemoryAdapters::connect(SqliteMemoryConfig {
			path: tempdir.path().join("memory.db"),
		})
		.expect("sqlite adapters should connect");
		let binding_id = "chat-1";

		let descriptor = adapters
			.session_management
			.create_session(binding_id, SessionCreateRequest::default())
			.expect("session should create");
		let selected = adapters
			.session_management
			.select_active_session(binding_id, &descriptor.session_id)
			.expect("session should select");
		assert_eq!(selected.session_id, descriptor.session_id);

		adapters
			.session_state
			.save_session_state(
				&descriptor.session_id,
				SessionState {
					planning_mode: None,
					pending_loop: None,
				},
			)
			.expect("session state should save");
		adapters
			.short_term
			.append_continuity_turn(
				&descriptor.session_id,
				ConversationTurn {
					role: ConversationRole::User,
					content: "hello".to_string(),
					created_at_unix_ms: 1,
				},
			)
			.expect("continuity should save");
		adapters
			.pending_loop
			.save_pending_loop_snapshot(
				&descriptor.session_id,
				Some(PendingLoopSnapshot {
					run_id: "run-1".to_string(),
					loop_state_json: "{\"status\":\"waiting\"}".to_string(),
				}),
			)
			.expect("pending loop should save");

		let renamed = adapters
			.session_management
			.rename_session(binding_id, &descriptor.session_id, "Renamed Session")
			.expect("session should rename");
		assert_eq!(renamed.name, "Renamed Session");
		assert_eq!(
			adapters
				.session_management
				.list_sessions(binding_id)
				.expect("sessions should list")
				.len(),
			1
		);

		adapters
			.session_management
			.delete_session(
				binding_id,
				&descriptor.session_id,
				SessionDeleteMode::RetainLongTermMemory,
			)
			.expect("session should delete");
		assert!(
			adapters
				.session_management
				.get_active_session(binding_id)
				.expect("active session should load")
				.is_none()
		);
		assert!(
			adapters
				.session_state
				.load_session_state(&descriptor.session_id)
				.expect("session state should load")
				.is_none()
		);
		assert!(
			adapters
				.short_term
				.load_short_term_continuity(&descriptor.session_id, 8)
				.expect("continuity should load")
				.is_empty()
		);
		assert!(
			adapters
				.pending_loop
				.load_pending_loop_snapshot(&descriptor.session_id)
				.expect("pending loop should load")
				.is_none()
		);
	}

	#[test]
	fn pending_loop_snapshot_survives_simulated_process_restart() {
		let tempdir = tempfile::tempdir().unwrap();
		let db_path = tempdir.path().join("pending-restart-test.db");
		let config = crate::store::SqliteMemoryStoreConfig::new(db_path.clone());

		// Phase 1: Store snapshot
		{
			let adapter = SqlitePendingLoopSnapshotAdapter::connect(config.clone()).unwrap();
			let snapshot = PendingLoopSnapshot {
				run_id: "run-restart-test".to_string(),
				loop_state_json:
					r#"{"session_id":"s1","status":"awaiting_user","goal":"test restart"}"#
						.to_string(),
			};
			adapter
				.save_pending_loop_snapshot("session-restart", Some(snapshot))
				.unwrap();
		} // adapter dropped -- simulates process exit

		// Phase 2: New adapter, same database -- simulates restart
		{
			let adapter = SqlitePendingLoopSnapshotAdapter::connect(config.clone()).unwrap();
			let loaded = adapter
				.load_pending_loop_snapshot("session-restart")
				.unwrap();
			assert!(loaded.is_some(), "snapshot should survive restart");
			let loaded = loaded.unwrap();
			assert_eq!(loaded.run_id, "run-restart-test");
			assert!(loaded.loop_state_json.contains("awaiting_user"));

			// Phase 3: Delete and verify
			adapter
				.clear_pending_loop_snapshot("session-restart")
				.unwrap();
			let after_delete = adapter
				.load_pending_loop_snapshot("session-restart")
				.unwrap();
			assert!(after_delete.is_none(), "snapshot should be deleted");
		}
	}

	#[test]
	fn pending_loop_store_load_returns_none_for_nonexistent() {
		let tempdir = tempfile::tempdir().unwrap();
		let config = crate::store::SqliteMemoryStoreConfig::new(
			tempdir.path().join("pending-nonexistent.db"),
		);
		let adapter = SqlitePendingLoopSnapshotAdapter::connect(config).unwrap();

		let loaded = adapter
			.load_pending_loop_snapshot("no-such-session")
			.unwrap();
		assert!(loaded.is_none());
	}

	#[test]
	fn pending_loop_store_overwrites_previous_snapshot() {
		let tempdir = tempfile::tempdir().unwrap();
		let config =
			crate::store::SqliteMemoryStoreConfig::new(tempdir.path().join("pending-overwrite.db"));
		let adapter = SqlitePendingLoopSnapshotAdapter::connect(config).unwrap();

		let first = PendingLoopSnapshot {
			run_id: "run-v1".to_string(),
			loop_state_json: r#"{"version":1}"#.to_string(),
		};
		adapter
			.save_pending_loop_snapshot("session-ow", Some(first))
			.unwrap();

		let second = PendingLoopSnapshot {
			run_id: "run-v2".to_string(),
			loop_state_json: r#"{"version":2}"#.to_string(),
		};
		adapter
			.save_pending_loop_snapshot("session-ow", Some(second))
			.unwrap();

		let loaded = adapter
			.load_pending_loop_snapshot("session-ow")
			.unwrap()
			.expect("snapshot should exist after overwrite");
		assert_eq!(loaded.run_id, "run-v2");
		assert!(loaded.loop_state_json.contains("\"version\":2"));
	}

	#[test]
	fn pending_loop_load_falls_back_to_legacy_session_preferences() {
		let tempdir = tempfile::tempdir().unwrap();
		let config =
			crate::store::SqliteMemoryStoreConfig::new(tempdir.path().join("legacy-fallback.db"));

		// Phase 1: Write a pending-loop snapshot via the LEGACY path (session_preferences).
		{
			let mut legacy_repo =
				crate::store::SqliteSessionPreferenceRepository::connect(config.clone()).unwrap();
			let prefs = roku_common_types::SessionPreferences {
				pending_loop: Some(PendingLoopSnapshot {
					run_id: "legacy-run".to_string(),
					loop_state_json: r#"{"status":"awaiting_user","goal":"legacy"}"#.to_string(),
				}),
				..Default::default()
			};
			legacy_repo.save_preferences("session-legacy", prefs).unwrap();
		}

		// Phase 2: Load via the NEW adapter — should fall back to legacy and return data.
		let adapter = SqlitePendingLoopSnapshotAdapter::connect(config.clone()).unwrap();
		let loaded = adapter
			.load_pending_loop_snapshot("session-legacy")
			.unwrap();
		assert!(loaded.is_some(), "should fall back to legacy session_preferences");
		let loaded = loaded.unwrap();
		assert_eq!(loaded.run_id, "legacy-run");
		assert!(loaded.loop_state_json.contains("awaiting_user"));

		// Phase 3: After fallback read, data should be promoted to dedicated table.
		// Verify by checking the dedicated table directly.
		let repo = crate::store::SqlitePendingLoopSnapshotRepository::connect(config).unwrap();
		let promoted = repo.load_snapshot("session-legacy").unwrap();
		assert!(promoted.is_some(), "legacy data should be promoted to dedicated table");
	}
}
