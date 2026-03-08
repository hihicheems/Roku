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

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use roku_common_types::{
	ApprovalDecision, ApprovalId, ConversationRole, ConversationTurn, PlanningModeHint,
	RequestEnvelope, ResponseEnvelope, RuntimeError, SessionPreferences,
};
use roku_observability::{LogLevel, LogRecord, emit_global_log};
use roku_state_store::{
	ConversationRepository, InMemoryConversationRepository, InMemorySessionPreferenceRepository,
	PostgresConversationRepository, PostgresSessionPreferenceRepository, PostgresStoreConfig,
	SessionPreferenceRepository, StoreError,
};

use crate::CommandError;
use crate::runtime::build_live_runtime_service_from_env;

pub fn run_telegram_bot_from_env() -> Result<(), CommandError> {
	let service = Arc::new(build_live_runtime_service_from_env()?);
	let session_state = Arc::new(TelegramSessionState::from_env()?);
	let runner = roku_connectors_telegram::TelegramPollingRunner::from_env()?;
	let _ = emit_global_log(LogRecord::new(
		"roku-cmd",
		LogLevel::Info,
		"starting telegram bot polling loop",
	));
	runner
		.run(RuntimeServiceTelegramHandler {
			service,
			session_state,
		})
		.map_err(CommandError::TelegramTransport)
}

struct RuntimeServiceTelegramHandler {
	service: Arc<roku_runtime_service::RuntimeService>,
	session_state: Arc<TelegramSessionState>,
}

impl roku_connectors_telegram::TelegramInteractionHandler for RuntimeServiceTelegramHandler {
	fn handle_request(
		&self,
		mut request: RequestEnvelope,
	) -> Result<ResponseEnvelope, RuntimeError> {
		let session_id = request.session_id.clone();
		let preferences = self.session_state.load_preferences(&session_id)?;
		if request.planning_mode_hint.is_none() {
			request.planning_mode_hint = preferences.planning_mode;
		}
		request.conversation_history = self.session_state.load_recent_turns(&session_id, 12)?;
		self.session_state.append_turn(
			&session_id,
			ConversationTurn {
				role: ConversationRole::User,
				content: request.goal.clone(),
				created_at_unix_ms: now_unix_ms(),
			},
		)?;

		match self.service.execute(request) {
			Ok(response) => {
				self.session_state.append_turn(
					&session_id,
					ConversationTurn {
						role: ConversationRole::Assistant,
						content: response.message.clone(),
						created_at_unix_ms: now_unix_ms(),
					},
				)?;
				Ok(response)
			}
			Err(error) => {
				self.session_state.append_turn(
					&session_id,
					ConversationTurn {
						role: ConversationRole::Assistant,
						content: format!("task failed: {}", error.message),
						created_at_unix_ms: now_unix_ms(),
					},
				)?;
				Err(error)
			}
		}
	}

	fn update_session_planning_mode(
		&self,
		session_id: &str,
		planning_mode: Option<PlanningModeHint>,
	) -> Result<(), RuntimeError> {
		self.session_state
			.save_preferences(session_id, SessionPreferences { planning_mode })
	}

	fn handle_approval_decision(
		&self,
		approval_id: ApprovalId,
		decision: ApprovalDecision,
	) -> Result<ResponseEnvelope, RuntimeError> {
		self.service.decide_approval(&approval_id, decision)
	}
}

struct TelegramSessionState {
	preferences: Mutex<Box<dyn SessionPreferenceRepository + Send>>,
	conversation: Mutex<Box<dyn ConversationRepository + Send>>,
}

impl TelegramSessionState {
	fn from_env() -> Result<Self, CommandError> {
		if let Some(config) = PostgresStoreConfig::from_env()
			.map_err(|error| CommandError::StateStoreBootstrap(error.to_string()))?
		{
			let _ = emit_global_log(
				LogRecord::new(
					"roku-cmd",
					LogLevel::Info,
					"using postgres-backed telegram session state",
				)
				.with_field("schema", config.schema.clone()),
			);
			return Ok(Self::new(
				Box::new(
					PostgresSessionPreferenceRepository::connect(config.clone())
						.map_err(|error| CommandError::StateStoreBootstrap(error.to_string()))?,
				),
				Box::new(
					PostgresConversationRepository::connect(config)
						.map_err(|error| CommandError::StateStoreBootstrap(error.to_string()))?,
				),
			));
		}

		let _ = emit_global_log(LogRecord::new(
			"roku-cmd",
			LogLevel::Info,
			"using in-memory telegram session state",
		));
		Ok(Self::default())
	}

	fn new(
		preferences: Box<dyn SessionPreferenceRepository + Send>,
		conversation: Box<dyn ConversationRepository + Send>,
	) -> Self {
		Self {
			preferences: Mutex::new(preferences),
			conversation: Mutex::new(conversation),
		}
	}

	fn save_preferences(
		&self,
		session_id: &str,
		preferences: SessionPreferences,
	) -> Result<(), RuntimeError> {
		let mut store = self.lock_preferences()?;
		store
			.save_preferences(session_id, preferences)
			.map_err(runtime_store_error)?;
		Ok(())
	}

	fn load_preferences(&self, session_id: &str) -> Result<SessionPreferences, RuntimeError> {
		let store = self.lock_preferences()?;
		Ok(store
			.load_preferences(session_id)
			.map_err(runtime_store_error)?
			.unwrap_or_default())
	}

	fn append_turn(&self, session_id: &str, turn: ConversationTurn) -> Result<(), RuntimeError> {
		let mut store = self.lock_conversation()?;
		store
			.append_turn(session_id, turn)
			.map_err(runtime_store_error)?;
		Ok(())
	}

	fn load_recent_turns(
		&self,
		session_id: &str,
		limit: usize,
	) -> Result<Vec<ConversationTurn>, RuntimeError> {
		let store = self.lock_conversation()?;
		store
			.load_recent_turns(session_id, limit)
			.map_err(runtime_store_error)
	}

	fn lock_preferences(
		&self,
	) -> Result<std::sync::MutexGuard<'_, Box<dyn SessionPreferenceRepository + Send>>, RuntimeError>
	{
		self.preferences
			.lock()
			.map_err(|_| RuntimeError::new("session preference store is poisoned"))
	}

	fn lock_conversation(
		&self,
	) -> Result<std::sync::MutexGuard<'_, Box<dyn ConversationRepository + Send>>, RuntimeError> {
		self.conversation
			.lock()
			.map_err(|_| RuntimeError::new("conversation store is poisoned"))
	}
}

impl Default for TelegramSessionState {
	fn default() -> Self {
		Self::new(
			Box::new(InMemorySessionPreferenceRepository::default()),
			Box::new(InMemoryConversationRepository::default()),
		)
	}
}

fn runtime_store_error(error: StoreError) -> RuntimeError {
	RuntimeError::new(error.to_string())
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
	use std::sync::Arc;

	use roku_agent_runtime::GenericAgentRuntime;
	use roku_common_types::{PlanningModeHint, RequestEnvelope, RequestId, ResponseStatus};
	use roku_connectors_telegram::TelegramInteractionHandler;
	use roku_llm_adapter::{
		GenerationRequest, LlmProvider, LlmRouter, ModelProfile, ProviderCallError,
		ProviderResponse, RiskTier, RoutingPolicy,
	};
	use roku_runtime_service::RuntimeService;

	use super::*;

	struct SessionAwareLlmProvider;

	impl LlmProvider for SessionAwareLlmProvider {
		fn provider_name(&self) -> &'static str {
			"session-test-provider"
		}

		fn complete(
			&self,
			_model: &ModelProfile,
			request: &GenerationRequest,
		) -> Result<ProviderResponse, ProviderCallError> {
			let output = if request.prompt.contains("User request:\n沙县小吃是什么？") {
				"沙县小吃是福建沙县起源的一类大众化中式快餐小吃。".to_string()
			} else if request.prompt.contains("User request:\n我刚问了你什么？") {
				let last_user_turn = extract_last_user_turn(&request.prompt)
					.unwrap_or_else(|| "我没有看到上一条用户消息。".to_string());
				format!("你刚才问的是：{last_user_turn}")
			} else {
				"我是Roku。".to_string()
			};

			Ok(ProviderResponse {
				output,
				prompt_tokens: 64,
				output_tokens: 24,
				latency_ms: 10,
			})
		}
	}

	#[test]
	fn telegram_handler_persists_mode_and_multi_turn_memory() {
		let mut router = LlmRouter::new(RoutingPolicy {
			max_request_cost_usd: 1.0,
			max_latency_ms: 5_000,
		});
		router.register_provider(SessionAwareLlmProvider);
		router.register_model(ModelProfile {
			model_id: "session-test-model".to_string(),
			provider: "session-test-provider".to_string(),
			max_context_tokens: 16_000,
			cost_per_1k_tokens_usd: 0.0,
			max_risk_tier: RiskTier::Critical,
			route_priority: 100,
		});

		let runtime = GenericAgentRuntime::with_llm_router(router);
		let handler = RuntimeServiceTelegramHandler {
			service: Arc::new(RuntimeService::in_memory_with_agent_runtime(runtime)),
			session_state: Arc::new(TelegramSessionState::default()),
		};
		let session_id = "telegram-session-1";
		handler
			.update_session_planning_mode(session_id, Some(PlanningModeHint::ReAct))
			.expect("session mode update should succeed");

		let first = handler
			.handle_request(request(session_id, "今天周几？"))
			.expect("first request should succeed");
		assert_eq!(first.status, ResponseStatus::Succeeded);
		assert!(first.message.starts_with("星期"));

		let second = handler
			.handle_request(request(session_id, "沙县小吃是什么？"))
			.expect("second request should succeed");
		assert_eq!(second.status, ResponseStatus::Succeeded);
		assert!(second.message.contains("福建沙县"));

		let third = handler
			.handle_request(request(session_id, "我刚问了你什么？"))
			.expect("third request should succeed");
		assert_eq!(third.status, ResponseStatus::Succeeded);
		assert_eq!(third.message, "你刚才问的是：沙县小吃是什么？");

		let preferences = handler
			.session_state
			.load_preferences(session_id)
			.expect("preferences should load");
		assert_eq!(preferences.planning_mode, Some(PlanningModeHint::ReAct));

		let turns = handler
			.session_state
			.load_recent_turns(session_id, 8)
			.expect("turns should load");
		assert_eq!(turns.len(), 6);
		assert_eq!(turns[0].content, "今天周几？");
		assert_eq!(turns[2].content, "沙县小吃是什么？");
		assert_eq!(turns[4].content, "我刚问了你什么？");
	}

	fn request(session_id: &str, goal: &str) -> RequestEnvelope {
		RequestEnvelope {
			request_id: RequestId(format!("req-{goal}")),
			session_id: session_id.to_string(),
			goal: goal.to_string(),
			planning_mode_hint: None,
			conversation_history: Vec::new(),
		}
	}

	fn extract_last_user_turn(prompt: &str) -> Option<String> {
		let history = prompt
			.split("Conversation history (most recent first-order context):\n")
			.nth(1)?
			.split("\n\nTrusted runtime context:")
			.next()?;

		history
			.lines()
			.rev()
			.find_map(|line| line.strip_prefix("user: ").map(str::to_string))
	}
}
