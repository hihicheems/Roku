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
use rusqlite::{Connection, OptionalExtension, params};
use thiserror::Error;

const SQLITE_BUSY_TIMEOUT_MS: u64 = 5_000;
const SQLITE_WAL_AUTOCHECKPOINT_PAGES: i64 = 200;
const SQLITE_APPLICATION_ID: i64 = 0x524f_4b55;
const SQLITE_USER_VERSION: i64 = 1;

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
		CREATE INDEX IF NOT EXISTS idx_conversation_turns_session_seq
			ON conversation_turns(session_id, seq);",
	)?;
	Ok(())
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
