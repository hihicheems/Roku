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

use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use thiserror::Error;

use crate::types::{
	MemoryFilters, MemoryHit, MemoryProvenance, MemoryQuery, MemoryRecord, MemoryScope,
	MemoryWriteRequest,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryWriteAck {
	pub accepted: bool,
	pub record_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryBackendStatus {
	Healthy,
	Degraded,
	Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryBackendHealth {
	pub backend: String,
	pub status: MemoryBackendStatus,
	pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryDeleteSelector {
	pub record_id: String,
}

#[derive(Debug, Error)]
pub enum MemoryError {
	#[error("memory backend is unavailable: {0}")]
	Unavailable(String),
	#[error("memory backend rejected request: {0}")]
	Rejected(String),
	#[error("memory backend failed: {0}")]
	Internal(String),
}

pub trait LongTermMemoryBackend: Send + Sync {
	fn backend_name(&self) -> &'static str;

	fn search(&self, query: &MemoryQuery) -> Result<Vec<MemoryHit>, MemoryError>;

	fn write(&self, request: &MemoryWriteRequest) -> Result<MemoryWriteAck, MemoryError>;

	fn delete(&self, selector: &MemoryDeleteSelector) -> Result<(), MemoryError>;

	fn health(&self) -> Result<MemoryBackendHealth, MemoryError>;
}

#[derive(Debug, Default)]
pub struct NoopLongTermMemoryBackend;

impl LongTermMemoryBackend for NoopLongTermMemoryBackend {
	fn backend_name(&self) -> &'static str {
		"noop"
	}

	fn search(&self, _query: &MemoryQuery) -> Result<Vec<MemoryHit>, MemoryError> {
		Ok(Vec::new())
	}

	fn write(&self, _request: &MemoryWriteRequest) -> Result<MemoryWriteAck, MemoryError> {
		Ok(MemoryWriteAck {
			accepted: false,
			record_id: None,
		})
	}

	fn delete(&self, _selector: &MemoryDeleteSelector) -> Result<(), MemoryError> {
		Ok(())
	}

	fn health(&self) -> Result<MemoryBackendHealth, MemoryError> {
		Ok(MemoryBackendHealth {
			backend: self.backend_name().to_string(),
			status: MemoryBackendStatus::Healthy,
			detail: Some("long-term memory is disabled".to_string()),
		})
	}
}

#[derive(Debug, Default)]
pub struct InMemoryLongTermMemoryBackend {
	state: Mutex<InMemoryBackendState>,
}

#[derive(Debug, Default)]
struct InMemoryBackendState {
	next_record_id: u64,
	records: Vec<MemoryRecord>,
	queries: Vec<MemoryQuery>,
	writes: Vec<MemoryWriteRequest>,
	deletes: Vec<MemoryDeleteSelector>,
}

impl InMemoryLongTermMemoryBackend {
	pub fn recorded_queries(&self) -> Vec<MemoryQuery> {
		self.state
			.lock()
			.expect("in-memory memory backend state poisoned")
			.queries
			.clone()
	}

	pub fn recorded_writes(&self) -> Vec<MemoryWriteRequest> {
		self.state
			.lock()
			.expect("in-memory memory backend state poisoned")
			.writes
			.clone()
	}

	pub fn stored_records(&self) -> Vec<MemoryRecord> {
		self.state
			.lock()
			.expect("in-memory memory backend state poisoned")
			.records
			.clone()
	}
}

impl LongTermMemoryBackend for InMemoryLongTermMemoryBackend {
	fn backend_name(&self) -> &'static str {
		"in-memory"
	}

	fn search(&self, query: &MemoryQuery) -> Result<Vec<MemoryHit>, MemoryError> {
		let mut state = self
			.state
			.lock()
			.map_err(|error| MemoryError::Internal(error.to_string()))?;
		state.queries.push(query.clone());

		let normalized_query = query.query_text.trim().to_lowercase();
		let limit = query.limit.max(1);
		let hits = state
			.records
			.iter()
			.filter(|record| scope_matches(record, query))
			.filter(|record| kind_matches(record, &query.filters))
			.filter(|record| text_matches(record, &normalized_query))
			.take(limit)
			.map(|record| MemoryHit {
				record: record.clone(),
				score: score_record(record, &normalized_query),
				provenance: MemoryProvenance {
					backend: self.backend_name().to_string(),
					locator: Some(record.record_id.clone()),
					detail: None,
				},
			})
			.collect();
		Ok(hits)
	}

	fn write(&self, request: &MemoryWriteRequest) -> Result<MemoryWriteAck, MemoryError> {
		let mut state = self
			.state
			.lock()
			.map_err(|error| MemoryError::Internal(error.to_string()))?;
		state.writes.push(request.clone());
		state.next_record_id += 1;
		let timestamp = unix_ms_now();
		let record_id = format!("memory-record-{}", state.next_record_id);
		state.records.push(MemoryRecord {
			record_id: record_id.clone(),
			kind: request.kind,
			scope: request.scope,
			content: request.content.clone(),
			summary: request.summary.clone(),
			source_refs: request.source_refs.clone(),
			metadata: request.metadata.clone(),
			session_id: request.session_id.clone(),
			user_id: request.user_id.clone(),
			project_id: request.project_id.clone(),
			workspace_id: request.workspace_id.clone(),
			created_at_unix_ms: timestamp,
			updated_at_unix_ms: timestamp,
		});
		Ok(MemoryWriteAck {
			accepted: true,
			record_id: Some(record_id),
		})
	}

	fn delete(&self, selector: &MemoryDeleteSelector) -> Result<(), MemoryError> {
		let mut state = self
			.state
			.lock()
			.map_err(|error| MemoryError::Internal(error.to_string()))?;
		state.deletes.push(selector.clone());
		state
			.records
			.retain(|record| record.record_id != selector.record_id);
		Ok(())
	}

	fn health(&self) -> Result<MemoryBackendHealth, MemoryError> {
		Ok(MemoryBackendHealth {
			backend: self.backend_name().to_string(),
			status: MemoryBackendStatus::Healthy,
			detail: None,
		})
	}
}

fn scope_matches(record: &MemoryRecord, query: &MemoryQuery) -> bool {
	if record.scope != query.scope {
		return false;
	}

	match query.scope {
		MemoryScope::Session => match (&record.session_id, &query.session_id) {
			(Some(record_session), Some(query_session)) => record_session == query_session,
			_ => true,
		},
		MemoryScope::User => match (&record.user_id, &query.user_id) {
			(Some(record_user), Some(query_user)) => record_user == query_user,
			_ => true,
		},
		MemoryScope::Project => match (&record.project_id, &query.project_id) {
			(Some(record_project), Some(query_project)) => record_project == query_project,
			_ => true,
		},
		MemoryScope::Workspace => match (&record.workspace_id, &query.workspace_id) {
			(Some(record_workspace), Some(query_workspace)) => record_workspace == query_workspace,
			_ => true,
		},
		MemoryScope::Global => true,
	}
}

fn kind_matches(record: &MemoryRecord, filters: &MemoryFilters) -> bool {
	filters.kinds.is_empty() || filters.kinds.contains(&record.kind)
}

fn text_matches(record: &MemoryRecord, normalized_query: &str) -> bool {
	if normalized_query.is_empty() {
		return true;
	}

	let haystack = format!("{} {}", record.summary, record.content).to_lowercase();
	haystack.contains(normalized_query)
}

fn score_record(record: &MemoryRecord, normalized_query: &str) -> f32 {
	if normalized_query.is_empty() {
		return 0.5;
	}

	let haystack = format!("{} {}", record.summary, record.content).to_lowercase();
	if haystack == normalized_query {
		1.0
	} else if haystack.contains(normalized_query) {
		0.9
	} else {
		0.5
	}
}

fn unix_ms_now() -> u64 {
	SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.unwrap_or_default()
		.as_millis()
		.try_into()
		.unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
	use crate::{
		InMemoryLongTermMemoryBackend, LongTermMemoryBackend, MemoryKind, MemoryQuery,
		MemoryRecallReason, MemoryScope, MemoryWriteReason, MemoryWriteRequest,
		NoopLongTermMemoryBackend,
	};

	#[test]
	fn noop_backend_returns_empty_hits_and_non_persisting_ack() {
		let backend = NoopLongTermMemoryBackend;
		let query = MemoryQuery::new(
			"remember my coding style",
			MemoryRecallReason::RequestIntake,
			MemoryScope::Session,
		);
		let write = MemoryWriteRequest::new(
			MemoryKind::UserPreference,
			MemoryScope::Session,
			"Prefer concise explanations.",
			"User prefers concise explanations.",
			MemoryWriteReason::OperatorRequested,
		);

		let hits = backend.search(&query).expect("noop search should succeed");
		let ack = backend.write(&write).expect("noop write should succeed");

		assert!(hits.is_empty());
		assert!(!ack.accepted);
		assert_eq!(ack.record_id, None);
	}

	#[test]
	fn in_memory_backend_roundtrips_query_and_record() {
		let backend = InMemoryLongTermMemoryBackend::default();
		let mut write = MemoryWriteRequest::new(
			MemoryKind::UserPreference,
			MemoryScope::Session,
			"User prefers Rust examples.",
			"Rust preference",
			MemoryWriteReason::TaskSucceeded,
		);
		write.session_id = Some("session-1".to_string());

		let ack = backend.write(&write).expect("write should succeed");
		assert!(ack.accepted);

		let mut query = MemoryQuery::new(
			"rust examples",
			MemoryRecallReason::RequestIntake,
			MemoryScope::Session,
		);
		query.session_id = Some("session-1".to_string());
		query.limit = 3;
		let hits = backend.search(&query).expect("search should succeed");

		assert_eq!(backend.recorded_writes().len(), 1);
		assert_eq!(backend.recorded_queries().len(), 1);
		assert_eq!(hits.len(), 1);
		assert_eq!(hits[0].record.summary, "Rust preference");
	}
}
