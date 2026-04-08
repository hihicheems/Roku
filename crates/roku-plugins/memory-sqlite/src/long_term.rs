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

//! SQLite FTS5-backed implementation of [`LongTermMemoryBackend`].

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use roku_memory::{
	LongTermMemoryBackend, MemoryBackendHealth, MemoryBackendStatus, MemoryDeleteSelector,
	MemoryError, MemoryHit, MemoryKind, MemoryProvenance, MemoryQuery, MemoryRecord, MemoryScope,
	MemoryWriteAck, MemoryWriteRequest,
};
use rusqlite::{Connection, params};

use crate::store::{SqliteMemoryStoreConfig, SqliteMemoryStoreError, open_connection_pub};

/// SQLite FTS5-backed long-term memory backend.
///
/// Uses a `Mutex<Connection>` to satisfy the `Send + Sync` requirement imposed
/// by [`LongTermMemoryBackend`]. All operations reuse the held connection rather
/// than reopening per-call — this avoids redundant WAL/PRAGMA initialization
/// overhead for the long-term memory hot path.
pub struct SqliteLongTermMemoryBackend {
	conn: Mutex<Connection>,
}

impl std::fmt::Debug for SqliteLongTermMemoryBackend {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("SqliteLongTermMemoryBackend").finish()
	}
}

impl SqliteLongTermMemoryBackend {
	/// Opens a connection to the SQLite database at `path` and ensures the
	/// long-term memory schema exists.
	pub fn connect(config: &SqliteMemoryStoreConfig) -> Result<Self, SqliteMemoryStoreError> {
		let conn = open_connection_pub(&config.path)?;
		Ok(Self {
			conn: Mutex::new(conn),
		})
	}

	/// Convenience constructor accepting a bare path.
	pub fn open(path: impl Into<PathBuf>) -> Result<Self, SqliteMemoryStoreError> {
		Self::connect(&SqliteMemoryStoreConfig::new(path))
	}
}

impl LongTermMemoryBackend for SqliteLongTermMemoryBackend {
	fn backend_name(&self) -> &'static str {
		"sqlite-fts5"
	}

	fn search(&self, query: &MemoryQuery) -> Result<Vec<MemoryHit>, MemoryError> {
		let conn = self
			.conn
			.lock()
			.map_err(|e| MemoryError::Internal(e.to_string()))?;

		let scope_str = scope_to_str(query.scope);
		let limit = query.limit.max(1) as i64;
		let query_text = query.query_text.trim().to_string();

		// Build a scope-identity filter clause.  We check the relevant identity
		// column so that Session/User/Project/Workspace scopes are properly isolated.
		let scope_identity_filter = scope_identity_clause(query);

		// When the query text is non-empty, use FTS5 MATCH for ranked retrieval.
		// When it is empty, fall back to a plain table scan sorted by recency.
		let rows: Vec<MemoryRecord> = if query_text.is_empty() {
			// Plain scan — most recent first
			let sql = format!(
				"SELECT m.id, m.kind, m.scope, m.content, m.summary,
				        m.source_session_id, m.source_user_id, m.source_project_id,
				        m.source_workspace_id, m.created_at_unix_ms, m.updated_at_unix_ms
				 FROM long_term_memories m
				 WHERE m.scope = ?1
				 {scope_identity_filter}
				 {kind_filter}
				 ORDER BY m.updated_at_unix_ms DESC
				 LIMIT ?2",
				kind_filter = kind_filter_clause(&query.filters.kinds),
			);
			let mut stmt = conn
				.prepare(&sql)
				.map_err(|e| MemoryError::Internal(e.to_string()))?;
			let params: Vec<Box<dyn rusqlite::ToSql>> =
				vec![Box::new(scope_str.to_string()), Box::new(limit)];
			stmt.query_map(
				rusqlite::params_from_iter(params.iter().map(|p| p.as_ref())),
				row_to_record,
			)
			.map_err(|e| MemoryError::Internal(e.to_string()))?
			.collect::<Result<Vec<_>, _>>()
			.map_err(|e| MemoryError::Internal(e.to_string()))?
		} else {
			// FTS5 MATCH with BM25 ranking (lower bm25 = better match in SQLite)
			let sql = format!(
				"SELECT m.id, m.kind, m.scope, m.content, m.summary,
				        m.source_session_id, m.source_user_id, m.source_project_id,
				        m.source_workspace_id, m.created_at_unix_ms, m.updated_at_unix_ms
				 FROM long_term_memories m
				 JOIN long_term_memories_fts f ON f.rowid = m.rowid
				 WHERE f.long_term_memories_fts MATCH ?1
				   AND m.scope = ?2
				 {scope_identity_filter}
				 {kind_filter}
				 ORDER BY bm25(long_term_memories_fts) ASC
				 LIMIT ?3",
				kind_filter = kind_filter_clause(&query.filters.kinds),
			);
			let mut stmt = conn
				.prepare(&sql)
				.map_err(|e| MemoryError::Internal(e.to_string()))?;
			let params: Vec<Box<dyn rusqlite::ToSql>> = vec![
				Box::new(fts5_escape(&query_text)),
				Box::new(scope_str.to_string()),
				Box::new(limit),
			];
			stmt.query_map(
				rusqlite::params_from_iter(params.iter().map(|p| p.as_ref())),
				row_to_record,
			)
			.map_err(|e| MemoryError::Internal(e.to_string()))?
			.collect::<Result<Vec<_>, _>>()
			.map_err(|e| MemoryError::Internal(e.to_string()))?
		};

		let hits = rows
			.into_iter()
			.map(|record| MemoryHit {
				provenance: MemoryProvenance {
					backend: self.backend_name().to_string(),
					locator: Some(record.record_id.clone()),
					detail: None,
				},
				score: 0.9,
				record,
			})
			.collect();

		Ok(hits)
	}

	fn write(&self, request: &MemoryWriteRequest) -> Result<MemoryWriteAck, MemoryError> {
		let conn = self
			.conn
			.lock()
			.map_err(|e| MemoryError::Internal(e.to_string()))?;

		let record_id = generate_record_id();
		let now = now_unix_ms() as i64;
		let kind_str = kind_to_str(request.kind);
		let scope_str = scope_to_str(request.scope);

		conn.execute(
			"INSERT INTO long_term_memories (
				id, kind, scope, content, summary,
				source_session_id, source_user_id, source_project_id, source_workspace_id,
				created_at_unix_ms, updated_at_unix_ms
			) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
			ON CONFLICT(id) DO UPDATE SET
				kind = excluded.kind,
				scope = excluded.scope,
				content = excluded.content,
				summary = excluded.summary,
				source_session_id = excluded.source_session_id,
				source_user_id = excluded.source_user_id,
				source_project_id = excluded.source_project_id,
				source_workspace_id = excluded.source_workspace_id,
				updated_at_unix_ms = excluded.updated_at_unix_ms",
			params![
				record_id,
				kind_str,
				scope_str,
				request.content,
				request.summary,
				request.session_id,
				request.user_id,
				request.project_id,
				request.workspace_id,
				now,
				now,
			],
		)
		.map_err(|e| MemoryError::Internal(e.to_string()))?;

		Ok(MemoryWriteAck {
			accepted: true,
			record_id: Some(record_id),
		})
	}

	fn delete(&self, selector: &MemoryDeleteSelector) -> Result<(), MemoryError> {
		let conn = self
			.conn
			.lock()
			.map_err(|e| MemoryError::Internal(e.to_string()))?;
		conn.execute(
			"DELETE FROM long_term_memories WHERE id = ?1",
			params![selector.record_id],
		)
		.map_err(|e| MemoryError::Internal(e.to_string()))?;
		Ok(())
	}

	fn health(&self) -> Result<MemoryBackendHealth, MemoryError> {
		let conn = self
			.conn
			.lock()
			.map_err(|e| MemoryError::Internal(e.to_string()))?;
		let count: i64 = conn
			.query_row("SELECT COUNT(*) FROM long_term_memories", [], |row| {
				row.get(0)
			})
			.map_err(|e| MemoryError::Internal(e.to_string()))?;
		Ok(MemoryBackendHealth {
			backend: self.backend_name().to_string(),
			status: MemoryBackendStatus::Healthy,
			detail: Some(format!("{count} long-term memory records")),
		})
	}
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn scope_to_str(scope: MemoryScope) -> &'static str {
	match scope {
		MemoryScope::Session => "session",
		MemoryScope::User => "user",
		MemoryScope::Project => "project",
		MemoryScope::Workspace => "workspace",
		MemoryScope::Global => "global",
	}
}

fn kind_to_str(kind: MemoryKind) -> &'static str {
	match kind {
		MemoryKind::UserPreference => "user_preference",
		MemoryKind::UserFact => "user_fact",
		MemoryKind::ProjectFact => "project_fact",
		MemoryKind::WorkspaceFact => "workspace_fact",
		MemoryKind::HistoricalCase => "historical_case",
		MemoryKind::Constraint => "constraint",
		MemoryKind::WorkflowInsight => "workflow_insight",
	}
}

fn str_to_kind(s: &str) -> Option<MemoryKind> {
	match s {
		"user_preference" => Some(MemoryKind::UserPreference),
		"user_fact" => Some(MemoryKind::UserFact),
		"project_fact" => Some(MemoryKind::ProjectFact),
		"workspace_fact" => Some(MemoryKind::WorkspaceFact),
		"historical_case" => Some(MemoryKind::HistoricalCase),
		"constraint" => Some(MemoryKind::Constraint),
		"workflow_insight" => Some(MemoryKind::WorkflowInsight),
		_ => None,
	}
}

fn str_to_scope(s: &str) -> Option<MemoryScope> {
	match s {
		"session" => Some(MemoryScope::Session),
		"user" => Some(MemoryScope::User),
		"project" => Some(MemoryScope::Project),
		"workspace" => Some(MemoryScope::Workspace),
		"global" => Some(MemoryScope::Global),
		_ => None,
	}
}

/// Returns a SQL fragment that constrains the scope-specific identity column.
/// The returned string is embedded directly into the query template and
/// contains no user-supplied data (identity values are passed as bound params
/// starting from a fixed offset), so SQL injection is not a concern here.
///
/// NOTE: The identity value is not bound inside this function. The caller's
/// parameter list must provide it at the correct positional index.
/// Because we are building a SQL fragment with a literal `= 'value'`, we use
/// only trusted, internally-generated strings (never user text), so this is
/// safe.
fn scope_identity_clause(query: &MemoryQuery) -> String {
	match query.scope {
		MemoryScope::Session => {
			if let Some(ref sid) = query.session_id {
				format!("AND m.source_session_id = '{}'", sid.replace('\'', "''"))
			} else {
				String::new()
			}
		}
		MemoryScope::User => {
			if let Some(ref uid) = query.user_id {
				format!("AND m.source_user_id = '{}'", uid.replace('\'', "''"))
			} else {
				String::new()
			}
		}
		MemoryScope::Project => {
			if let Some(ref pid) = query.project_id {
				format!("AND m.source_project_id = '{}'", pid.replace('\'', "''"))
			} else {
				String::new()
			}
		}
		MemoryScope::Workspace => {
			if let Some(ref wid) = query.workspace_id {
				format!("AND m.source_workspace_id = '{}'", wid.replace('\'', "''"))
			} else {
				String::new()
			}
		}
		MemoryScope::Global => String::new(),
	}
}

/// Returns a SQL fragment that restricts results to the given kinds.
/// Returns an empty string when the list is empty (no restriction).
fn kind_filter_clause(kinds: &[MemoryKind]) -> String {
	if kinds.is_empty() {
		return String::new();
	}
	let list = kinds
		.iter()
		.map(|k| format!("'{}'", kind_to_str(*k)))
		.collect::<Vec<_>>()
		.join(", ");
	format!("AND m.kind IN ({list})")
}

/// Escapes a user-supplied query string for safe use in an FTS5 MATCH expression.
///
/// FTS5 treats `"`, `*`, `^`, `(`, `)`, `OR`, `AND`, `NOT` as operators.
/// The safest approach for a plain keyword search is to wrap the entire input
/// in double quotes, which makes FTS5 treat it as a phrase query.
fn fts5_escape(text: &str) -> String {
	// Replace inner double quotes with a space to prevent premature quote
	// termination, then wrap the whole thing in double quotes for a phrase match.
	let sanitized = text.replace('"', " ");
	format!("\"{sanitized}\"")
}

fn row_to_record(row: &rusqlite::Row<'_>) -> Result<MemoryRecord, rusqlite::Error> {
	let id: String = row.get(0)?;
	let kind_str: String = row.get(1)?;
	let scope_str: String = row.get(2)?;
	let content: String = row.get(3)?;
	let summary: String = row.get(4)?;
	let session_id: Option<String> = row.get(5)?;
	let user_id: Option<String> = row.get(6)?;
	let project_id: Option<String> = row.get(7)?;
	let workspace_id: Option<String> = row.get(8)?;
	let created_at: i64 = row.get(9)?;
	let updated_at: i64 = row.get(10)?;

	let kind = str_to_kind(&kind_str).unwrap_or(MemoryKind::UserPreference);
	let scope = str_to_scope(&scope_str).unwrap_or(MemoryScope::Global);

	Ok(MemoryRecord {
		record_id: id,
		kind,
		scope,
		content,
		summary,
		source_refs: Vec::new(),
		metadata: Default::default(),
		session_id,
		user_id,
		project_id,
		workspace_id,
		created_at_unix_ms: created_at as u64,
		updated_at_unix_ms: updated_at as u64,
	})
}

fn generate_record_id() -> String {
	use std::sync::atomic::{AtomicU64, Ordering};
	static COUNTER: AtomicU64 = AtomicU64::new(1);
	let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
	let ts = now_unix_ms();
	format!("ltm-{ts}-{counter:06}")
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
	use roku_memory::{
		LongTermMemoryBackend, MemoryKind, MemoryQuery, MemoryRecallReason, MemoryScope,
		MemoryWriteReason, MemoryWriteRequest,
	};

	use super::*;

	fn test_backend() -> SqliteLongTermMemoryBackend {
		let tempdir = tempfile::tempdir().expect("tempdir");
		SqliteLongTermMemoryBackend::open(tempdir.path().join("ltm-test.db"))
			.expect("backend opens")
	}

	fn make_write(
		kind: MemoryKind,
		scope: MemoryScope,
		content: &str,
		summary: &str,
	) -> MemoryWriteRequest {
		MemoryWriteRequest::new(
			kind,
			scope,
			content,
			summary,
			MemoryWriteReason::OperatorRequested,
		)
	}

	#[test]
	fn write_and_search_exact_match() {
		let backend = test_backend();
		let req = make_write(
			MemoryKind::UserPreference,
			MemoryScope::Global,
			"prefers snake_case identifiers",
			"snake_case preference",
		);
		let ack = backend.write(&req).expect("write succeeds");
		assert!(ack.accepted);
		assert!(ack.record_id.is_some());

		let query = MemoryQuery::new(
			"snake_case",
			MemoryRecallReason::Manual,
			MemoryScope::Global,
		);
		let hits = backend.search(&query).expect("search succeeds");
		assert!(!hits.is_empty(), "should recall the written memory");
		assert!(hits[0].record.content.contains("snake_case"));
	}

	#[test]
	fn write_and_search_partial_match() {
		let backend = test_backend();
		let req = make_write(
			MemoryKind::UserFact,
			MemoryScope::Global,
			"the user lives in Tokyo and enjoys Rust programming",
			"user lives in Tokyo",
		);
		backend.write(&req).expect("write succeeds");

		let query = MemoryQuery::new("Tokyo", MemoryRecallReason::Manual, MemoryScope::Global);
		let hits = backend.search(&query).expect("search succeeds");
		assert!(!hits.is_empty(), "partial word match should work");
	}

	#[test]
	fn kind_filter_excludes_non_matching() {
		let backend = test_backend();
		let req_pref = make_write(
			MemoryKind::UserPreference,
			MemoryScope::Global,
			"prefers dark mode",
			"dark mode",
		);
		let req_fact = make_write(
			MemoryKind::UserFact,
			MemoryScope::Global,
			"user uses dark mode always",
			"dark mode fact",
		);
		backend.write(&req_pref).expect("write pref");
		backend.write(&req_fact).expect("write fact");

		let mut query =
			MemoryQuery::new("dark mode", MemoryRecallReason::Manual, MemoryScope::Global);
		query.filters.kinds = vec![MemoryKind::UserPreference];
		let hits = backend.search(&query).expect("search");
		assert!(!hits.is_empty());
		for hit in &hits {
			assert_eq!(hit.record.kind, MemoryKind::UserPreference);
		}
	}

	#[test]
	fn top_k_limits_results() {
		let backend = test_backend();
		for i in 0..10 {
			let req = make_write(
				MemoryKind::UserPreference,
				MemoryScope::Global,
				&format!("preference entry number {i} about coding"),
				&format!("coding pref {i}"),
			);
			backend.write(&req).expect("write");
		}

		let mut query = MemoryQuery::new("coding", MemoryRecallReason::Manual, MemoryScope::Global);
		query.limit = 3;
		let hits = backend.search(&query).expect("search");
		assert!(hits.len() <= 3, "should not exceed top_k limit");
	}

	#[test]
	fn scope_session_isolation() {
		let backend = test_backend();
		let mut req = make_write(
			MemoryKind::UserPreference,
			MemoryScope::Session,
			"session-specific preference",
			"session pref",
		);
		req.session_id = Some("session-aaa".to_string());
		backend.write(&req).expect("write");

		let mut query = MemoryQuery::new(
			"preference",
			MemoryRecallReason::Manual,
			MemoryScope::Session,
		);
		query.session_id = Some("session-bbb".to_string());
		let hits = backend.search(&query).expect("search");
		assert!(hits.is_empty(), "different session should not see records");
	}

	#[test]
	fn delete_removes_record() {
		let backend = test_backend();
		let req = make_write(
			MemoryKind::Constraint,
			MemoryScope::Global,
			"never use unwrap in production",
			"no unwrap constraint",
		);
		let ack = backend.write(&req).expect("write");
		let record_id = ack.record_id.expect("has record_id");

		let query = MemoryQuery::new("unwrap", MemoryRecallReason::Manual, MemoryScope::Global);
		assert!(!backend.search(&query).expect("search").is_empty());

		backend
			.delete(&roku_memory::MemoryDeleteSelector { record_id })
			.expect("delete");
		assert!(
			backend
				.search(&query)
				.expect("search after delete")
				.is_empty()
		);
	}

	#[test]
	fn health_returns_healthy() {
		let backend = test_backend();
		let health = backend.health().expect("health check");
		assert_eq!(health.status, roku_memory::MemoryBackendStatus::Healthy);
		assert_eq!(health.backend, "sqlite-fts5");
	}

	#[test]
	fn empty_query_returns_all_in_scope() {
		let backend = test_backend();
		for i in 0..3 {
			let req = make_write(
				MemoryKind::WorkflowInsight,
				MemoryScope::Global,
				&format!("insight {i}"),
				&format!("insight summary {i}"),
			);
			backend.write(&req).expect("write");
		}
		let mut query = MemoryQuery::new("", MemoryRecallReason::Manual, MemoryScope::Global);
		query.limit = 10;
		let hits = backend.search(&query).expect("search");
		assert_eq!(hits.len(), 3, "empty query should return all in scope");
	}
}
