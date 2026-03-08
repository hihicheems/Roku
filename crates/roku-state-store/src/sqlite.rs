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
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use roku_common_types::{
	ApprovalId, ApprovalTicket, ConversationTurn, NodeId, ResultEnvelope, SessionPreferences, Task,
	TaskEvent, TaskId,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::{
	ApprovalRepository, BackpressureSnapshot, ConversationRepository, DispatchClaim,
	DispatchEnvelope, DispatchLease, DispatchQueue, EventRepository, ResultRepository, RetryClaim,
	SessionPreferenceRepository, StoreError, TaskRepository,
};

const SQLITE_BUSY_TIMEOUT_MS: u64 = 5_000;
const SQLITE_WAL_AUTOCHECKPOINT_PAGES: i64 = 200;
const SQLITE_APPLICATION_ID: i64 = 0x524f_4b55;
const SQLITE_USER_VERSION: i64 = 1;
const DEFAULT_LEASE_DURATION_MS: u64 = 30_000;
const DEFAULT_MAX_IN_FLIGHT: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqliteStoreConfig {
	pub path: PathBuf,
}

impl SqliteStoreConfig {
	pub fn new(path: impl Into<PathBuf>) -> Self {
		Self { path: path.into() }
	}
}

#[derive(Debug, Clone)]
pub struct SqliteTaskRepository {
	config: SqliteStoreConfig,
}

impl SqliteTaskRepository {
	pub fn connect(config: SqliteStoreConfig) -> Result<Self, StoreError> {
		open_connection(&config.path)?;
		Ok(Self { config })
	}

	fn open(&self) -> Result<Connection, StoreError> {
		open_connection(&self.config.path)
	}
}

impl TaskRepository for SqliteTaskRepository {
	fn save_task(&mut self, task: Task) -> Result<(), StoreError> {
		let connection = self.open()?;
		connection.execute(
			"INSERT INTO tasks (task_id, task_json, updated_at_unix_ms) VALUES (?1, ?2, ?3)
			 ON CONFLICT(task_id) DO UPDATE SET task_json = excluded.task_json, updated_at_unix_ms = excluded.updated_at_unix_ms",
			params![task.task_id.0, serde_json::to_string(&task)?, now_unix_ms()],
		)?;
		Ok(())
	}

	fn load_task(&self, task_id: &TaskId) -> Result<Option<Task>, StoreError> {
		let connection = self.open()?;
		let encoded = connection
			.query_row(
				"SELECT task_json FROM tasks WHERE task_id = ?1",
				params![task_id.0],
				|row| row.get::<_, String>(0),
			)
			.optional()?;
		encoded
			.map(|value| serde_json::from_str(&value).map_err(StoreError::from))
			.transpose()
	}
}

#[derive(Debug, Clone)]
pub struct SqliteEventRepository {
	config: SqliteStoreConfig,
}

impl SqliteEventRepository {
	pub fn connect(config: SqliteStoreConfig) -> Result<Self, StoreError> {
		open_connection(&config.path)?;
		Ok(Self { config })
	}

	fn open(&self) -> Result<Connection, StoreError> {
		open_connection(&self.config.path)
	}
}

impl EventRepository for SqliteEventRepository {
	fn append_event(&mut self, event: TaskEvent) -> Result<(), StoreError> {
		let connection = self.open()?;
		connection.execute(
			"INSERT INTO task_events (task_id, event_json) VALUES (?1, ?2)",
			params![event.task_id.0, serde_json::to_string(&event)?],
		)?;
		Ok(())
	}

	fn list_events(&self, task_id: &TaskId) -> Result<Vec<TaskEvent>, StoreError> {
		let connection = self.open()?;
		let mut statement = connection
			.prepare("SELECT event_json FROM task_events WHERE task_id = ?1 ORDER BY seq ASC")?;
		statement
			.query_map(params![task_id.0], |row| row.get::<_, String>(0))?
			.map(|row| {
				let encoded = row?;
				serde_json::from_str(&encoded).map_err(StoreError::from)
			})
			.collect()
	}
}

#[derive(Debug, Clone)]
pub struct SqliteApprovalRepository {
	config: SqliteStoreConfig,
}

impl SqliteApprovalRepository {
	pub fn connect(config: SqliteStoreConfig) -> Result<Self, StoreError> {
		open_connection(&config.path)?;
		Ok(Self { config })
	}

	fn open(&self) -> Result<Connection, StoreError> {
		open_connection(&self.config.path)
	}
}

impl ApprovalRepository for SqliteApprovalRepository {
	fn save_ticket(&mut self, ticket: ApprovalTicket) -> Result<(), StoreError> {
		let connection = self.open()?;
		connection.execute(
			"INSERT INTO approval_tickets (approval_id, task_id, ticket_json, updated_at_unix_ms) VALUES (?1, ?2, ?3, ?4)
			 ON CONFLICT(approval_id) DO UPDATE SET task_id = excluded.task_id, ticket_json = excluded.ticket_json, updated_at_unix_ms = excluded.updated_at_unix_ms",
			params![
				ticket.approval_id.0,
				ticket.task_id.0,
				serde_json::to_string(&ticket)?,
				now_unix_ms(),
			],
		)?;
		Ok(())
	}

	fn load_ticket(&self, approval_id: &ApprovalId) -> Result<Option<ApprovalTicket>, StoreError> {
		let connection = self.open()?;
		let encoded = connection
			.query_row(
				"SELECT ticket_json FROM approval_tickets WHERE approval_id = ?1",
				params![approval_id.0],
				|row| row.get::<_, String>(0),
			)
			.optional()?;
		encoded
			.map(|value| serde_json::from_str(&value).map_err(StoreError::from))
			.transpose()
	}

	fn list_tickets_for_task(&self, task_id: &TaskId) -> Result<Vec<ApprovalTicket>, StoreError> {
		let connection = self.open()?;
		let mut statement = connection.prepare(
			"SELECT ticket_json FROM approval_tickets WHERE task_id = ?1 ORDER BY approval_id ASC",
		)?;
		statement
			.query_map(params![task_id.0], |row| row.get::<_, String>(0))?
			.map(|row| {
				let encoded = row?;
				serde_json::from_str(&encoded).map_err(StoreError::from)
			})
			.collect()
	}
}

#[derive(Debug, Clone)]
pub struct SqliteResultRepository {
	config: SqliteStoreConfig,
}

impl SqliteResultRepository {
	pub fn connect(config: SqliteStoreConfig) -> Result<Self, StoreError> {
		open_connection(&config.path)?;
		Ok(Self { config })
	}

	fn open(&self) -> Result<Connection, StoreError> {
		open_connection(&self.config.path)
	}
}

impl ResultRepository for SqliteResultRepository {
	fn save_result(&mut self, result: ResultEnvelope) -> Result<(), StoreError> {
		let connection = self.open()?;
		connection.execute(
			"INSERT INTO node_results (task_id, node_id, result_json, updated_at_unix_ms) VALUES (?1, ?2, ?3, ?4)
			 ON CONFLICT(task_id, node_id) DO UPDATE SET result_json = excluded.result_json, updated_at_unix_ms = excluded.updated_at_unix_ms",
			params![
				result.task_id.0,
				result.node_id.0,
				serde_json::to_string(&result)?,
				now_unix_ms(),
			],
		)?;
		Ok(())
	}

	fn load_result(
		&self,
		task_id: &TaskId,
		node_id: &NodeId,
	) -> Result<Option<ResultEnvelope>, StoreError> {
		let connection = self.open()?;
		let encoded = connection
			.query_row(
				"SELECT result_json FROM node_results WHERE task_id = ?1 AND node_id = ?2",
				params![task_id.0, node_id.0],
				|row| row.get::<_, String>(0),
			)
			.optional()?;
		encoded
			.map(|value| serde_json::from_str(&value).map_err(StoreError::from))
			.transpose()
	}

	fn list_results(&self, task_id: &TaskId) -> Result<Vec<ResultEnvelope>, StoreError> {
		let connection = self.open()?;
		let mut statement = connection.prepare(
			"SELECT result_json FROM node_results WHERE task_id = ?1 ORDER BY node_id ASC",
		)?;
		statement
			.query_map(params![task_id.0], |row| row.get::<_, String>(0))?
			.map(|row| {
				let encoded = row?;
				serde_json::from_str(&encoded).map_err(StoreError::from)
			})
			.collect()
	}
}

#[derive(Debug, Clone)]
pub struct SqliteSessionPreferenceRepository {
	config: SqliteStoreConfig,
}

impl SqliteSessionPreferenceRepository {
	pub fn connect(config: SqliteStoreConfig) -> Result<Self, StoreError> {
		open_connection(&config.path)?;
		Ok(Self { config })
	}

	fn open(&self) -> Result<Connection, StoreError> {
		open_connection(&self.config.path)
	}
}

impl SessionPreferenceRepository for SqliteSessionPreferenceRepository {
	fn save_preferences(
		&mut self,
		session_id: &str,
		preferences: SessionPreferences,
	) -> Result<(), StoreError> {
		let connection = self.open()?;
		connection.execute(
			"INSERT INTO session_preferences (session_id, preferences_json, updated_at_unix_ms) VALUES (?1, ?2, ?3)
			 ON CONFLICT(session_id) DO UPDATE SET preferences_json = excluded.preferences_json, updated_at_unix_ms = excluded.updated_at_unix_ms",
			params![session_id, serde_json::to_string(&preferences)?, now_unix_ms()],
		)?;
		Ok(())
	}

	fn load_preferences(&self, session_id: &str) -> Result<Option<SessionPreferences>, StoreError> {
		let connection = self.open()?;
		let encoded = connection
			.query_row(
				"SELECT preferences_json FROM session_preferences WHERE session_id = ?1",
				params![session_id],
				|row| row.get::<_, String>(0),
			)
			.optional()?;
		encoded
			.map(|value| serde_json::from_str(&value).map_err(StoreError::from))
			.transpose()
	}
}

#[derive(Debug, Clone)]
pub struct SqliteConversationRepository {
	config: SqliteStoreConfig,
}

impl SqliteConversationRepository {
	pub fn connect(config: SqliteStoreConfig) -> Result<Self, StoreError> {
		open_connection(&config.path)?;
		Ok(Self { config })
	}

	fn open(&self) -> Result<Connection, StoreError> {
		open_connection(&self.config.path)
	}
}

impl ConversationRepository for SqliteConversationRepository {
	fn append_turn(&mut self, session_id: &str, turn: ConversationTurn) -> Result<(), StoreError> {
		let connection = self.open()?;
		connection.execute(
			"INSERT INTO conversation_turns (session_id, turn_json) VALUES (?1, ?2)",
			params![session_id, serde_json::to_string(&turn)?],
		)?;
		Ok(())
	}

	fn load_recent_turns(
		&self,
		session_id: &str,
		limit: usize,
	) -> Result<Vec<ConversationTurn>, StoreError> {
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
				serde_json::from_str(&encoded).map_err(StoreError::from)
			})
			.collect::<Result<Vec<_>, _>>()?;
		turns.reverse();
		Ok(turns)
	}
}

#[derive(Debug, Clone)]
pub struct SqliteDispatchQueue {
	config: SqliteStoreConfig,
	lease_duration_ms: u64,
	max_in_flight: usize,
}

impl SqliteDispatchQueue {
	pub fn connect(config: SqliteStoreConfig) -> Result<Self, StoreError> {
		Self::with_limits(config, DEFAULT_MAX_IN_FLIGHT, DEFAULT_LEASE_DURATION_MS)
	}

	pub fn with_limits(
		config: SqliteStoreConfig,
		max_in_flight: usize,
		lease_duration_ms: u64,
	) -> Result<Self, StoreError> {
		open_connection(&config.path)?;
		Ok(Self {
			config,
			lease_duration_ms,
			max_in_flight,
		})
	}

	fn open(&self) -> Result<Connection, StoreError> {
		open_connection(&self.config.path)
	}

	fn verify_lease(
		transaction: &rusqlite::Transaction<'_>,
		lease: &DispatchLease,
	) -> Result<(), StoreError> {
		let stored = transaction
			.query_row(
				"SELECT consumer_id, lease_token, expires_at_unix_ms FROM dispatch_entries WHERE entry_id = ?1 AND state = 'leased'",
				params![lease.entry_id],
				|row| {
					Ok((
						row.get::<_, Option<String>>(0)?,
						row.get::<_, Option<String>>(1)?,
						row.get::<_, Option<u64>>(2)?,
					))
				},
			)
			.optional()?;
		let Some((consumer_id, lease_token, expires_at_unix_ms)) = stored else {
			return Err(StoreError::Storage(format!(
				"dispatch lease not found for {}",
				lease.entry_id
			)));
		};
		if consumer_id.as_deref() != Some(lease.consumer_id.as_str())
			|| lease_token.as_deref() != Some(lease.lease_token.as_str())
			|| expires_at_unix_ms != Some(lease.expires_at_unix_ms)
		{
			return Err(StoreError::Storage(format!(
				"dispatch lease mismatch for {}",
				lease.entry_id
			)));
		}
		Ok(())
	}
}

impl DispatchQueue for SqliteDispatchQueue {
	fn publish(&mut self, envelope: DispatchEnvelope) -> Result<(), StoreError> {
		let connection = self.open()?;
		connection.execute(
			"INSERT INTO dispatch_entries (
				entry_id, task_id, node_id, attempt, payload, state, created_at_unix_ms, updated_at_unix_ms
			 ) VALUES (?1, ?2, ?3, ?4, ?5, 'queued', ?6, ?6)
			 ON CONFLICT(entry_id) DO UPDATE SET
				task_id = excluded.task_id,
				node_id = excluded.node_id,
				attempt = excluded.attempt,
				payload = excluded.payload,
				state = 'queued',
				consumer_id = NULL,
				lease_token = NULL,
				expires_at_unix_ms = NULL,
				updated_at_unix_ms = excluded.updated_at_unix_ms",
			params![
				envelope.entry_id,
				envelope.task_id.0,
				envelope.node_id.0,
				envelope.attempt,
				envelope.payload,
				now_unix_ms(),
			],
		)?;
		Ok(())
	}

	fn claim(
		&mut self,
		consumer_id: &str,
		now_unix_ms: u64,
	) -> Result<Option<DispatchClaim>, StoreError> {
		let mut connection = self.open()?;
		let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
		requeue_expired_entries(&transaction, now_unix_ms)?;
		let leased_count: usize = transaction.query_row(
			"SELECT COUNT(*) FROM dispatch_entries WHERE state = 'leased'",
			[],
			|row| row.get(0),
		)?;
		if leased_count >= self.max_in_flight {
			transaction.commit()?;
			return Ok(None);
		}

		let envelope = transaction
			.query_row(
				"SELECT entry_id, task_id, node_id, attempt, payload
				 FROM dispatch_entries
				 WHERE state = 'queued'
				 ORDER BY created_at_unix_ms ASC, entry_id ASC
				 LIMIT 1",
				[],
				|row| {
					Ok(DispatchEnvelope {
						entry_id: row.get(0)?,
						task_id: TaskId(row.get(1)?),
						node_id: NodeId(row.get(2)?),
						attempt: row.get(3)?,
						payload: row.get(4)?,
					})
				},
			)
			.optional()?;
		let Some(envelope) = envelope else {
			transaction.commit()?;
			return Ok(None);
		};

		let lease = DispatchLease {
			entry_id: envelope.entry_id.clone(),
			consumer_id: consumer_id.to_string(),
			lease_token: new_lease_token(&envelope.entry_id, consumer_id),
			expires_at_unix_ms: now_unix_ms.saturating_add(self.lease_duration_ms),
		};
		let updated = transaction.execute(
			"UPDATE dispatch_entries
			 SET state = 'leased', consumer_id = ?1, lease_token = ?2, expires_at_unix_ms = ?3, updated_at_unix_ms = ?4
			 WHERE entry_id = ?5 AND state = 'queued'",
			params![
				lease.consumer_id,
				lease.lease_token,
				lease.expires_at_unix_ms,
				now_unix_ms,
				lease.entry_id,
			],
		)?;
		transaction.commit()?;
		if updated == 0 {
			return Ok(None);
		}

		Ok(Some(DispatchClaim { envelope, lease }))
	}

	fn ack(&mut self, lease: &DispatchLease) -> Result<(), StoreError> {
		let mut connection = self.open()?;
		let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
		Self::verify_lease(&transaction, lease)?;
		transaction.execute(
			"DELETE FROM dispatch_entries WHERE entry_id = ?1 AND state = 'leased'",
			params![lease.entry_id],
		)?;
		transaction.commit()?;
		Ok(())
	}

	fn nack(&mut self, lease: &DispatchLease, retry: RetryClaim) -> Result<(), StoreError> {
		let mut connection = self.open()?;
		let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
		Self::verify_lease(&transaction, lease)?;
		transaction.execute(
			"UPDATE dispatch_entries
			 SET attempt = ?1,
				 state = 'queued',
				 consumer_id = NULL,
				 lease_token = NULL,
				 expires_at_unix_ms = NULL,
				 updated_at_unix_ms = ?2
			 WHERE entry_id = ?3 AND state = 'leased'",
			params![retry.next_attempt, now_unix_ms(), lease.entry_id],
		)?;
		transaction.commit()?;
		Ok(())
	}

	fn renew_lease(
		&mut self,
		lease: &DispatchLease,
		now_unix_ms: u64,
	) -> Result<Option<DispatchLease>, StoreError> {
		let mut connection = self.open()?;
		let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
		requeue_expired_entries(&transaction, now_unix_ms)?;
		Self::verify_lease(&transaction, lease)?;
		let renewed = DispatchLease {
			entry_id: lease.entry_id.clone(),
			consumer_id: lease.consumer_id.clone(),
			lease_token: new_lease_token(&lease.entry_id, &lease.consumer_id),
			expires_at_unix_ms: now_unix_ms.saturating_add(self.lease_duration_ms),
		};
		transaction.execute(
			"UPDATE dispatch_entries
			 SET lease_token = ?1, expires_at_unix_ms = ?2, updated_at_unix_ms = ?3
			 WHERE entry_id = ?4 AND state = 'leased'",
			params![
				renewed.lease_token,
				renewed.expires_at_unix_ms,
				now_unix_ms,
				renewed.entry_id,
			],
		)?;
		transaction.commit()?;
		Ok(Some(renewed))
	}

	fn backpressure(&self) -> BackpressureSnapshot {
		let Ok(connection) = self.open() else {
			return BackpressureSnapshot {
				queued: 0,
				leased: 0,
				max_in_flight: self.max_in_flight,
				available_slots: self.max_in_flight,
			};
		};
		let queued = connection
			.query_row(
				"SELECT COUNT(*) FROM dispatch_entries WHERE state = 'queued'",
				[],
				|row| row.get::<_, usize>(0),
			)
			.unwrap_or(0);
		let leased = connection
			.query_row(
				"SELECT COUNT(*) FROM dispatch_entries WHERE state = 'leased'",
				[],
				|row| row.get::<_, usize>(0),
			)
			.unwrap_or(0);
		BackpressureSnapshot {
			queued,
			leased,
			max_in_flight: self.max_in_flight,
			available_slots: self.max_in_flight.saturating_sub(leased),
		}
	}
}

fn open_connection(path: &Path) -> Result<Connection, StoreError> {
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

fn ensure_schema_objects(connection: &Connection) -> Result<(), StoreError> {
	connection.execute_batch(
		"CREATE TABLE IF NOT EXISTS tasks (
			task_id TEXT PRIMARY KEY,
			task_json TEXT NOT NULL,
			updated_at_unix_ms INTEGER NOT NULL DEFAULT 0
		);
		CREATE TABLE IF NOT EXISTS task_events (
			seq INTEGER PRIMARY KEY AUTOINCREMENT,
			task_id TEXT NOT NULL,
			event_json TEXT NOT NULL
		);
		CREATE INDEX IF NOT EXISTS idx_task_events_task_id_seq
			ON task_events(task_id, seq);
		CREATE TABLE IF NOT EXISTS approval_tickets (
			approval_id TEXT PRIMARY KEY,
			task_id TEXT NOT NULL,
			ticket_json TEXT NOT NULL,
			updated_at_unix_ms INTEGER NOT NULL DEFAULT 0
		);
		CREATE INDEX IF NOT EXISTS idx_approval_tickets_task_id
			ON approval_tickets(task_id);
		CREATE TABLE IF NOT EXISTS node_results (
			task_id TEXT NOT NULL,
			node_id TEXT NOT NULL,
			result_json TEXT NOT NULL,
			updated_at_unix_ms INTEGER NOT NULL DEFAULT 0,
			PRIMARY KEY (task_id, node_id)
		);
		CREATE TABLE IF NOT EXISTS session_preferences (
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
			ON conversation_turns(session_id, seq);
		CREATE TABLE IF NOT EXISTS dispatch_entries (
			entry_id TEXT PRIMARY KEY,
			task_id TEXT NOT NULL,
			node_id TEXT NOT NULL,
			attempt INTEGER NOT NULL,
			payload TEXT NOT NULL,
			state TEXT NOT NULL,
			consumer_id TEXT,
			lease_token TEXT,
			expires_at_unix_ms INTEGER,
			created_at_unix_ms INTEGER NOT NULL DEFAULT 0,
			updated_at_unix_ms INTEGER NOT NULL DEFAULT 0
		);
		CREATE INDEX IF NOT EXISTS idx_dispatch_entries_state_created
			ON dispatch_entries(state, created_at_unix_ms, entry_id);
		CREATE INDEX IF NOT EXISTS idx_dispatch_entries_expires_at
			ON dispatch_entries(state, expires_at_unix_ms);",
	)?;
	Ok(())
}

fn requeue_expired_entries(
	transaction: &rusqlite::Transaction<'_>,
	now_unix_ms: u64,
) -> Result<(), StoreError> {
	transaction.execute(
		"UPDATE dispatch_entries
		 SET state = 'queued',
			 consumer_id = NULL,
			 lease_token = NULL,
			 expires_at_unix_ms = NULL,
			 updated_at_unix_ms = ?1
		 WHERE state = 'leased' AND expires_at_unix_ms IS NOT NULL AND expires_at_unix_ms <= ?2",
		params![now_unix_ms, now_unix_ms],
	)?;
	Ok(())
}

fn ensure_parent_dir(path: &Path) -> Result<(), StoreError> {
	if let Some(parent) = path.parent() {
		fs::create_dir_all(parent)?;
	}
	Ok(())
}

fn new_lease_token(entry_id: &str, consumer_id: &str) -> String {
	let now = SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.unwrap_or_default()
		.as_nanos();
	format!(
		"lease-{entry_id}-{consumer_id}-{}-{now}",
		std::process::id()
	)
}

fn now_unix_ms() -> u64 {
	SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.unwrap_or_default()
		.as_millis()
		.try_into()
		.unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
	use super::*;
	use roku_common_types::{
		ApprovalStatus, ConversationRole, EvidenceItem, PlanningModeHint, RequestId, ResultStatus,
		TaskState,
	};

	fn unique_path(suffix: &str) -> PathBuf {
		let nanos = SystemTime::now()
			.duration_since(UNIX_EPOCH)
			.expect("clock should be after epoch")
			.as_nanos();
		std::env::temp_dir().join(format!("roku-store-sqlite-{suffix}-{nanos}.db"))
	}

	fn sample_task() -> Task {
		Task {
			task_id: TaskId("task-1".to_string()),
			request_id: RequestId("req-1".to_string()),
			session_id: "session-1".to_string(),
			goal: "analyze market".to_string(),
			state: TaskState::Queued,
			attempts: 0,
			planning_mode_hint: None,
			conversation_history: Vec::new(),
			completed_nodes: Vec::new(),
			next_node_index: 0,
			pending_approval_id: None,
			last_result: None,
			compensation_records: Vec::new(),
			graph: None,
		}
	}

	fn sample_event() -> TaskEvent {
		TaskEvent {
			task_id: TaskId("task-1".to_string()),
			from: TaskState::Queued,
			to: TaskState::Planning,
			reason: "start".to_string(),
			error_class: None,
			kind: roku_common_types::TaskEventKind::StateTransition,
			node_id: None,
			node_kind: None,
			attempt: Some(0),
		}
	}

	fn sample_ticket() -> ApprovalTicket {
		ApprovalTicket {
			approval_id: ApprovalId("approval-1".to_string()),
			task_id: TaskId("task-1".to_string()),
			request_id: RequestId("req-1".to_string()),
			node_id: NodeId("node-1".to_string()),
			summary: "review risky action".to_string(),
			status: ApprovalStatus::Pending,
			decided_by: None,
			comment: None,
		}
	}

	fn sample_result() -> ResultEnvelope {
		ResultEnvelope {
			task_id: TaskId("task-1".to_string()),
			node_id: NodeId("node-1".to_string()),
			producer: "agent-1".to_string(),
			schema_version: "result.v1".to_string(),
			status: ResultStatus::Ok,
			payload: "payload".to_string(),
			evidence: vec![EvidenceItem {
				kind: "artifact_ref".to_string(),
				value: "artifact://1".to_string(),
			}],
			confidence: 0.9,
		}
	}

	#[test]
	fn sqlite_repositories_roundtrip() {
		let path = unique_path("repos");
		let config = SqliteStoreConfig::new(path.clone());
		let mut task_repo = SqliteTaskRepository::connect(config.clone()).expect("task repo");
		let mut event_repo = SqliteEventRepository::connect(config.clone()).expect("event repo");
		let mut approval_repo =
			SqliteApprovalRepository::connect(config.clone()).expect("approval repo");
		let mut result_repo = SqliteResultRepository::connect(config.clone()).expect("result repo");
		let mut session_repo =
			SqliteSessionPreferenceRepository::connect(config.clone()).expect("session repo");
		let mut conversation_repo =
			SqliteConversationRepository::connect(config).expect("conversation repo");

		task_repo.save_task(sample_task()).expect("save task");
		event_repo.append_event(sample_event()).expect("save event");
		approval_repo
			.save_ticket(sample_ticket())
			.expect("save ticket");
		result_repo
			.save_result(sample_result())
			.expect("save result");
		session_repo
			.save_preferences(
				"session-1",
				SessionPreferences {
					planning_mode: Some(PlanningModeHint::TreeSearch),
				},
			)
			.expect("save preferences");
		conversation_repo
			.append_turn(
				"session-1",
				ConversationTurn {
					role: ConversationRole::User,
					content: "hello".to_string(),
					created_at_unix_ms: 1,
				},
			)
			.expect("save turn");

		assert!(
			task_repo
				.load_task(&TaskId("task-1".to_string()))
				.expect("load task")
				.is_some()
		);
		assert_eq!(
			event_repo
				.list_events(&TaskId("task-1".to_string()))
				.expect("load events")
				.len(),
			1
		);
		assert!(
			approval_repo
				.load_ticket(&ApprovalId("approval-1".to_string()))
				.expect("load ticket")
				.is_some()
		);
		assert_eq!(
			approval_repo
				.list_tickets_for_task(&TaskId("task-1".to_string()))
				.expect("list tickets")
				.len(),
			1
		);
		assert!(
			result_repo
				.load_result(&TaskId("task-1".to_string()), &NodeId("node-1".to_string()))
				.expect("load result")
				.is_some()
		);
		assert_eq!(
			session_repo
				.load_preferences("session-1")
				.expect("load preferences")
				.and_then(|preferences| preferences.planning_mode),
			Some(PlanningModeHint::TreeSearch)
		);
		assert_eq!(
			conversation_repo
				.load_recent_turns("session-1", 8)
				.expect("load turns")
				.len(),
			1
		);

		let _ = fs::remove_file(path);
	}

	#[test]
	fn sqlite_dispatch_queue_requeues_expired_leases() {
		let path = unique_path("dispatch");
		let config = SqliteStoreConfig::new(path.clone());
		let mut queue =
			SqliteDispatchQueue::with_limits(config, 1, 10).expect("dispatch queue should open");
		queue
			.publish(DispatchEnvelope {
				entry_id: "entry-1".to_string(),
				task_id: TaskId("task-1".to_string()),
				node_id: NodeId("node-1".to_string()),
				attempt: 1,
				payload: "payload-entry-1".to_string(),
			})
			.expect("publish should succeed");

		let claim = queue
			.claim("worker-a", 100)
			.expect("claim should succeed")
			.expect("claim should exist");
		let reclaimed = queue
			.claim("worker-b", 111)
			.expect("second claim should succeed")
			.expect("expired lease should requeue the entry");

		assert_eq!(claim.envelope.entry_id, reclaimed.envelope.entry_id);
		assert_eq!(reclaimed.lease.consumer_id, "worker-b");

		let _ = fs::remove_file(path);
	}
}
