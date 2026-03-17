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

//! Provider-neutral long-term memory domain types.
//!
//! These types are shared across runtime, policy, and backend adapters. They carry
//! Roku's own semantics for recall scope, record shape, provenance, and write-back
//! intent, independent of any specific storage provider.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Structured metadata attached to a memory record or write request.
///
/// Keys are provider-neutral strings chosen by Roku runtime or policy code.
pub type MemoryMetadata = BTreeMap<String, String>;

/// Semantic category assigned to a long-term memory record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKind {
	/// A durable user preference that can influence future responses.
	UserPreference,
	/// A durable fact about the user.
	UserFact,
	/// A project-scoped fact that is useful across multiple requests.
	ProjectFact,
	/// A workspace-wide fact or convention.
	WorkspaceFact,
	/// A past case or example that may help with future work.
	HistoricalCase,
	/// A constraint that should shape future execution or responses.
	Constraint,
	/// A reusable workflow insight or operating pattern.
	WorkflowInsight,
}

/// Visibility boundary that controls which identity fields must match during recall.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryScope {
	/// Visible only within a single session.
	Session,
	/// Visible across sessions for the same user.
	User,
	/// Visible across work attached to the same project.
	Project,
	/// Visible across the current workspace.
	Workspace,
	/// Visible globally, without scope-specific identity qualifiers.
	Global,
}

/// Why runtime is attempting to recall long-term memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryRecallReason {
	/// Recall triggered during initial request intake.
	RequestIntake,
	/// Recall explicitly requested by an operator or caller.
	Manual,
	/// Recall triggered while resuming an existing runtime loop.
	Resume,
}

/// Why runtime is attempting to persist a long-term memory record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryWriteReason {
	/// A completed task produced something durable enough to retain.
	TaskSucceeded,
	/// Runtime observed a high-value fact worth retaining independent of task success.
	HighValueObservation,
	/// The operator explicitly requested persistence.
	OperatorRequested,
}

/// Optional narrowing hints applied to a recall query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MemoryFilters {
	/// Restricts results to these semantic kinds when the backend supports kind filtering.
	#[serde(default)]
	pub kinds: Vec<MemoryKind>,
	/// Optional tag hints.
	///
	/// First-party backends may ignore tags when they do not index them yet, so
	/// callers should treat this as best-effort narrowing rather than a hard
	/// contract.
	#[serde(default)]
	pub tags: Vec<String>,
}

/// Reference back to the source material that justified a memory record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MemorySourceRef {
	/// Source category such as `request_id`, `artifact`, or `provider_locator`.
	pub kind: String,
	/// Opaque source identifier within the chosen [`MemorySourceRef::kind`].
	pub value: String,
}

/// Provider-neutral recall request built by runtime policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryQuery {
	/// Natural-language recall prompt supplied to the backend.
	pub query_text: String,
	/// Why this recall is happening.
	pub recall_reason: MemoryRecallReason,
	/// Scope boundary the backend must honor.
	pub scope: MemoryScope,
	/// Maximum number of hits the caller wants back.
	pub limit: usize,
	#[serde(default)]
	/// Optional narrowing hints for the backend.
	pub filters: MemoryFilters,
	#[serde(default)]
	/// Session identity required for [`MemoryScope::Session`] queries.
	pub session_id: Option<String>,
	#[serde(default)]
	/// User identity required for [`MemoryScope::User`] queries.
	pub user_id: Option<String>,
	#[serde(default)]
	/// Project identity required for [`MemoryScope::Project`] queries.
	pub project_id: Option<String>,
	#[serde(default)]
	/// Workspace identity required for [`MemoryScope::Workspace`] queries.
	pub workspace_id: Option<String>,
}

impl MemoryQuery {
	/// Builds a minimally configured recall query.
	///
	/// The returned query defaults to `limit = 5` and leaves all scope identity
	/// fields unset. Callers must populate the identity field required by the chosen
	/// [`MemoryScope`] before handing the query to a backend.
	pub fn new(
		query_text: impl Into<String>,
		recall_reason: MemoryRecallReason,
		scope: MemoryScope,
	) -> Self {
		Self {
			query_text: query_text.into(),
			recall_reason,
			scope,
			limit: 5,
			filters: MemoryFilters::default(),
			session_id: None,
			user_id: None,
			project_id: None,
			workspace_id: None,
		}
	}
}

/// Canonical long-term memory record returned by a backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryRecord {
	/// Opaque backend record identifier.
	pub record_id: String,
	/// Semantic category assigned by Roku policy.
	pub kind: MemoryKind,
	/// Recall visibility boundary for this record.
	pub scope: MemoryScope,
	/// Full persisted content for the record.
	pub content: String,
	/// Concise summary suitable for recall results and prompt injection.
	pub summary: String,
	#[serde(default)]
	/// Source references that explain where the record came from.
	pub source_refs: Vec<MemorySourceRef>,
	#[serde(default)]
	/// Provider-neutral metadata recorded alongside the memory.
	pub metadata: MemoryMetadata,
	#[serde(default)]
	/// Session identity when [`MemoryRecord::scope`] is [`MemoryScope::Session`].
	pub session_id: Option<String>,
	#[serde(default)]
	/// User identity when [`MemoryRecord::scope`] is [`MemoryScope::User`].
	pub user_id: Option<String>,
	#[serde(default)]
	/// Project identity when [`MemoryRecord::scope`] is [`MemoryScope::Project`].
	pub project_id: Option<String>,
	#[serde(default)]
	/// Workspace identity when [`MemoryRecord::scope`] is [`MemoryScope::Workspace`].
	pub workspace_id: Option<String>,
	/// Record creation time in Unix milliseconds.
	pub created_at_unix_ms: u64,
	/// Last update time in Unix milliseconds.
	pub updated_at_unix_ms: u64,
}

/// Backend-specific provenance for a recalled memory hit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MemoryProvenance {
	/// Backend name that produced the hit.
	pub backend: String,
	#[serde(default)]
	/// Optional provider-local locator for the record or sub-resource.
	pub locator: Option<String>,
	#[serde(default)]
	/// Optional backend-specific explanation, such as a match reason.
	pub detail: Option<String>,
}

/// A recalled memory record together with backend scoring metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryHit {
	/// The canonical record projected into Roku's memory schema.
	pub record: MemoryRecord,
	/// Backend-local score for ranking within the current result set.
	pub score: f32,
	#[serde(default)]
	/// Provider provenance retained for diagnostics and auditing.
	pub provenance: MemoryProvenance,
}

/// Provider-neutral write request produced by runtime policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryWriteRequest {
	/// Semantic category for the new record.
	pub kind: MemoryKind,
	/// Visibility boundary for the new record.
	pub scope: MemoryScope,
	/// Full content to persist.
	pub content: String,
	/// Concise summary used for recall and prompt assembly.
	pub summary: String,
	/// Why runtime chose to persist this record.
	pub write_reason: MemoryWriteReason,
	#[serde(default)]
	/// References to the source material that justified the write.
	pub source_refs: Vec<MemorySourceRef>,
	#[serde(default)]
	/// Provider-neutral metadata recorded alongside the write.
	pub metadata: MemoryMetadata,
	#[serde(default)]
	/// Session identity required for [`MemoryScope::Session`] writes.
	pub session_id: Option<String>,
	#[serde(default)]
	/// User identity required for [`MemoryScope::User`] writes.
	pub user_id: Option<String>,
	#[serde(default)]
	/// Project identity required for [`MemoryScope::Project`] writes.
	pub project_id: Option<String>,
	#[serde(default)]
	/// Workspace identity required for [`MemoryScope::Workspace`] writes.
	pub workspace_id: Option<String>,
}

impl MemoryWriteRequest {
	/// Builds a minimally configured write request.
	///
	/// The returned request leaves scope identity fields unset. Callers must fill in
	/// the identifier required by the chosen [`MemoryScope`] before sending it to a
	/// backend.
	pub fn new(
		kind: MemoryKind,
		scope: MemoryScope,
		content: impl Into<String>,
		summary: impl Into<String>,
		write_reason: MemoryWriteReason,
	) -> Self {
		Self {
			kind,
			scope,
			content: content.into(),
			summary: summary.into(),
			write_reason,
			source_refs: Vec::new(),
			metadata: MemoryMetadata::default(),
			session_id: None,
			user_id: None,
			project_id: None,
			workspace_id: None,
		}
	}
}
