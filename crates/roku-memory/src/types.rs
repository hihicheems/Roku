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

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub type MemoryMetadata = BTreeMap<String, String>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKind {
	UserPreference,
	UserFact,
	ProjectFact,
	WorkspaceFact,
	HistoricalCase,
	Constraint,
	WorkflowInsight,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryScope {
	Session,
	User,
	Project,
	Workspace,
	Global,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryRecallReason {
	RequestIntake,
	Manual,
	Resume,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryWriteReason {
	TaskSucceeded,
	HighValueObservation,
	OperatorRequested,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MemoryFilters {
	#[serde(default)]
	pub kinds: Vec<MemoryKind>,
	#[serde(default)]
	pub tags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MemorySourceRef {
	pub kind: String,
	pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryQuery {
	pub query_text: String,
	pub recall_reason: MemoryRecallReason,
	pub scope: MemoryScope,
	pub limit: usize,
	#[serde(default)]
	pub filters: MemoryFilters,
	#[serde(default)]
	pub session_id: Option<String>,
	#[serde(default)]
	pub user_id: Option<String>,
	#[serde(default)]
	pub project_id: Option<String>,
	#[serde(default)]
	pub workspace_id: Option<String>,
}

impl MemoryQuery {
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryRecord {
	pub record_id: String,
	pub kind: MemoryKind,
	pub scope: MemoryScope,
	pub content: String,
	pub summary: String,
	#[serde(default)]
	pub source_refs: Vec<MemorySourceRef>,
	#[serde(default)]
	pub metadata: MemoryMetadata,
	#[serde(default)]
	pub session_id: Option<String>,
	#[serde(default)]
	pub user_id: Option<String>,
	#[serde(default)]
	pub project_id: Option<String>,
	#[serde(default)]
	pub workspace_id: Option<String>,
	pub created_at_unix_ms: u64,
	pub updated_at_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MemoryProvenance {
	pub backend: String,
	#[serde(default)]
	pub locator: Option<String>,
	#[serde(default)]
	pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryHit {
	pub record: MemoryRecord,
	pub score: f32,
	#[serde(default)]
	pub provenance: MemoryProvenance,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryWriteRequest {
	pub kind: MemoryKind,
	pub scope: MemoryScope,
	pub content: String,
	pub summary: String,
	pub write_reason: MemoryWriteReason,
	#[serde(default)]
	pub source_refs: Vec<MemorySourceRef>,
	#[serde(default)]
	pub metadata: MemoryMetadata,
	#[serde(default)]
	pub session_id: Option<String>,
	#[serde(default)]
	pub user_id: Option<String>,
	#[serde(default)]
	pub project_id: Option<String>,
	#[serde(default)]
	pub workspace_id: Option<String>,
}

impl MemoryWriteRequest {
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
