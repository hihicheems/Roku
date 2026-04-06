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

//! Runtime-owned policy inputs and lifecycle decisions for long-term memory.
//!
//! Backends persist and retrieve records, but they do not decide when recall
//! should happen or what should be written back. Those decisions stay in runtime
//! policy, which consumes the inputs defined here and emits provider-neutral
//! [`MemoryQuery`] or [`MemoryWriteRequest`] values.

use roku_common_types::{ConversationTurn, ResponseStatus};
use serde::{Deserialize, Serialize};

use super::types::{
	MemoryHit, MemoryKind, MemoryQuery, MemoryRecallReason, MemoryScope, MemoryWriteReason,
	MemoryWriteRequest,
};

/// Decides when runtime should recall or persist long-term memory.
pub trait MemoryLifecyclePolicy: Send + Sync {
	/// Builds a recall query for the current runtime situation.
	///
	/// Returning `None` means recall should be skipped for this turn.
	fn build_recall_query(&self, input: &MemoryRecallInput) -> Option<MemoryQuery>;

	/// Builds a write-back request for the current runtime result.
	///
	/// Returning `None` means no long-term write should happen.
	fn build_write_request(&self, input: &MemoryWritePolicyInput) -> Option<MemoryWriteRequest>;
}

/// Runtime facts available when deciding whether to recall long-term memory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryRecallInput {
	/// Session receiving the request.
	pub session_id: String,
	/// Current user goal in natural language.
	pub goal: String,
	/// Whether runtime is resuming an already-active pending loop.
	pub pending_loop_active: bool,
	#[serde(default)]
	/// Recent conversation turns kept only for short-term continuity.
	pub short_term_continuity: Vec<ConversationTurn>,
	#[serde(default)]
	/// Optional user identity for wider-scope recall.
	pub user_id: Option<String>,
	#[serde(default)]
	/// Optional project identity for wider-scope recall.
	pub project_id: Option<String>,
	#[serde(default)]
	/// Optional workspace identity for wider-scope recall.
	pub workspace_id: Option<String>,
}

/// Runtime facts available when deciding whether to write back long-term memory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryWritePolicyInput {
	/// Request that produced the candidate memory.
	pub request_id: String,
	/// Session associated with the candidate memory.
	pub session_id: String,
	/// User goal that led to the response.
	pub goal: String,
	/// Final response status observed by runtime.
	pub response_status: ResponseStatus,
	/// Final response text observed by runtime.
	pub response_message: String,
	/// Whether the request ended with a still-pending loop.
	pub pending_loop_active: bool,
	#[serde(default)]
	/// Recent conversation turns retained for short-term continuity only.
	pub short_term_continuity: Vec<ConversationTurn>,
	#[serde(default)]
	/// Recall results that influenced this response, if any.
	pub recalled_hits: Vec<MemoryHit>,
	#[serde(default)]
	/// Optional user identity for wider-scope writes.
	pub user_id: Option<String>,
	#[serde(default)]
	/// Optional project identity for wider-scope writes.
	pub project_id: Option<String>,
	#[serde(default)]
	/// Optional workspace identity for wider-scope writes.
	pub workspace_id: Option<String>,
}

/// Conservative default policy used by Roku during the initial rollout.
///
/// It allows intake-time recall for ordinary requests but intentionally keeps
/// automatic write-back disabled until runtime has stronger extraction rules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConservativeMemoryLifecyclePolicy {
	/// Maximum number of hits to request when recall is enabled.
	pub recall_limit: usize,
	/// Whether automatic write-back is enabled for this runtime wiring.
	pub automatic_write_back: bool,
}

impl Default for ConservativeMemoryLifecyclePolicy {
	fn default() -> Self {
		Self {
			recall_limit: 3,
			automatic_write_back: false,
		}
	}
}

impl MemoryLifecyclePolicy for ConservativeMemoryLifecyclePolicy {
	fn build_recall_query(&self, input: &MemoryRecallInput) -> Option<MemoryQuery> {
		let query_text = input.goal.trim();
		if query_text.is_empty() || input.pending_loop_active {
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

	fn build_write_request(&self, input: &MemoryWritePolicyInput) -> Option<MemoryWriteRequest> {
		if !self.automatic_write_back
			|| input.response_status != ResponseStatus::Succeeded
			|| input.pending_loop_active
		{
			return None;
		}

		let scope = if input.workspace_id.is_some() {
			MemoryScope::Workspace
		} else {
			MemoryScope::Session
		};
		let mut request = MemoryWriteRequest::new(
			MemoryKind::HistoricalCase,
			scope,
			format!("goal={} response={}", input.goal, input.response_message),
			"Successful runtime response".to_string(),
			MemoryWriteReason::TaskSucceeded,
		);
		request.session_id = Some(input.session_id.clone());
		request.user_id = input.user_id.clone();
		request.project_id = input.project_id.clone();
		request.workspace_id = input.workspace_id.clone();
		Some(request)
	}
}

#[cfg(test)]
mod tests {
	use crate::{
		ConservativeMemoryLifecyclePolicy, MemoryLifecyclePolicy, MemoryRecallInput, MemoryScope,
		MemoryWritePolicyInput, MemoryWriteReason,
	};
	use roku_common_types::{ConversationRole, ConversationTurn, ResponseStatus};

	#[test]
	fn conservative_policy_builds_session_scoped_recall_query() {
		let policy = ConservativeMemoryLifecyclePolicy::default();
		let input = MemoryRecallInput {
			session_id: "session-1".to_string(),
			goal: "Remember my preferred coding language".to_string(),
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

	#[test]
	fn conservative_policy_can_enable_write_back_from_runtime_wiring() {
		let policy = ConservativeMemoryLifecyclePolicy {
			recall_limit: 4,
			automatic_write_back: true,
		};
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

		let request = policy
			.build_write_request(&input)
			.expect("write-back should be enabled");

		assert_eq!(request.scope, MemoryScope::Session);
		assert_eq!(request.write_reason, MemoryWriteReason::TaskSucceeded);
		assert_eq!(request.session_id.as_deref(), Some("session-1"));
	}
}
