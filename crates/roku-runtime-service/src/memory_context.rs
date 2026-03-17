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

use std::sync::Arc;

use roku_agent_runtime::RouteDecisionResult;
use roku_common_types::{
	ConversationTurn, RequestEnvelope, ResourceSelector, ResponseEnvelope, RuntimeError,
};
use roku_memory::{
	LongTermMemoryBackend, MemoryHit, MemoryLifecyclePolicy, MemoryRecallInput,
	MemoryWritePolicyInput,
};
use roku_observability::LogLevel;

use crate::{RuntimeService, log_runtime};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ContextBundle {
	pub short_term_continuity: Vec<ConversationTurn>,
	pub long_term_memory_hits: Vec<MemoryHit>,
	pub visible_resources: Vec<ResourceSelector>,
	pub pending_loop_active: bool,
	pub assumptions: Vec<String>,
	pub blockers: Vec<String>,
}

impl RuntimeService {
	pub fn with_long_term_memory_backend(
		mut self,
		memory_backend: Arc<dyn LongTermMemoryBackend>,
	) -> Self {
		self.memory_backend = memory_backend;
		self
	}

	pub fn with_memory_lifecycle_policy(
		mut self,
		memory_policy: Arc<dyn MemoryLifecyclePolicy>,
	) -> Self {
		self.memory_policy = memory_policy;
		self
	}

	pub(crate) fn build_context_bundle(
		&self,
		request: &RequestEnvelope,
		pending_loop_active: bool,
	) -> Result<ContextBundle, RuntimeError> {
		let mut bundle = ContextBundle {
			short_term_continuity: request.conversation_history.clone(),
			pending_loop_active,
			..ContextBundle::default()
		};
		let recall_input = MemoryRecallInput {
			session_id: request.session_id.clone(),
			goal: request.goal.clone(),
			planning_mode_hint_present: request.planning_mode_hint.is_some(),
			pending_loop_active,
			short_term_continuity: bundle.short_term_continuity.clone(),
			user_id: None,
			project_id: None,
			workspace_id: None,
		};

		let Some(query) = self.memory_policy.build_recall_query(&recall_input) else {
			log_runtime(
				LogLevel::Debug,
				"long-term memory recall skipped by policy",
				[
					("request_id", request.request_id.0.clone()),
					("session_id", request.session_id.clone()),
					(
						"pending_loop_active",
						bundle.pending_loop_active.to_string(),
					),
				],
			);
			return Ok(bundle);
		};

		match self.memory_backend.search(&query) {
			Ok(hits) => {
				bundle.long_term_memory_hits = hits;
				log_runtime(
					LogLevel::Info,
					"assembled runtime context bundle",
					[
						("request_id", request.request_id.0.clone()),
						("session_id", request.session_id.clone()),
						(
							"short_term_turns",
							bundle.short_term_continuity.len().to_string(),
						),
						(
							"long_term_hits",
							bundle.long_term_memory_hits.len().to_string(),
						),
						(
							"memory_backend",
							self.memory_backend.backend_name().to_string(),
						),
					],
				);
			}
			Err(error) => {
				let detail = error.to_string();
				bundle
					.blockers
					.push(format!("long-term recall unavailable: {detail}"));
				log_runtime(
					LogLevel::Warn,
					"long-term memory recall failed",
					[
						("request_id", request.request_id.0.clone()),
						("session_id", request.session_id.clone()),
						(
							"memory_backend",
							self.memory_backend.backend_name().to_string(),
						),
						("error", detail),
					],
				);
			}
		}

		Ok(bundle)
	}

	pub(crate) fn attach_resumed_loop_resources(
		&self,
		context_bundle: &mut ContextBundle,
		loop_state: &roku_agent_runtime::LoopState,
	) {
		context_bundle.visible_resources = loop_state.bound_resources.clone();
	}

	pub(crate) fn attach_visible_resources(
		&self,
		context_bundle: &mut ContextBundle,
		route: &RouteDecisionResult,
	) {
		context_bundle.visible_resources = match route {
			RouteDecisionResult::Direct(plan) => plan.bound_resources.clone(),
			RouteDecisionResult::Escalate(_) => Vec::new(),
		};
	}

	pub(crate) fn apply_memory_write_back(
		&self,
		request: &RequestEnvelope,
		response: &ResponseEnvelope,
		context_bundle: &ContextBundle,
	) {
		let write_input = MemoryWritePolicyInput {
			request_id: request.request_id.0.clone(),
			session_id: request.session_id.clone(),
			goal: request.goal.clone(),
			response_status: response.status,
			response_message: response.message.clone(),
			pending_loop_active: context_bundle.pending_loop_active,
			short_term_continuity: context_bundle.short_term_continuity.clone(),
			recalled_hits: context_bundle.long_term_memory_hits.clone(),
			user_id: None,
			project_id: None,
			workspace_id: None,
		};
		let Some(write_request) = self.memory_policy.build_write_request(&write_input) else {
			return;
		};

		if let Err(error) = self.memory_backend.write(&write_request) {
			log_runtime(
				LogLevel::Warn,
				"long-term memory write-back failed",
				[
					("request_id", request.request_id.0.clone()),
					("session_id", request.session_id.clone()),
					(
						"memory_backend",
						self.memory_backend.backend_name().to_string(),
					),
					("error", error.to_string()),
				],
			);
		}
	}
}
