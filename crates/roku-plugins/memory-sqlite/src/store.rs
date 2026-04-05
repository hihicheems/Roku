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

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use roku_common_types::{ConversationTurn, SessionPreferences};
use roku_memory::{SessionDescriptor, SessionSummary};
use rusqlite::{Connection, OptionalExtension, params};
use thiserror::Error;

const SQLITE_BUSY_TIMEOUT_MS: u64 = 5_000;
const SQLITE_WAL_AUTOCHECKPOINT_PAGES: i64 = 200;
const SQLITE_APPLICATION_ID: i64 = 0x524f_4b55;
const SQLITE_USER_VERSION: i64 = 3;

#[derive(Debug, Error)]
pub enum SqliteMemoryStoreError {
	#[error("io error: {0}")]
	Io(#[from] std::io::Error),
	#[error("serialization error: {0}")]
	Serde(#[from] serde_json::Error),
	#[error("sqlite error: {0}")]
	Sqlite(#[from] rusqlite::Error),
	#[error("storage error: {0}")]
	Storage(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqliteMemoryStoreConfig {
	pub path: PathBuf,
}

impl SqliteMemoryStoreConfig {
	pub fn new(path: impl Into<PathBuf>) -> Self {
		Self { path: path.into() }
	}
}

#[derive(Debug, Clone)]
pub struct SqliteSessionPreferenceRepository {
	config: SqliteMemoryStoreConfig,
}

impl SqliteSessionPreferenceRepository {
	pub fn connect(config: SqliteMemoryStoreConfig) -> Result<Self, SqliteMemoryStoreError> {
		open_connection(&config.path)?;
		Ok(Self { config })
	}

	fn open(&self) -> Result<Connection, SqliteMemoryStoreError> {
		open_connection(&self.config.path)
	}

	pub fn save_preferences(
		&mut self,
		session_id: &str,
		preferences: SessionPreferences,
	) -> Result<(), SqliteMemoryStoreError> {
		let connection = self.open()?;
		connection.execute(
			"INSERT INTO session_preferences (session_id, preferences_json, updated_at_unix_ms) VALUES (?1, ?2, ?3)
			 ON CONFLICT(session_id) DO UPDATE SET preferences_json = excluded.preferences_json, updated_at_unix_ms = excluded.updated_at_unix_ms",
			params![
				session_id,
				serde_json::to_string(&preferences)?,
				sql_i64_from_u64(now_unix_ms(), "session_preferences.updated_at_unix_ms")?,
			],
		)?;
		Ok(())
	}

	pub fn load_preferences(
		&self,
		session_id: &str,
	) -> Result<Option<SessionPreferences>, SqliteMemoryStoreError> {
		let connection = self.open()?;
		let encoded = connection
			.query_row(
				"SELECT preferences_json FROM session_preferences WHERE session_id = ?1",
				params![session_id],
				|row| row.get::<_, String>(0),
			)
			.optional()?;
		encoded
			.map(|value| serde_json::from_str(&value).map_err(SqliteMemoryStoreError::from))
			.transpose()
	}

	pub fn delete_preferences(&mut self, session_id: &str) -> Result<(), SqliteMemoryStoreError> {
		let connection = self.open()?;
		connection.execute(
			"DELETE FROM session_preferences WHERE session_id = ?1",
			params![session_id],
		)?;
		Ok(())
	}
}

#[derive(Debug, Clone)]
pub struct SqlitePendingLoopSnapshotRepository {
	config: SqliteMemoryStoreConfig,
}

impl SqlitePendingLoopSnapshotRepository {
	pub fn connect(config: SqliteMemoryStoreConfig) -> Result<Self, SqliteMemoryStoreError> {
		open_connection(&config.path)?;
		Ok(Self { config })
	}

	fn open(&self) -> Result<Connection, SqliteMemoryStoreError> {
		open_connection(&self.config.path)
	}

	pub fn store_snapshot(
		&self,
		session_id: &str,
		run_id: &str,
		loop_state_json: &str,
	) -> Result<(), SqliteMemoryStoreError> {
		let connection = self.open()?;
		connection.execute(
			"INSERT INTO pending_loop_snapshots (session_id, run_id, loop_state_json, updated_at_unix_ms)
			 VALUES (?1, ?2, ?3, ?4)
			 ON CONFLICT(session_id) DO UPDATE SET
			   run_id = excluded.run_id,
			   loop_state_json = excluded.loop_state_json,
			   updated_at_unix_ms = excluded.updated_at_unix_ms",
			params![
				session_id,
				run_id,
				loop_state_json,
				sql_i64_from_u64(now_unix_ms(), "pending_loop_snapshots.updated_at_unix_ms")?,
			],
		)?;
		Ok(())
	}

	pub fn load_snapshot(
		&self,
		session_id: &str,
	) -> Result<Option<(String, String)>, SqliteMemoryStoreError> {
		let connection = self.open()?;
		connection
			.query_row(
				"SELECT run_id, loop_state_json FROM pending_loop_snapshots WHERE session_id = ?1",
				params![session_id],
				|row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
			)
			.optional()
			.map_err(SqliteMemoryStoreError::from)
	}

	pub fn delete_snapshot(&self, session_id: &str) -> Result<(), SqliteMemoryStoreError> {
		let connection = self.open()?;
		connection.execute(
			"DELETE FROM pending_loop_snapshots WHERE session_id = ?1",
			params![session_id],
		)?;
		Ok(())
	}

	/// Clear the `pending_loop` field from the legacy `session_preferences` JSON blob.
	///
	/// Used during lazy migration to prevent stale legacy data from resurrecting
	/// after a delete on the dedicated table.
	pub fn clear_legacy_pending_loop(
		&self,
		session_id: &str,
	) -> Result<(), SqliteMemoryStoreError> {
		let connection = self.open()?;
		let encoded: Option<String> = connection
			.query_row(
				"SELECT preferences_json FROM session_preferences WHERE session_id = ?1",
				params![session_id],
				|row| row.get(0),
			)
			.optional()?;
		let Some(json) = encoded else {
			return Ok(());
		};
		let mut prefs: SessionPreferences =
			serde_json::from_str(&json).map_err(SqliteMemoryStoreError::from)?;
		if prefs.pending_loop.is_none() {
			return Ok(());
		}
		prefs.pending_loop = None;
		let updated = serde_json::to_string(&prefs).map_err(SqliteMemoryStoreError::from)?;
		connection.execute(
			"UPDATE session_preferences SET preferences_json = ?2, updated_at_unix_ms = ?3 WHERE session_id = ?1",
			params![
				session_id,
				updated,
				sql_i64_from_u64(now_unix_ms(), "session_preferences.updated_at_unix_ms")?,
			],
		)?;
		Ok(())
	}
}

#[derive(Debug, Clone)]
pub struct SqliteConversationRepository {
	config: SqliteMemoryStoreConfig,
}

impl SqliteConversationRepository {
	pub fn connect(config: SqliteMemoryStoreConfig) -> Result<Self, SqliteMemoryStoreError> {
		open_connection(&config.path)?;
		Ok(Self { config })
	}

	fn open(&self) -> Result<Connection, SqliteMemoryStoreError> {
		open_connection(&self.config.path)
	}

	pub fn append_turn(
		&mut self,
		session_id: &str,
		turn: ConversationTurn,
	) -> Result<(), SqliteMemoryStoreError> {
		let connection = self.open()?;
		connection.execute(
			"INSERT INTO conversation_turns (session_id, turn_json) VALUES (?1, ?2)",
			params![session_id, serde_json::to_string(&turn)?],
		)?;
		Ok(())
	}

	pub fn load_recent_turns(
		&self,
		session_id: &str,
		limit: usize,
	) -> Result<Vec<ConversationTurn>, SqliteMemoryStoreError> {
		let connection = self.open()?;
		let mut statement = connection.prepare(
			"SELECT turn_json FROM conversation_turns WHERE session_id = ?1 ORDER BY seq DESC LIMIT ?2",
		)?;
		let mut turns = statement
			.query_map(
				params![session_id, i64::try_from(limit).unwrap_or(i64::MAX)],
				|row| row.get::<_, String>(0),
			)?
			.map(|row| {
				let encoded = row?;
				serde_json::from_str(&encoded).map_err(SqliteMemoryStoreError::from)
			})
			.collect::<Result<Vec<_>, _>>()?;
		turns.reverse();
		Ok(turns)
	}

	pub fn delete_conversation(&mut self, session_id: &str) -> Result<(), SqliteMemoryStoreError> {
		let connection = self.open()?;
		connection.execute(
			"DELETE FROM conversation_turns WHERE session_id = ?1",
			params![session_id],
		)?;
		Ok(())
	}
}

#[derive(Debug, Clone)]
pub struct SqliteSessionCatalogRepository {
	config: SqliteMemoryStoreConfig,
}

impl SqliteSessionCatalogRepository {
	pub fn connect(config: SqliteMemoryStoreConfig) -> Result<Self, SqliteMemoryStoreError> {
		open_connection(&config.path)?;
		Ok(Self { config })
	}

	fn open(&self) -> Result<Connection, SqliteMemoryStoreError> {
		open_connection(&self.config.path)
	}

	pub fn insert_session(
		&mut self,
		binding_id: &str,
		descriptor: &SessionDescriptor,
	) -> Result<(), SqliteMemoryStoreError> {
		let connection = self.open()?;
		connection.execute(
			"INSERT INTO session_descriptors (
				binding_id,
				session_id,
				name,
				created_at_unix_ms,
				updated_at_unix_ms
			) VALUES (?1, ?2, ?3, ?4, ?5)",
			params![
				binding_id,
				descriptor.session_id,
				descriptor.name,
				descriptor.created_at_unix_ms,
				descriptor.updated_at_unix_ms,
			],
		)?;
		Ok(())
	}

	pub fn load_session(
		&self,
		binding_id: &str,
		session_id: &str,
	) -> Result<Option<SessionDescriptor>, SqliteMemoryStoreError> {
		let connection = self.open()?;
		connection
			.query_row(
				"SELECT session_id, name, created_at_unix_ms, updated_at_unix_ms
				 FROM session_descriptors
				 WHERE binding_id = ?1 AND session_id = ?2",
				params![binding_id, session_id],
				session_descriptor_from_row,
			)
			.optional()
			.map_err(SqliteMemoryStoreError::from)
	}

	pub fn list_sessions(
		&self,
		binding_id: &str,
	) -> Result<Vec<SessionSummary>, SqliteMemoryStoreError> {
		let connection = self.open()?;
		let mut statement = connection.prepare(
			"SELECT session_id, name, updated_at_unix_ms
			 FROM session_descriptors
			 WHERE binding_id = ?1",
		)?;
		statement
			.query_map(params![binding_id], |row| {
				Ok(SessionSummary {
					session_id: row.get(0)?,
					name: row.get(1)?,
					updated_at_unix_ms: row.get(2)?,
				})
			})?
			.collect::<Result<Vec<_>, _>>()
			.map_err(SqliteMemoryStoreError::from)
	}

	pub fn rename_session(
		&mut self,
		binding_id: &str,
		session_id: &str,
		name: &str,
		updated_at_unix_ms: i64,
	) -> Result<Option<SessionDescriptor>, SqliteMemoryStoreError> {
		let connection = self.open()?;
		let rows = connection.execute(
			"UPDATE session_descriptors
			 SET name = ?3, updated_at_unix_ms = ?4
			 WHERE binding_id = ?1 AND session_id = ?2",
			params![binding_id, session_id, name, updated_at_unix_ms],
		)?;
		if rows == 0 {
			return Ok(None);
		}
		self.load_session(binding_id, session_id)
	}

	pub fn load_active_session(
		&self,
		binding_id: &str,
	) -> Result<Option<SessionDescriptor>, SqliteMemoryStoreError> {
		let connection = self.open()?;
		connection
			.query_row(
				"SELECT d.session_id, d.name, d.created_at_unix_ms, d.updated_at_unix_ms
				 FROM active_session_bindings AS a
				 JOIN session_descriptors AS d
				   ON d.binding_id = a.binding_id AND d.session_id = a.session_id
				 WHERE a.binding_id = ?1",
				params![binding_id],
				session_descriptor_from_row,
			)
			.optional()
			.map_err(SqliteMemoryStoreError::from)
	}

	pub fn active_session_exists(&self, binding_id: &str) -> Result<bool, SqliteMemoryStoreError> {
		let connection = self.open()?;
		let exists = connection.query_row(
			"SELECT EXISTS(
					SELECT 1 FROM active_session_bindings WHERE binding_id = ?1
				)",
			params![binding_id],
			|row| row.get::<_, i64>(0),
		)? != 0;
		Ok(exists)
	}

	pub fn select_active_session(
		&mut self,
		binding_id: &str,
		session_id: &str,
		updated_at_unix_ms: i64,
	) -> Result<Option<SessionDescriptor>, SqliteMemoryStoreError> {
		let mut connection = self.open()?;
		let transaction = connection.transaction()?;
		let descriptor = transaction
			.query_row(
				"SELECT session_id, name, created_at_unix_ms, updated_at_unix_ms
				 FROM session_descriptors
				 WHERE binding_id = ?1 AND session_id = ?2",
				params![binding_id, session_id],
				session_descriptor_from_row,
			)
			.optional()?;
		let Some(mut descriptor) = descriptor else {
			return Ok(None);
		};
		transaction.execute(
			"UPDATE session_descriptors
			 SET updated_at_unix_ms = ?3
			 WHERE binding_id = ?1 AND session_id = ?2",
			params![binding_id, session_id, updated_at_unix_ms],
		)?;
		transaction.execute(
			"INSERT INTO active_session_bindings (binding_id, session_id, updated_at_unix_ms)
			 VALUES (?1, ?2, ?3)
			 ON CONFLICT(binding_id) DO UPDATE
			 SET session_id = excluded.session_id, updated_at_unix_ms = excluded.updated_at_unix_ms",
			params![binding_id, session_id, updated_at_unix_ms],
		)?;
		transaction.commit()?;
		descriptor.updated_at_unix_ms = updated_at_unix_ms;
		Ok(Some(descriptor))
	}

	pub fn delete_session(
		&mut self,
		binding_id: &str,
		session_id: &str,
	) -> Result<bool, SqliteMemoryStoreError> {
		let mut connection = self.open()?;
		let transaction = connection.transaction()?;
		let exists = transaction.query_row(
			"SELECT EXISTS(
					SELECT 1
					FROM session_descriptors
					WHERE binding_id = ?1 AND session_id = ?2
				)",
			params![binding_id, session_id],
			|row| row.get::<_, i64>(0),
		)? != 0;
		if !exists {
			return Ok(false);
		}
		transaction.execute(
			"DELETE FROM active_session_bindings
			 WHERE binding_id = ?1 AND session_id = ?2",
			params![binding_id, session_id],
		)?;
		transaction.execute(
			"DELETE FROM session_preferences WHERE session_id = ?1",
			params![session_id],
		)?;
		transaction.execute(
			"DELETE FROM pending_loop_snapshots WHERE session_id = ?1",
			params![session_id],
		)?;
		transaction.execute(
			"DELETE FROM conversation_turns WHERE session_id = ?1",
			params![session_id],
		)?;
		transaction.execute(
			"DELETE FROM session_descriptors
			 WHERE binding_id = ?1 AND session_id = ?2",
			params![binding_id, session_id],
		)?;
		transaction.commit()?;
		Ok(true)
	}
}

fn open_connection(path: &Path) -> Result<Connection, SqliteMemoryStoreError> {
	ensure_parent_dir(path)?;
	let connection = Connection::open(path)?;
	connection.busy_timeout(Duration::from_millis(SQLITE_BUSY_TIMEOUT_MS))?;
	connection.execute_batch(&format!(
		"PRAGMA journal_mode = WAL;
		 PRAGMA synchronous = NORMAL;
		 PRAGMA foreign_keys = ON;
		 PRAGMA busy_timeout = {SQLITE_BUSY_TIMEOUT_MS};
		 PRAGMA temp_store = MEMORY;
		 PRAGMA wal_autocheckpoint = {SQLITE_WAL_AUTOCHECKPOINT_PAGES};
		 PRAGMA auto_vacuum = INCREMENTAL;"
	))?;
	connection.pragma_update(None, "application_id", SQLITE_APPLICATION_ID)?;
	connection.pragma_update(None, "user_version", SQLITE_USER_VERSION)?;
	ensure_schema_objects(&connection)?;
	connection.execute_batch("PRAGMA optimize;")?;
	Ok(connection)
}

fn ensure_schema_objects(connection: &Connection) -> Result<(), SqliteMemoryStoreError> {
	connection.execute_batch(
		"CREATE TABLE IF NOT EXISTS session_preferences (
			session_id TEXT PRIMARY KEY,
			preferences_json TEXT NOT NULL,
			updated_at_unix_ms INTEGER NOT NULL DEFAULT 0
		);
		CREATE TABLE IF NOT EXISTS conversation_turns (
			seq INTEGER PRIMARY KEY AUTOINCREMENT,
			session_id TEXT NOT NULL,
			turn_json TEXT NOT NULL
		);
		CREATE TABLE IF NOT EXISTS session_descriptors (
			binding_id TEXT NOT NULL,
			session_id TEXT NOT NULL,
			name TEXT NOT NULL,
			created_at_unix_ms INTEGER NOT NULL,
			updated_at_unix_ms INTEGER NOT NULL,
			PRIMARY KEY(binding_id, session_id)
		);
		CREATE TABLE IF NOT EXISTS active_session_bindings (
			binding_id TEXT PRIMARY KEY,
			session_id TEXT NOT NULL,
			updated_at_unix_ms INTEGER NOT NULL DEFAULT 0
		);
		CREATE TABLE IF NOT EXISTS pending_loop_snapshots (
			session_id TEXT PRIMARY KEY,
			run_id TEXT NOT NULL,
			loop_state_json TEXT NOT NULL,
			updated_at_unix_ms INTEGER NOT NULL DEFAULT 0
		);
		CREATE INDEX IF NOT EXISTS idx_conversation_turns_session_seq
			ON conversation_turns(session_id, seq);
		CREATE INDEX IF NOT EXISTS idx_session_descriptors_binding_updated
			ON session_descriptors(binding_id, updated_at_unix_ms DESC, session_id);",
	)?;
	Ok(())
}

fn session_descriptor_from_row(
	row: &rusqlite::Row<'_>,
) -> Result<SessionDescriptor, rusqlite::Error> {
	Ok(SessionDescriptor {
		session_id: row.get(0)?,
		name: row.get(1)?,
		created_at_unix_ms: row.get(2)?,
		updated_at_unix_ms: row.get(3)?,
	})
}

fn ensure_parent_dir(path: &Path) -> Result<(), SqliteMemoryStoreError> {
	if let Some(parent) = path.parent() {
		fs::create_dir_all(parent)?;
	}
	Ok(())
}

fn sql_i64_from_u64(value: u64, field: &str) -> Result<i64, SqliteMemoryStoreError> {
	i64::try_from(value).map_err(|_| {
		SqliteMemoryStoreError::Storage(format!(
			"{field} value {value} exceeds SQLite INTEGER range"
		))
	})
}

fn now_unix_ms() -> u64 {
	std::time::SystemTime::now()
		.duration_since(std::time::UNIX_EPOCH)
		.unwrap_or_default()
		.as_millis()
		.try_into()
		.unwrap_or(u64::MAX)
}
