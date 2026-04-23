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

use super::backend::{LongTermMemoryBackend, MemoryError};
use super::types::{
	MemoryFilters, MemoryKind, MemoryQuery, MemoryRecallReason, MemoryRecord, MemoryScope,
};

/// Default limit when fetching session compact summaries. The runtime only
/// uses the most recent record, but a small over-fetch lets backends that
/// can't sort by recency still return the correct candidate as long as the
/// truth is among the few most recent workflow insights.
const DEFAULT_FETCH_LIMIT: usize = 8;

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
/// The helper currently filters by `MemoryKind::WorkflowInsight` because
/// that is the kind `write_back_compact_summaries` writes. If the write-back
/// path ever diversifies, extend `MemoryFilters::kinds` in lockstep.
pub fn latest_session_compact_summary(
	backend: &dyn LongTermMemoryBackend,
	session_id: &str,
) -> Result<Option<MemoryRecord>, MemoryError> {
	if session_id.is_empty() {
		return Ok(None);
	}
	let query = session_compact_summary_query(session_id);
	let hits = backend.search(&query)?;
	let latest = hits
		.into_iter()
		.map(|hit| hit.record)
		.max_by_key(|record| record.created_at_unix_ms);
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
			"Context compact summary",
			MemoryWriteReason::CompactSummary,
		);
		req.session_id = Some(session_id.to_string());
		backend.write(&req).expect("seed summary");
		for (other_session, other_content) in extra {
			let mut extra_req = MemoryWriteRequest::new(
				MemoryKind::WorkflowInsight,
				MemoryScope::Session,
				other_content,
				"Context compact summary",
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
	fn canonical_query_carries_session_id_and_workflow_kind() {
		let query = session_compact_summary_query("session-1");
		assert_eq!(query.scope, MemoryScope::Session);
		assert_eq!(query.session_id.as_deref(), Some("session-1"));
		assert_eq!(query.filters.kinds, vec![MemoryKind::WorkflowInsight]);
		assert!(query.query_text.is_empty());
		assert!(query.limit >= 1);
	}
}
