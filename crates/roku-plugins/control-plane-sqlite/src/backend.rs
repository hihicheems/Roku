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
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use roku_common_types::{
	ApprovalId, ApprovalTicket, NodeId, ResultEnvelope, Task, TaskEvent, TaskId, TaskReplaySnapshot,
};
use roku_control_plane::{
	ApprovalRepository, BackpressureSnapshot, ControlPlaneDataPlane, ControlPlaneError,
	DispatchClaim, DispatchEnvelope, DispatchLease, DispatchQueue, EventRepository,
	ResultRepository, RetryClaim, TaskRepository,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use thiserror::Error;

use crate::SqliteControlPlaneConfig;

const SQLITE_BUSY_TIMEOUT_MS: u64 = 5_000;
const SQLITE_WAL_AUTOCHECKPOINT_PAGES: i64 = 200;
const SQLITE_APPLICATION_ID: i64 = 0x524f_4b55;
const SQLITE_USER_VERSION: i64 = 1;
const DEFAULT_LEASE_DURATION_MS: u64 = 30_000;
const DEFAULT_MAX_IN_FLIGHT: usize = 32;

#[derive(Debug, Error)]
pub enum SqliteControlPlaneError {
	#[error("io error: {0}")]
	Io(#[from] std::io::Error),
	#[error("serialization error: {0}")]
	Serde(#[from] serde_json::Error),
	#[error("sqlite error: {0}")]
	Sqlite(#[from] rusqlite::Error),
	#[error("control-plane storage error: {0}")]
	Storage(String),
}

#[derive(Debug, Clone)]
pub struct SqliteTaskRepository {
	config: SqliteControlPlaneConfig,
}

impl SqliteTaskRepository {
	pub fn connect(config: SqliteControlPlaneConfig) -> Result<Self, SqliteControlPlaneError> {
		open_connection(&config.path)?;
		Ok(Self { config })
	}

	fn open(&self) -> Result<Connection, SqliteControlPlaneError> {
		open_connection(&self.config.path)
	}
}

impl TaskRepository for SqliteTaskRepository {
	fn save_task(&mut self, task: Task) -> Result<(), ControlPlaneError> {
		let connection = self.open().map_err(map_error)?;
		connection
			.execute(
				"INSERT INTO tasks (task_id, task_json, updated_at_unix_ms) VALUES (?1, ?2, ?3)
				 ON CONFLICT(task_id) DO UPDATE SET task_json = excluded.task_json, updated_at_unix_ms = excluded.updated_at_unix_ms",
				params![
					task.task_id.0,
					serde_json::to_string(&task).map_err(map_error)?,
					sql_i64_from_u64(now_unix_ms(), "tasks.updated_at_unix_ms").map_err(map_error)?,
				],
			)
			.map_err(map_error)?;
		Ok(())
	}

	fn load_task(&self, task_id: &TaskId) -> Result<Option<Task>, ControlPlaneError> {
		let connection = self.open().map_err(map_error)?;
		let encoded = connection
			.query_row(
				"SELECT task_json FROM tasks WHERE task_id = ?1",
				params![task_id.0],
				|row| row.get::<_, String>(0),
			)
			.optional()
			.map_err(map_error)?;
		encoded
			.map(|value| serde_json::from_str(&value).map_err(map_error))
			.transpose()
	}
}

#[derive(Debug, Clone)]
pub struct SqliteEventRepository {
	config: SqliteControlPlaneConfig,
}

impl SqliteEventRepository {
	pub fn connect(config: SqliteControlPlaneConfig) -> Result<Self, SqliteControlPlaneError> {
		open_connection(&config.path)?;
		Ok(Self { config })
	}

	fn open(&self) -> Result<Connection, SqliteControlPlaneError> {
		open_connection(&self.config.path)
	}
}

impl EventRepository for SqliteEventRepository {
	fn append_event(&mut self, event: TaskEvent) -> Result<(), ControlPlaneError> {
		let connection = self.open().map_err(map_error)?;
		connection
			.execute(
				"INSERT INTO task_events (task_id, event_json) VALUES (?1, ?2)",
				params![
					event.task_id.0,
					serde_json::to_string(&event).map_err(map_error)?
				],
			)
			.map_err(map_error)?;
		Ok(())
	}

	fn list_events(&self, task_id: &TaskId) -> Result<Vec<TaskEvent>, ControlPlaneError> {
		let connection = self.open().map_err(map_error)?;
		let mut statement = connection
			.prepare("SELECT event_json FROM task_events WHERE task_id = ?1 ORDER BY seq ASC")
			.map_err(map_error)?;
		statement
			.query_map(params![task_id.0], |row| row.get::<_, String>(0))
			.map_err(map_error)?
			.map(|row| {
				let encoded = row.map_err(map_error)?;
				serde_json::from_str(&encoded).map_err(map_error)
			})
			.collect()
	}

	fn load_replay_snapshot(
		&self,
		task_id: &TaskId,
	) -> Result<Option<TaskReplaySnapshot>, ControlPlaneError> {
		let connection = self.open().map_err(map_error)?;
		let encoded = connection
			.query_row(
				"SELECT snapshot_json FROM task_replay_snapshots WHERE task_id = ?1",
				params![task_id.0],
				|row| row.get::<_, String>(0),
			)
			.optional()
			.map_err(map_error)?;
		encoded
			.map(|value| serde_json::from_str(&value).map_err(map_error))
			.transpose()
	}

	fn compact_task_events(
		&mut self,
		mut snapshot: TaskReplaySnapshot,
		retain_events: usize,
	) -> Result<(), ControlPlaneError> {
		let mut connection = self.open().map_err(map_error)?;
		let transaction = connection
			.transaction_with_behavior(TransactionBehavior::Immediate)
			.map_err(map_error)?;
		let previous_snapshot = transaction
			.query_row(
				"SELECT snapshot_json FROM task_replay_snapshots WHERE task_id = ?1",
				params![snapshot.task_id.0],
				|row| row.get::<_, String>(0),
			)
			.optional()
			.map_err(map_error)?
			.map(|encoded| serde_json::from_str::<TaskReplaySnapshot>(&encoded))
			.transpose()
			.map_err(map_error)?;
		let current_event_count = transaction
			.query_row(
				"SELECT COUNT(*) FROM task_events WHERE task_id = ?1",
				params![snapshot.task_id.0],
				|row| {
					let count = row.get::<_, i64>(0)?;
					sql_usize_from_i64(count, "task_events.count")
						.map_err(rusqlite::Error::ToSqlConversionFailure)
				},
			)
			.map_err(map_error)?;
		let total_event_count =
			previous_snapshot
				.as_ref()
				.map_or(current_event_count, |existing| {
					existing
						.compacted_event_count
						.saturating_add(current_event_count)
				});
		let retained_count = current_event_count.min(retain_events);
		let cutoff_seq = if retained_count == 0 {
			None
		} else {
			transaction
				.query_row(
					"SELECT MIN(seq) FROM (
							SELECT seq FROM task_events
							WHERE task_id = ?1
							ORDER BY seq DESC
							LIMIT ?2
						)",
					params![
						snapshot.task_id.0,
						i64::try_from(retain_events).unwrap_or(i64::MAX),
					],
					|row| row.get::<_, Option<i64>>(0),
				)
				.map_err(map_error)?
		};
		if let Some(cutoff_seq) = cutoff_seq {
			transaction
				.execute(
					"DELETE FROM task_events WHERE task_id = ?1 AND seq < ?2",
					params![snapshot.task_id.0, cutoff_seq],
				)
				.map_err(map_error)?;
		} else {
			transaction
				.execute(
					"DELETE FROM task_events WHERE task_id = ?1",
					params![snapshot.task_id.0],
				)
				.map_err(map_error)?;
		}
		snapshot.compacted_event_count = total_event_count.saturating_sub(retained_count);
		transaction
			.execute(
				"INSERT INTO task_replay_snapshots (task_id, snapshot_json, updated_at_unix_ms) VALUES (?1, ?2, ?3)
				 ON CONFLICT(task_id) DO UPDATE SET snapshot_json = excluded.snapshot_json, updated_at_unix_ms = excluded.updated_at_unix_ms",
				params![
					snapshot.task_id.0,
					serde_json::to_string(&snapshot).map_err(map_error)?,
					sql_i64_from_u64(
						now_unix_ms(),
						"task_replay_snapshots.updated_at_unix_ms"
					)
					.map_err(map_error)?,
				],
			)
			.map_err(map_error)?;
		transaction.commit().map_err(map_error)?;
		Ok(())
	}
}

#[derive(Debug, Clone)]
pub struct SqliteApprovalRepository {
	config: SqliteControlPlaneConfig,
}

impl SqliteApprovalRepository {
	pub fn connect(config: SqliteControlPlaneConfig) -> Result<Self, SqliteControlPlaneError> {
		open_connection(&config.path)?;
		Ok(Self { config })
	}

	fn open(&self) -> Result<Connection, SqliteControlPlaneError> {
		open_connection(&self.config.path)
	}
}

impl ApprovalRepository for SqliteApprovalRepository {
	fn save_ticket(&mut self, ticket: ApprovalTicket) -> Result<(), ControlPlaneError> {
		let connection = self.open().map_err(map_error)?;
		connection
			.execute(
				"INSERT INTO approval_tickets (approval_id, task_id, ticket_json, updated_at_unix_ms) VALUES (?1, ?2, ?3, ?4)
				 ON CONFLICT(approval_id) DO UPDATE SET task_id = excluded.task_id, ticket_json = excluded.ticket_json, updated_at_unix_ms = excluded.updated_at_unix_ms",
				params![
					ticket.approval_id.0,
					ticket.task_id.0,
					serde_json::to_string(&ticket).map_err(map_error)?,
					sql_i64_from_u64(now_unix_ms(), "approval_tickets.updated_at_unix_ms")
						.map_err(map_error)?,
				],
			)
			.map_err(map_error)?;
		Ok(())
	}

	fn load_ticket(
		&self,
		approval_id: &ApprovalId,
	) -> Result<Option<ApprovalTicket>, ControlPlaneError> {
		let connection = self.open().map_err(map_error)?;
		let encoded = connection
			.query_row(
				"SELECT ticket_json FROM approval_tickets WHERE approval_id = ?1",
				params![approval_id.0],
				|row| row.get::<_, String>(0),
			)
			.optional()
			.map_err(map_error)?;
		encoded
			.map(|value| serde_json::from_str(&value).map_err(map_error))
			.transpose()
	}

	fn list_tickets_for_task(
		&self,
		task_id: &TaskId,
	) -> Result<Vec<ApprovalTicket>, ControlPlaneError> {
		let connection = self.open().map_err(map_error)?;
		let mut statement = connection
			.prepare(
				"SELECT ticket_json FROM approval_tickets WHERE task_id = ?1 ORDER BY approval_id ASC",
			)
			.map_err(map_error)?;
		statement
			.query_map(params![task_id.0], |row| row.get::<_, String>(0))
			.map_err(map_error)?
			.map(|row| {
				let encoded = row.map_err(map_error)?;
				serde_json::from_str(&encoded).map_err(map_error)
			})
			.collect()
	}
}

#[derive(Debug, Clone)]
pub struct SqliteResultRepository {
	config: SqliteControlPlaneConfig,
}

impl SqliteResultRepository {
	pub fn connect(config: SqliteControlPlaneConfig) -> Result<Self, SqliteControlPlaneError> {
		open_connection(&config.path)?;
		Ok(Self { config })
	}

	fn open(&self) -> Result<Connection, SqliteControlPlaneError> {
		open_connection(&self.config.path)
	}
}

impl ResultRepository for SqliteResultRepository {
	fn save_result(&mut self, result: ResultEnvelope) -> Result<(), ControlPlaneError> {
		let connection = self.open().map_err(map_error)?;
		connection
			.execute(
				"INSERT INTO node_results (task_id, node_id, result_json, updated_at_unix_ms) VALUES (?1, ?2, ?3, ?4)
				 ON CONFLICT(task_id, node_id) DO UPDATE SET result_json = excluded.result_json, updated_at_unix_ms = excluded.updated_at_unix_ms",
				params![
					result.task_id.0,
					result.node_id.0,
					serde_json::to_string(&result).map_err(map_error)?,
					sql_i64_from_u64(now_unix_ms(), "node_results.updated_at_unix_ms")
						.map_err(map_error)?,
				],
			)
			.map_err(map_error)?;
		Ok(())
	}

	fn load_result(
		&self,
		task_id: &TaskId,
		node_id: &NodeId,
	) -> Result<Option<ResultEnvelope>, ControlPlaneError> {
		let connection = self.open().map_err(map_error)?;
		let encoded = connection
			.query_row(
				"SELECT result_json FROM node_results WHERE task_id = ?1 AND node_id = ?2",
				params![task_id.0, node_id.0],
				|row| row.get::<_, String>(0),
			)
			.optional()
			.map_err(map_error)?;
		encoded
			.map(|value| serde_json::from_str(&value).map_err(map_error))
			.transpose()
	}

	fn list_results(&self, task_id: &TaskId) -> Result<Vec<ResultEnvelope>, ControlPlaneError> {
		let connection = self.open().map_err(map_error)?;
		let mut statement = connection
			.prepare("SELECT result_json FROM node_results WHERE task_id = ?1 ORDER BY node_id ASC")
			.map_err(map_error)?;
		statement
			.query_map(params![task_id.0], |row| row.get::<_, String>(0))
			.map_err(map_error)?
			.map(|row| {
				let encoded = row.map_err(map_error)?;
				serde_json::from_str(&encoded).map_err(map_error)
			})
			.collect()
	}
}

#[derive(Debug, Clone)]
pub struct SqliteDispatchQueue {
	config: SqliteControlPlaneConfig,
	lease_duration_ms: u64,
	max_in_flight: usize,
}

impl SqliteDispatchQueue {
	pub fn connect(config: SqliteControlPlaneConfig) -> Result<Self, SqliteControlPlaneError> {
		Self::with_limits(config, DEFAULT_MAX_IN_FLIGHT, DEFAULT_LEASE_DURATION_MS)
	}

	pub fn with_limits(
		config: SqliteControlPlaneConfig,
		max_in_flight: usize,
		lease_duration_ms: u64,
	) -> Result<Self, SqliteControlPlaneError> {
		open_connection(&config.path)?;
		Ok(Self {
			config,
			lease_duration_ms,
			max_in_flight,
		})
	}

	fn open(&self) -> Result<Connection, SqliteControlPlaneError> {
		open_connection(&self.config.path)
	}

	fn verify_lease(
		transaction: &rusqlite::Transaction<'_>,
		lease: &DispatchLease,
	) -> Result<(), SqliteControlPlaneError> {
		let stored = transaction
			.query_row(
				"SELECT consumer_id, lease_token, expires_at_unix_ms FROM dispatch_entries WHERE entry_id = ?1 AND state = 'leased'",
				params![lease.entry_id],
				|row| {
					Ok((
						row.get::<_, Option<String>>(0)?,
						row.get::<_, Option<String>>(1)?,
						sql_opt_u64_from_row(row, 2, "dispatch_entries.expires_at_unix_ms")?,
					))
				},
			)
			.optional()?;
		let Some((consumer_id, lease_token, expires_at_unix_ms)) = stored else {
			return Err(SqliteControlPlaneError::Storage(format!(
				"dispatch lease not found for {}",
				lease.entry_id
			)));
		};
		if consumer_id.as_deref() != Some(lease.consumer_id.as_str())
			|| lease_token.as_deref() != Some(lease.lease_token.as_str())
			|| expires_at_unix_ms != Some(lease.expires_at_unix_ms)
		{
			return Err(SqliteControlPlaneError::Storage(format!(
				"dispatch lease mismatch for {}",
				lease.entry_id
			)));
		}
		Ok(())
	}
}

impl DispatchQueue for SqliteDispatchQueue {
	fn publish(&mut self, envelope: DispatchEnvelope) -> Result<(), ControlPlaneError> {
		let connection = self.open().map_err(map_error)?;
		connection
			.execute(
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
					sql_i64_from_u64(now_unix_ms(), "dispatch_entries.updated_at_unix_ms")
						.map_err(map_error)?,
				],
			)
			.map_err(map_error)?;
		Ok(())
	}

	fn claim(
		&mut self,
		consumer_id: &str,
		now_unix_ms: u64,
	) -> Result<Option<DispatchClaim>, ControlPlaneError> {
		let mut connection = self.open().map_err(map_error)?;
		let transaction = connection
			.transaction_with_behavior(TransactionBehavior::Immediate)
			.map_err(map_error)?;
		requeue_expired_entries(&transaction, now_unix_ms).map_err(map_error)?;
		let leased_count: usize = transaction
			.query_row(
				"SELECT COUNT(*) FROM dispatch_entries WHERE state = 'leased'",
				[],
				|row| {
					let count = row.get::<_, i64>(0)?;
					sql_usize_from_i64(count, "dispatch_entries.leased_count")
						.map_err(rusqlite::Error::ToSqlConversionFailure)
				},
			)
			.map_err(map_error)?;
		if leased_count >= self.max_in_flight {
			transaction.commit().map_err(map_error)?;
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
			.optional()
			.map_err(map_error)?;
		let Some(envelope) = envelope else {
			transaction.commit().map_err(map_error)?;
			return Ok(None);
		};

		let lease = DispatchLease {
			entry_id: envelope.entry_id.clone(),
			consumer_id: consumer_id.to_string(),
			lease_token: new_lease_token(&envelope.entry_id, consumer_id),
			expires_at_unix_ms: now_unix_ms.saturating_add(self.lease_duration_ms),
		};
		let updated = transaction
			.execute(
				"UPDATE dispatch_entries
				 SET state = 'leased', consumer_id = ?1, lease_token = ?2, expires_at_unix_ms = ?3, updated_at_unix_ms = ?4
				 WHERE entry_id = ?5 AND state = 'queued'",
				params![
					lease.consumer_id,
					lease.lease_token,
					sql_i64_from_u64(lease.expires_at_unix_ms, "dispatch_entries.expires_at_unix_ms")
						.map_err(map_error)?,
					sql_i64_from_u64(now_unix_ms, "dispatch_entries.updated_at_unix_ms")
						.map_err(map_error)?,
					lease.entry_id,
				],
			)
			.map_err(map_error)?;
		transaction.commit().map_err(map_error)?;
		if updated == 0 {
			return Ok(None);
		}

		Ok(Some(DispatchClaim { envelope, lease }))
	}

	fn ack(&mut self, lease: &DispatchLease) -> Result<(), ControlPlaneError> {
		let mut connection = self.open().map_err(map_error)?;
		let transaction = connection
			.transaction_with_behavior(TransactionBehavior::Immediate)
			.map_err(map_error)?;
		Self::verify_lease(&transaction, lease).map_err(map_error)?;
		transaction
			.execute(
				"DELETE FROM dispatch_entries WHERE entry_id = ?1 AND state = 'leased'",
				params![lease.entry_id],
			)
			.map_err(map_error)?;
		transaction.commit().map_err(map_error)?;
		Ok(())
	}

	fn nack(&mut self, lease: &DispatchLease, retry: RetryClaim) -> Result<(), ControlPlaneError> {
		let mut connection = self.open().map_err(map_error)?;
		let transaction = connection
			.transaction_with_behavior(TransactionBehavior::Immediate)
			.map_err(map_error)?;
		Self::verify_lease(&transaction, lease).map_err(map_error)?;
		transaction
			.execute(
				"UPDATE dispatch_entries
				 SET attempt = ?1,
					 state = 'queued',
					 consumer_id = NULL,
					 lease_token = NULL,
					 expires_at_unix_ms = NULL,
					 updated_at_unix_ms = ?2
				 WHERE entry_id = ?3 AND state = 'leased'",
				params![
					retry.next_attempt,
					sql_i64_from_u64(now_unix_ms(), "dispatch_entries.updated_at_unix_ms")
						.map_err(map_error)?,
					lease.entry_id,
				],
			)
			.map_err(map_error)?;
		transaction.commit().map_err(map_error)?;
		Ok(())
	}

	fn renew_lease(
		&mut self,
		lease: &DispatchLease,
		now_unix_ms: u64,
	) -> Result<Option<DispatchLease>, ControlPlaneError> {
		let mut connection = self.open().map_err(map_error)?;
		let transaction = connection
			.transaction_with_behavior(TransactionBehavior::Immediate)
			.map_err(map_error)?;
		requeue_expired_entries(&transaction, now_unix_ms).map_err(map_error)?;
		Self::verify_lease(&transaction, lease).map_err(map_error)?;
		let renewed = DispatchLease {
			entry_id: lease.entry_id.clone(),
			consumer_id: lease.consumer_id.clone(),
			lease_token: new_lease_token(&lease.entry_id, &lease.consumer_id),
			expires_at_unix_ms: now_unix_ms.saturating_add(self.lease_duration_ms),
		};
		transaction
			.execute(
				"UPDATE dispatch_entries
				 SET lease_token = ?1, expires_at_unix_ms = ?2, updated_at_unix_ms = ?3
				 WHERE entry_id = ?4 AND state = 'leased'",
				params![
					renewed.lease_token,
					sql_i64_from_u64(
						renewed.expires_at_unix_ms,
						"dispatch_entries.expires_at_unix_ms"
					)
					.map_err(map_error)?,
					sql_i64_from_u64(now_unix_ms, "dispatch_entries.updated_at_unix_ms")
						.map_err(map_error)?,
					renewed.entry_id,
				],
			)
			.map_err(map_error)?;
		transaction.commit().map_err(map_error)?;
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
				|row| {
					let count = row.get::<_, i64>(0)?;
					sql_usize_from_i64(count, "dispatch_entries.queued_count")
						.map_err(rusqlite::Error::ToSqlConversionFailure)
				},
			)
			.unwrap_or(0);
		let leased = connection
			.query_row(
				"SELECT COUNT(*) FROM dispatch_entries WHERE state = 'leased'",
				[],
				|row| {
					let count = row.get::<_, i64>(0)?;
					sql_usize_from_i64(count, "dispatch_entries.leased_count")
						.map_err(rusqlite::Error::ToSqlConversionFailure)
				},
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

/// SQLite-backed control-plane bundle used by entry-registry assembly.
pub struct SqliteControlPlaneDataPlane;

impl SqliteControlPlaneDataPlane {
	pub fn connect(
		config: SqliteControlPlaneConfig,
	) -> Result<ControlPlaneDataPlane, SqliteControlPlaneError> {
		Ok(ControlPlaneDataPlane {
			task_repo: Box::new(SqliteTaskRepository::connect(config.clone())?),
			event_repo: Box::new(SqliteEventRepository::connect(config.clone())?),
			approval_repo: Box::new(SqliteApprovalRepository::connect(config.clone())?),
			result_repo: Box::new(SqliteResultRepository::connect(config.clone())?),
			dispatch_queue: Box::new(SqliteDispatchQueue::connect(config)?),
		})
	}
}

fn map_error<E: ToString>(error: E) -> ControlPlaneError {
	ControlPlaneError::Backend(error.to_string())
}

fn open_connection(path: &Path) -> Result<Connection, SqliteControlPlaneError> {
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

fn ensure_schema_objects(connection: &Connection) -> Result<(), SqliteControlPlaneError> {
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
		CREATE TABLE IF NOT EXISTS task_replay_snapshots (
			task_id TEXT PRIMARY KEY,
			snapshot_json TEXT NOT NULL,
			updated_at_unix_ms INTEGER NOT NULL DEFAULT 0
		);
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
) -> Result<(), SqliteControlPlaneError> {
	transaction.execute(
		"UPDATE dispatch_entries
		 SET state = 'queued',
			 consumer_id = NULL,
			 lease_token = NULL,
			 expires_at_unix_ms = NULL,
			 updated_at_unix_ms = ?1
		 WHERE state = 'leased' AND expires_at_unix_ms IS NOT NULL AND expires_at_unix_ms <= ?2",
		params![
			sql_i64_from_u64(now_unix_ms, "dispatch_entries.updated_at_unix_ms")?,
			sql_i64_from_u64(now_unix_ms, "dispatch_entries.expires_at_unix_ms")?,
		],
	)?;
	Ok(())
}

fn sql_i64_from_u64(value: u64, field: &str) -> Result<i64, SqliteControlPlaneError> {
	i64::try_from(value).map_err(|_| {
		SqliteControlPlaneError::Storage(format!(
			"{field} value {value} exceeds SQLite INTEGER range"
		))
	})
}

fn sql_usize_from_i64(
	value: i64,
	field: &str,
) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
	if value < 0 {
		return Err(format!("{field} returned negative SQLite INTEGER {value}").into());
	}
	usize::try_from(value).map_err(|_| format!("{field} value {value} exceeds usize range").into())
}

fn sql_u64_from_i64(value: i64, field: &str) -> Result<u64, rusqlite::Error> {
	if value < 0 {
		return Err(rusqlite::Error::FromSqlConversionFailure(
			0,
			rusqlite::types::Type::Integer,
			format!("{field} returned negative SQLite INTEGER {value}").into(),
		));
	}
	u64::try_from(value).map_err(|_| {
		rusqlite::Error::FromSqlConversionFailure(
			0,
			rusqlite::types::Type::Integer,
			format!("{field} value {value} exceeds u64 range").into(),
		)
	})
}

fn sql_opt_u64_from_row(
	row: &rusqlite::Row<'_>,
	index: usize,
	field: &str,
) -> Result<Option<u64>, rusqlite::Error> {
	row.get::<_, Option<i64>>(index)?
		.map(|value| sql_u64_from_i64(value, field))
		.transpose()
}

fn ensure_parent_dir(path: &Path) -> Result<(), SqliteControlPlaneError> {
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
	use std::path::PathBuf;

	use roku_common_types::{
		ApprovalStatus, EvidenceItem, RequestId, ResultStatus, TaskEventKind, TaskState,
	};

	use super::*;

	fn unique_path(suffix: &str) -> PathBuf {
		let nanos = SystemTime::now()
			.duration_since(UNIX_EPOCH)
			.expect("clock should be after epoch")
			.as_nanos();
		std::env::temp_dir().join(format!("roku-control-plane-{suffix}-{nanos}.db"))
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
			kind: TaskEventKind::StateTransition,
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
	fn sqlite_control_plane_roundtrips_all_repos() {
		let path = unique_path("repos");
		let config = SqliteControlPlaneConfig::new(path.clone());
		let mut bundle =
			SqliteControlPlaneDataPlane::connect(config).expect("control-plane should connect");

		bundle
			.task_repo
			.save_task(sample_task())
			.expect("save task");
		bundle
			.event_repo
			.append_event(sample_event())
			.expect("save event");
		bundle
			.approval_repo
			.save_ticket(sample_ticket())
			.expect("save ticket");
		bundle
			.result_repo
			.save_result(sample_result())
			.expect("save result");

		assert!(
			bundle
				.task_repo
				.load_task(&TaskId("task-1".to_string()))
				.expect("load task")
				.is_some()
		);
		assert_eq!(
			bundle
				.event_repo
				.list_events(&TaskId("task-1".to_string()))
				.expect("load events")
				.len(),
			1
		);
		assert!(
			bundle
				.approval_repo
				.load_ticket(&ApprovalId("approval-1".to_string()))
				.expect("load ticket")
				.is_some()
		);
		assert!(
			bundle
				.result_repo
				.load_result(&TaskId("task-1".to_string()), &NodeId("node-1".to_string()))
				.expect("load result")
				.is_some()
		);

		let _ = fs::remove_file(path);
	}

	#[test]
	fn sqlite_dispatch_queue_requeues_expired_leases() {
		let path = unique_path("dispatch");
		let config = SqliteControlPlaneConfig::new(path.clone());
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
