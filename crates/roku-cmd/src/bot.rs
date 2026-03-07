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
					PostgresSessionPreferenceRepository::connect(config.clone()).map_err(|error| {
						CommandError::StateStoreBootstrap(error.to_string())
					})?,
				),
				Box::new(PostgresConversationRepository::connect(config).map_err(|error| {
					CommandError::StateStoreBootstrap(error.to_string())
				})?),
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
	) -> Result<
		std::sync::MutexGuard<'_, Box<dyn SessionPreferenceRepository + Send>>,
		RuntimeError,
	> {
		self.preferences
			.lock()
			.map_err(|_| RuntimeError::new("session preference store is poisoned"))
	}

	fn lock_conversation(
		&self,
	) -> Result<
		std::sync::MutexGuard<'_, Box<dyn ConversationRepository + Send>>,
		RuntimeError,
	> {
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
