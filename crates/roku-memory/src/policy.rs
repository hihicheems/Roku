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

use roku_common_types::{ConversationTurn, ResponseStatus};
use serde::{Deserialize, Serialize};

use crate::types::{MemoryHit, MemoryQuery, MemoryRecallReason, MemoryScope, MemoryWriteRequest};

pub trait MemoryLifecyclePolicy: Send + Sync {
	fn build_recall_query(&self, input: &MemoryRecallInput) -> Option<MemoryQuery>;

	fn build_write_request(&self, input: &MemoryWritePolicyInput) -> Option<MemoryWriteRequest>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryRecallInput {
	pub session_id: String,
	pub goal: String,
	pub planning_mode_hint_present: bool,
	pub pending_loop_active: bool,
	#[serde(default)]
	pub short_term_continuity: Vec<ConversationTurn>,
	#[serde(default)]
	pub user_id: Option<String>,
	#[serde(default)]
	pub project_id: Option<String>,
	#[serde(default)]
	pub workspace_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryWritePolicyInput {
	pub request_id: String,
	pub session_id: String,
	pub goal: String,
	pub response_status: ResponseStatus,
	pub response_message: String,
	pub pending_loop_active: bool,
	#[serde(default)]
	pub short_term_continuity: Vec<ConversationTurn>,
	#[serde(default)]
	pub recalled_hits: Vec<MemoryHit>,
	#[serde(default)]
	pub user_id: Option<String>,
	#[serde(default)]
	pub project_id: Option<String>,
	#[serde(default)]
	pub workspace_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConservativeMemoryLifecyclePolicy {
	pub recall_limit: usize,
}

impl Default for ConservativeMemoryLifecyclePolicy {
	fn default() -> Self {
		Self { recall_limit: 3 }
	}
}

impl MemoryLifecyclePolicy for ConservativeMemoryLifecyclePolicy {
	fn build_recall_query(&self, input: &MemoryRecallInput) -> Option<MemoryQuery> {
		let query_text = input.goal.trim();
		if query_text.is_empty() || input.planning_mode_hint_present || input.pending_loop_active {
			return None;
		}

		let scope = if input.workspace_id.is_some() {
			MemoryScope::Workspace
		} else {
			MemoryScope::Session
		};
		let mut query = MemoryQuery::new(query_text, MemoryRecallReason::RequestIntake, scope);
		query.limit = self.recall_limit.max(1);
		query.session_id = Some(input.session_id.clone());
		query.user_id = input.user_id.clone();
		query.project_id = input.project_id.clone();
		query.workspace_id = input.workspace_id.clone();
		Some(query)
	}

	fn build_write_request(&self, _input: &MemoryWritePolicyInput) -> Option<MemoryWriteRequest> {
		None
	}
}

#[cfg(test)]
mod tests {
	use crate::{
		ConservativeMemoryLifecyclePolicy, MemoryLifecyclePolicy, MemoryRecallInput, MemoryScope,
		MemoryWritePolicyInput,
	};
	use roku_common_types::{ConversationRole, ConversationTurn, ResponseStatus};

	#[test]
	fn conservative_policy_builds_session_scoped_recall_query() {
		let policy = ConservativeMemoryLifecyclePolicy::default();
		let input = MemoryRecallInput {
			session_id: "session-1".to_string(),
			goal: "Remember my preferred coding language".to_string(),
			planning_mode_hint_present: false,
			pending_loop_active: false,
			short_term_continuity: vec![ConversationTurn {
				role: ConversationRole::User,
				content: "Prefer Rust snippets.".to_string(),
				created_at_unix_ms: 0,
			}],
			user_id: None,
			project_id: None,
			workspace_id: None,
		};

		let query = policy
			.build_recall_query(&input)
			.expect("policy should build recall query");

		assert_eq!(query.scope, MemoryScope::Session);
		assert_eq!(query.session_id.as_deref(), Some("session-1"));
		assert_eq!(query.limit, 3);
	}

	#[test]
	fn conservative_policy_skips_default_write_back() {
		let policy = ConservativeMemoryLifecyclePolicy::default();
		let input = MemoryWritePolicyInput {
			request_id: "req-1".to_string(),
			session_id: "session-1".to_string(),
			goal: "Summarize the repo".to_string(),
			response_status: ResponseStatus::Succeeded,
			response_message: "Done".to_string(),
			pending_loop_active: false,
			short_term_continuity: Vec::new(),
			recalled_hits: Vec::new(),
			user_id: None,
			project_id: None,
			workspace_id: None,
		};

		assert!(policy.build_write_request(&input).is_none());
	}
}
