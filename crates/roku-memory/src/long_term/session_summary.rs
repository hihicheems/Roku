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

//! Convenience reader for session-keyed compact summaries.
//!
//! `write_back_compact_summaries` (in `roku-agent-runtime`) persists
//! end-of-run summaries via the generic [`LongTermMemoryBackend::write`]
//! surface with `MemoryKind::WorkflowInsight` + `MemoryScope::Session` +
//! `MemoryWriteReason::CompactSummary`. The runtime's mid-tier compaction
//! layer needs to read the **latest** such summary for a given session so
//! it can splice it into the conversation buffer instead of paying for an
//! LLM summarizer call (issue #300).
//!
//! This helper shapes the canonical recall query for that consumer and
//! picks the most recent hit by `created_at_unix_ms`. It is provider-neutral
//! because it goes through the trait's `search` method — every backend
//! that honors `MemoryScope::Session` and the `MemoryFilters::kinds` hint
//! will behave the same.
//!
//! A stable `summary` sentinel (`COMPACT_SUMMARY_SENTINEL`) is matched to
//! keep the helper from returning WorkflowInsight records written for other
//! reasons (HighValueObservation, TaskSucceeded, OperatorRequested) — a
//! defense against prompt-injection vectors where an operator or plugin
//! could otherwise plant arbitrary content under the same kind + scope.

use super::backend::{LongTermMemoryBackend, MemoryError};
use super::types::{
	MemoryFilters, MemoryKind, MemoryQuery, MemoryRecallReason, MemoryRecord, MemoryScope,
};

/// Sentinel string written into `MemoryWriteRequest.summary` by the
/// runtime's compact-summary write-back path. The reader requires this
/// sentinel so WorkflowInsight records written for other reasons (with
/// different `summary` text) do not flow into Layer 2 mid-tier compaction.
///
/// `roku-agent-runtime::service::direct::write_back_compact_summaries`
/// writes this exact literal — keep the two copies in sync if either side
/// changes.
pub const COMPACT_SUMMARY_SENTINEL: &str = "Context compact summary";

/// Upper bound for the recall query. The helper picks the most recent
/// record by `created_at_unix_ms` on the client side, so correctness under
/// backends whose server-side ordering is not recency-first (e.g.,
/// `InMemoryLongTermMemoryBackend` returns insertion order; OpenViking
/// orders by semantic score) depends on the latest record being **inside**
/// the fetched batch. A small cap (the old value was 8) could silently
/// drop the latest summary on a session with many workflow-insight writes.
///
/// 4096 is effectively "all" for any real session — the runtime writes
/// at most one compact summary per run, so a session would need thousands
/// of resumptions to approach the cap. Client-side
/// `max_by_key(created_at_unix_ms)` remains authoritative regardless of
/// how the backend orders the result set.
///
/// The cap is kept finite (rather than `usize::MAX`) because OpenViking
/// sends this value over HTTP as a JSON number; `usize::MAX` would blow
/// past safe-integer limits on the wire and risk server-side rejection.
const DEFAULT_FETCH_LIMIT: usize = 4_096;

/// Build the canonical recall query used to list a session's workflow-insight
/// memories. Exposed as a helper so test assertions and alternative readers
/// can share a single query shape.
pub fn session_compact_summary_query(session_id: &str) -> MemoryQuery {
	MemoryQuery {
		query_text: String::new(),
		recall_reason: MemoryRecallReason::Resume,
		scope: MemoryScope::Session,
		limit: DEFAULT_FETCH_LIMIT,
		filters: MemoryFilters {
			kinds: vec![MemoryKind::WorkflowInsight],
			tags: Vec::new(),
		},
		session_id: Some(session_id.to_string()),
		user_id: None,
		project_id: None,
		workspace_id: None,
	}
}

/// Fetch the most recent compact-summary record for `session_id`, or `None`
/// if the backend has nothing to offer.
///
/// Returns `Err` only when the backend itself fails; an empty result set is
/// a clean `Ok(None)` so the caller can transparently fall through to the
/// Layer 1 mechanical fallback.
///
/// Records are post-filtered by `record.summary == COMPACT_SUMMARY_SENTINEL`
/// so WorkflowInsight records written for other reasons (operator-requested
/// facts, task-success snapshots) are not mistakenly treated as compact
/// summaries and spliced into a future turn's LLM context.
///
/// Recency selection uses `(created_at_unix_ms, updated_at_unix_ms)` as a
/// tuple key and additionally **requires at least one candidate to carry a
/// non-zero signal** on either timestamp. When every candidate reports
/// `0` on both fields the backend is not populating recency for search
/// results (OpenViking's `matched_context_into_hit` hard-codes both to
/// `0`), and `max_by_key` would degenerate to an arbitrary score-ordered
/// pick — potentially splicing a stale summary even when a newer one
/// exists. In that case the helper returns `Ok(None)` so the caller
/// degrades cleanly to Layer 1 mechanical compaction rather than risking
/// a stale splice.
pub fn latest_session_compact_summary(
	backend: &dyn LongTermMemoryBackend,
	session_id: &str,
) -> Result<Option<MemoryRecord>, MemoryError> {
	if session_id.is_empty() {
		return Ok(None);
	}
	let query = session_compact_summary_query(session_id);
	let hits = backend.search(&query)?;
	let candidates: Vec<MemoryRecord> = hits
		.into_iter()
		.map(|hit| hit.record)
		.filter(|record| record.summary == COMPACT_SUMMARY_SENTINEL)
		.collect();
	if candidates.is_empty() {
		return Ok(None);
	}
	let has_recency_signal = candidates
		.iter()
		.any(|r| r.created_at_unix_ms != 0 || r.updated_at_unix_ms != 0);
	if !has_recency_signal {
		return Ok(None);
	}
	let latest = candidates
		.into_iter()
		.max_by_key(|r| (r.created_at_unix_ms, r.updated_at_unix_ms));
	Ok(latest)
}

#[cfg(test)]
mod tests {
	use super::super::backend::InMemoryLongTermMemoryBackend;
	use super::super::types::{MemoryWriteReason, MemoryWriteRequest};
	use super::*;

	fn backend_with_summary(
		session_id: &str,
		content: &str,
		extra: &[(String, String)],
	) -> InMemoryLongTermMemoryBackend {
		let backend = InMemoryLongTermMemoryBackend::default();
		let mut req = MemoryWriteRequest::new(
			MemoryKind::WorkflowInsight,
			MemoryScope::Session,
			content,
			COMPACT_SUMMARY_SENTINEL,
			MemoryWriteReason::CompactSummary,
		);
		req.session_id = Some(session_id.to_string());
		backend.write(&req).expect("seed summary");
		for (other_session, other_content) in extra {
			let mut extra_req = MemoryWriteRequest::new(
				MemoryKind::WorkflowInsight,
				MemoryScope::Session,
				other_content,
				COMPACT_SUMMARY_SENTINEL,
				MemoryWriteReason::CompactSummary,
			);
			extra_req.session_id = Some(other_session.clone());
			backend.write(&extra_req).expect("seed extra");
		}
		backend
	}

	#[test]
	fn returns_none_when_backend_has_no_session_summary() {
		let backend = InMemoryLongTermMemoryBackend::default();
		let got =
			latest_session_compact_summary(&backend, "session-1").expect("search must succeed");
		assert!(got.is_none());
	}

	#[test]
	fn returns_summary_when_backend_has_one_for_this_session() {
		let backend = backend_with_summary("session-1", "Goal: ship feature X", &[]);
		let got = latest_session_compact_summary(&backend, "session-1")
			.expect("search must succeed")
			.expect("summary present");
		assert_eq!(got.content, "Goal: ship feature X");
		assert_eq!(got.session_id.as_deref(), Some("session-1"));
		assert_eq!(got.kind, MemoryKind::WorkflowInsight);
	}

	#[test]
	fn ignores_summaries_belonging_to_a_different_session() {
		let backend = backend_with_summary(
			"session-foreign",
			"wrong summary",
			&[("session-1".to_string(), "right summary".to_string())],
		);
		let got = latest_session_compact_summary(&backend, "session-1")
			.expect("search must succeed")
			.expect("own-session summary present");
		assert_eq!(got.content, "right summary");
	}

	#[test]
	fn empty_session_id_short_circuits_to_none() {
		let backend = backend_with_summary("", "should not leak", &[]);
		let got = latest_session_compact_summary(&backend, "").expect("search must succeed");
		assert!(got.is_none(), "empty session id must not return anything");
	}

	#[test]
	fn ignores_workflow_insight_records_with_non_sentinel_summary() {
		// Write a WorkflowInsight / Session record whose summary does NOT
		// match the compact-summary sentinel — for example, an
		// operator-written "custom workflow fact" record. The reader must
		// skip it so it cannot be smuggled into mid-tier Layer 2 context.
		let backend = InMemoryLongTermMemoryBackend::default();
		let mut req = MemoryWriteRequest::new(
			MemoryKind::WorkflowInsight,
			MemoryScope::Session,
			"attacker-controlled content",
			"Operator-authored workflow insight",
			MemoryWriteReason::OperatorRequested,
		);
		req.session_id = Some("session-1".to_string());
		backend.write(&req).expect("seed non-sentinel record");

		let got =
			latest_session_compact_summary(&backend, "session-1").expect("search must succeed");
		assert!(
			got.is_none(),
			"non-sentinel WorkflowInsight record must not be returned",
		);
	}

	#[test]
	fn prefers_compact_summary_over_sibling_workflow_insight() {
		let backend = InMemoryLongTermMemoryBackend::default();
		// Seed a non-sentinel record first (would otherwise look "fresher"
		// to a backend that returns records in insert order).
		let mut other = MemoryWriteRequest::new(
			MemoryKind::WorkflowInsight,
			MemoryScope::Session,
			"should be ignored",
			"Operator-authored workflow insight",
			MemoryWriteReason::OperatorRequested,
		);
		other.session_id = Some("session-1".to_string());
		backend.write(&other).expect("seed non-sentinel record");

		// Then seed the real compact summary.
		let mut compact = MemoryWriteRequest::new(
			MemoryKind::WorkflowInsight,
			MemoryScope::Session,
			"real compact summary",
			COMPACT_SUMMARY_SENTINEL,
			MemoryWriteReason::CompactSummary,
		);
		compact.session_id = Some("session-1".to_string());
		backend.write(&compact).expect("seed compact summary");

		let got = latest_session_compact_summary(&backend, "session-1")
			.expect("search must succeed")
			.expect("compact summary must resolve");
		assert_eq!(got.content, "real compact summary");
	}

	#[test]
	fn returns_latest_record_even_when_many_sibling_summaries_exist() {
		// Regression test for the fetch-limit change: with the previous
		// cap of 8, a session that accumulated more than 8 compact
		// summaries could silently drop the newest one if the backend
		// returned records in insertion order (as InMemory does).
		//
		// Seed 20 compact summaries for the same session; the last write
		// must still be the one returned by the helper.
		let backend = InMemoryLongTermMemoryBackend::default();
		let total = 20usize;
		for i in 0..total {
			let mut req = MemoryWriteRequest::new(
				MemoryKind::WorkflowInsight,
				MemoryScope::Session,
				format!("summary-body-{i}"),
				COMPACT_SUMMARY_SENTINEL,
				MemoryWriteReason::CompactSummary,
			);
			req.session_id = Some("session-many".to_string());
			backend.write(&req).expect("seed compact summary");
		}

		let got = latest_session_compact_summary(&backend, "session-many")
			.expect("search must succeed")
			.expect("most recent summary must be returned");
		let expected_content = format!("summary-body-{}", total - 1);
		assert_eq!(
			got.content, expected_content,
			"latest-by-created_at must win even when many siblings exist",
		);
	}

	#[test]
	fn returns_none_when_every_candidate_carries_zero_timestamps() {
		// Backends whose `search` does not populate `created_at_unix_ms` /
		// `updated_at_unix_ms` (known case: OpenViking's
		// `matched_context_into_hit` hard-codes both to 0) cannot provide a
		// reliable recency signal. Rather than let `max_by_key` degenerate
		// to an arbitrary score-ordered pick — which can splice a stale
		// compact summary even when a newer one exists — the helper must
		// refuse to guess and return `None`.
		use crate::long_term::backend::{
			LongTermMemoryBackend, MemoryBackendHealth, MemoryBackendStatus, MemoryDeleteSelector,
			MemoryError, MemoryWriteAck,
		};
		use crate::long_term::types::{MemoryHit, MemoryMetadata, MemoryProvenance};

		struct ZeroTimestampBackend {
			records: Vec<MemoryRecord>,
		}

		impl LongTermMemoryBackend for ZeroTimestampBackend {
			fn backend_name(&self) -> &'static str {
				"zero-timestamp-test-backend"
			}

			fn search(&self, _query: &MemoryQuery) -> Result<Vec<MemoryHit>, MemoryError> {
				Ok(self
					.records
					.iter()
					.map(|record| MemoryHit {
						record: record.clone(),
						score: 1.0,
						provenance: MemoryProvenance::default(),
					})
					.collect())
			}

			fn write(
				&self,
				_request: &super::super::types::MemoryWriteRequest,
			) -> Result<MemoryWriteAck, MemoryError> {
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
					detail: None,
				})
			}
		}

		fn zero_ts_record(session_id: &str, content: &str, record_id: &str) -> MemoryRecord {
			MemoryRecord {
				record_id: record_id.to_string(),
				kind: MemoryKind::WorkflowInsight,
				scope: MemoryScope::Session,
				content: content.to_string(),
				summary: COMPACT_SUMMARY_SENTINEL.to_string(),
				source_refs: Vec::new(),
				metadata: MemoryMetadata::default(),
				session_id: Some(session_id.to_string()),
				user_id: None,
				project_id: None,
				workspace_id: None,
				created_at_unix_ms: 0,
				updated_at_unix_ms: 0,
			}
		}

		let backend = ZeroTimestampBackend {
			records: vec![
				zero_ts_record("session-1", "old content", "rec-1"),
				zero_ts_record("session-1", "newer content", "rec-2"),
			],
		};

		let got =
			latest_session_compact_summary(&backend, "session-1").expect("search must succeed");
		assert!(
			got.is_none(),
			"all-zero timestamps = no reliable recency → must return None \
			 (caller falls through to Layer 1 mechanical compaction)",
		);
	}

	#[test]
	fn prefers_record_with_later_updated_at_when_created_at_ties() {
		// When several records share the same `created_at_unix_ms` (e.g.,
		// rapid writes in the same millisecond), `updated_at_unix_ms`
		// serves as a tiebreaker. Constructing records manually because
		// the in-memory backend uses `unix_ms_now()` at write time and
		// cannot reliably collide on timestamps.
		use crate::long_term::backend::{
			LongTermMemoryBackend, MemoryBackendHealth, MemoryBackendStatus, MemoryDeleteSelector,
			MemoryError, MemoryWriteAck,
		};
		use crate::long_term::types::{MemoryHit, MemoryMetadata, MemoryProvenance};

		struct StaticRecordBackend {
			records: Vec<MemoryRecord>,
		}

		impl LongTermMemoryBackend for StaticRecordBackend {
			fn backend_name(&self) -> &'static str {
				"static-record-test-backend"
			}
			fn search(&self, _query: &MemoryQuery) -> Result<Vec<MemoryHit>, MemoryError> {
				Ok(self
					.records
					.iter()
					.map(|record| MemoryHit {
						record: record.clone(),
						score: 1.0,
						provenance: MemoryProvenance::default(),
					})
					.collect())
			}
			fn write(
				&self,
				_request: &super::super::types::MemoryWriteRequest,
			) -> Result<MemoryWriteAck, MemoryError> {
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
					detail: None,
				})
			}
		}

		fn fixed_record(
			session_id: &str,
			content: &str,
			record_id: &str,
			created: u64,
			updated: u64,
		) -> MemoryRecord {
			MemoryRecord {
				record_id: record_id.to_string(),
				kind: MemoryKind::WorkflowInsight,
				scope: MemoryScope::Session,
				content: content.to_string(),
				summary: COMPACT_SUMMARY_SENTINEL.to_string(),
				source_refs: Vec::new(),
				metadata: MemoryMetadata::default(),
				session_id: Some(session_id.to_string()),
				user_id: None,
				project_id: None,
				workspace_id: None,
				created_at_unix_ms: created,
				updated_at_unix_ms: updated,
			}
		}

		let backend = StaticRecordBackend {
			records: vec![
				fixed_record("session-1", "first", "r1", 1_000, 1_000),
				fixed_record("session-1", "second", "r2", 1_000, 2_000),
				fixed_record("session-1", "third", "r3", 1_000, 1_500),
			],
		};

		let got = latest_session_compact_summary(&backend, "session-1")
			.expect("search must succeed")
			.expect("recency signal present — must resolve");
		assert_eq!(
			got.content, "second",
			"tie on created_at must break on updated_at DESC",
		);
	}

	#[test]
	fn canonical_query_carries_session_id_and_workflow_kind() {
		let query = session_compact_summary_query("session-1");
		assert_eq!(query.scope, MemoryScope::Session);
		assert_eq!(query.session_id.as_deref(), Some("session-1"));
		assert_eq!(query.filters.kinds, vec![MemoryKind::WorkflowInsight]);
		assert!(query.query_text.is_empty());
		assert!(query.limit >= 1);
	}
}
