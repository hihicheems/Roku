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

//! CLI-side Telegram runtime adapter.
//!
//! This module binds the Telegram transport layer to the runtime service and Telegram-scoped
//! session state. It owns chat-local concerns such as conversation history, pending-loop resume
//! bindings, and out-of-band control commands. It does not introduce a separate Telegram
//! strategy-selection layer or a multi-session model.

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use roku_common_types::{
	ApprovalDecision, ApprovalId, ConversationRole, ConversationTurn, RequestEnvelope, RequestId,
	ResponseEnvelope, ResponseStatus, RuntimeError,
};
use roku_memory::{
	InMemorySessionStateBackend, InMemoryShortTermContinuityBackend, PendingLoopSnapshot,
	PendingLoopSnapshotBackend, PendingLoopSnapshotError, ResolvedMemorySubsystem, SessionState,
	SessionStateBackend, SessionStateError, ShortTermContinuityBackend, ShortTermContinuityError,
};
use roku_observability::{LogLevel, LogRecord, emit_global_log};
use roku_plugin_telegram::{
	TelegramBotConfig, TelegramChat, TelegramConnector, TelegramControlCommand,
	TelegramControlCommandRequest, TelegramInteraction, TelegramInteractionHandler,
	TelegramMessage, TelegramOutboundMessage, TelegramParseMode, TelegramRuntimeConfig,
	TelegramUpdate, TelegramUser,
};
use roku_runtime_service::{RuntimeExecutionMode, RuntimeModeReport};
use serde_json::json;

use crate::CommandError;
use crate::entry_registry::resolve_memory_subsystem;
use crate::runtime::ExecutionRequestOptions;
use crate::runtime::{
	apply_request_env_overrides, build_live_runtime_service_from_layout_and_bootstrap,
	build_plugin_bootstrap_from_env, ensure_plugin_enabled_for_command,
};
use crate::runtime_config::load_runtime_configs;
use crate::storage::LocalStorageLayout;
use crate::telegram_loop_bridge::{
	restore_pending_loop_from_session, sync_pending_loop_to_session,
};

/// Starts the Telegram bot polling loop using env-driven layout and plugin bootstrap.
///
/// Requires the `telegram` plugin to be enabled and `TELOXIDE_TOKEN` or `TELEGRAM_BOT_TOKEN` to be
/// set. This is the main entry point for long-running bot processes; it does not return until the
/// transport stops.
pub fn run_telegram_bot_from_env() -> Result<(), CommandError> {
	let (layout, bootstrap) = build_plugin_bootstrap_from_env()?;
	ensure_plugin_enabled_for_command(
		&bootstrap.plugin_snapshot,
		"telegram",
		"telegram-once/telegram-bot",
	)?;
	let runner = roku_plugin_telegram::TelegramPollingRunner::new(telegram_bot_config_from_env(
		bootstrap.runtime_configs.telegram.clone(),
	)?)?;
	let handler = RuntimeServiceTelegramHandler {
		service: Arc::new(build_live_runtime_service_from_layout_and_bootstrap(
			&layout, bootstrap,
		)?),
		transport_state: Arc::new(TelegramTransportState::from_env()?),
	};
	let _ = emit_global_log(LogRecord::new(
		"roku-cmd",
		LogLevel::Info,
		"starting telegram bot polling loop",
	));
	runner.run(handler).map_err(CommandError::TelegramTransport)
}

/// Runs a single Telegram-style request in-process and returns a JSON preview string.
///
/// Used by the `telegram-once` CLI: builds a synthetic update from `options.goal`, executes one
/// request or control command, and renders the result as JSON (no real Telegram I/O). Session ID
/// and planning hint come from `options`; approval callbacks are not supported in this mode.
pub(crate) fn run_telegram_once_with_options_from_env(
	options: ExecutionRequestOptions,
) -> Result<String, CommandError> {
	apply_request_env_overrides(&options);
	let handler = build_live_telegram_handler_from_env()?;
	let chat_id = 1;
	let interaction = TelegramConnector
		.interaction_from_update(TelegramUpdate {
			update_id: 1,
			message: Some(TelegramMessage {
				message_id: 1,
				chat: TelegramChat {
					id: chat_id,
					title: None,
					kind: "private".to_string(),
				},
				from: Some(TelegramUser {
					id: 1,
					is_bot: false,
					username: Some("telegram-once".to_string()),
				}),
				text: Some(options.goal.clone()),
			}),
			callback_query: None,
		})
		.map_err(|error| {
			CommandError::Usage(format!("failed to build telegram preview: {error}"))
		})?;

	match interaction {
		TelegramInteraction::Request {
			chat_id,
			mut request,
		} => {
			request.request_id = RequestId(format!("tg-cli-{}", now_unix_ms()));
			request.session_id = options.session_id;
			request.planning_mode_hint = options.planning_mode_hint;
			render_telegram_preview(chat_id, handler.handle_request(request))
		}
		TelegramInteraction::ControlCommand(mut command) => {
			command.session_id = options.session_id;
			render_telegram_preview(chat_id, handler.handle_control_command(command))
		}
		TelegramInteraction::ApprovalDecision(_) => Err(CommandError::Usage(
			"telegram-once preview should not produce approval callbacks".to_string(),
		)),
	}
}

/// Builds a live handler from env (layout + plugin bootstrap); used by telegram-once and tests.
fn build_live_telegram_handler_from_env() -> Result<RuntimeServiceTelegramHandler, CommandError> {
	let (layout, bootstrap) = build_plugin_bootstrap_from_env()?;
	ensure_plugin_enabled_for_command(
		&bootstrap.plugin_snapshot,
		"telegram",
		"telegram-once/telegram-bot",
	)?;
	Ok(RuntimeServiceTelegramHandler {
		service: Arc::new(build_live_runtime_service_from_layout_and_bootstrap(
			&layout, bootstrap,
		)?),
		transport_state: Arc::new(TelegramTransportState::from_env()?),
	})
}

/// Builds bot config from env; token precedence is `TELOXIDE_TOKEN` then `TELEGRAM_BOT_TOKEN`.
fn telegram_bot_config_from_env(
	runtime_config: TelegramRuntimeConfig,
) -> Result<TelegramBotConfig, CommandError> {
	let token = std::env::var("TELOXIDE_TOKEN")
		.ok()
		.filter(|value| !value.trim().is_empty())
		.or_else(|| {
			std::env::var("TELEGRAM_BOT_TOKEN")
				.ok()
				.filter(|value| !value.trim().is_empty())
		})
		.ok_or(roku_plugin_telegram::TelegramTransportError::MissingBotToken)?;
	Ok(runtime_config.with_token(token))
}

/// Binds Telegram transport to the runtime service and Telegram-scoped session state.
///
/// Owns one shared runtime service and one [`TelegramTransportState`]; each request restores
/// pending loop from session, runs the runtime, then syncs pending loop and conversation back.
/// Does not perform strategy selection or multi-session routing; that stays in the runtime.
struct RuntimeServiceTelegramHandler {
	service: Arc<roku_runtime_service::RuntimeService>,
	transport_state: Arc<TelegramTransportState>,
}

/// Stable snapshot for Telegram session management commands.
///
/// This is a view-model for `/status` and `/sessions`, not the source of truth. The underlying
/// truth still lives in the runtime service's pending-loop store and the session repositories.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TelegramSessionSnapshot {
	session_id: String,
	/// When present, the chat has a resumable pending loop; consumed by `/status` and `/cancel`.
	pending_run_id: Option<String>,
	recent_turn_count: usize,
	/// Last turn summary (e.g. "user: ..." or "assistant: ...") for display only.
	latest_activity: Option<String>,
}

impl roku_plugin_telegram::TelegramInteractionHandler for RuntimeServiceTelegramHandler {
	fn handle_request(
		&self,
		mut request: RequestEnvelope,
	) -> Result<ResponseEnvelope, RuntimeError> {
		let session_id = request.session_id.clone();
		// Restore pending loop so this request continues from last saved state; then load history.
		restore_pending_loop_from_session(&self.service, &*self.transport_state, &session_id)?;
		request.conversation_history = self
			.transport_state
			.load_short_term_continuity(&session_id, 12)?;
		self.transport_state.append_turn(
			&session_id,
			ConversationTurn {
				role: ConversationRole::User,
				content: request.goal.clone(),
				created_at_unix_ms: now_unix_ms(),
			},
		)?;

		match self.service.execute(request) {
			Ok(response) => {
				// Sync pending loop and append assistant turn so session always reflects last outcome.
				sync_pending_loop_to_session(&self.service, &*self.transport_state, &session_id)?;
				self.transport_state.append_turn(
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
				// Same sync and append on error so conversation history and pending state stay consistent.
				sync_pending_loop_to_session(&self.service, &*self.transport_state, &session_id)?;
				self.transport_state.append_turn(
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

	fn handle_control_command(
		&self,
		command: TelegramControlCommandRequest,
	) -> Result<ResponseEnvelope, RuntimeError> {
		// Reject inline args for commands that do not allow them (e.g. /clear, /cancel).
		if command.argument.is_some() && !command.command.allows_inline_argument() {
			return Ok(self.control_command_response(
				command.command,
				ResponseStatus::Failed,
				format!(
					"`/{}` does not accept extra arguments. Use it on its own.",
					command.command.as_str()
				),
			));
		}

		match command.command {
			TelegramControlCommand::Help => Ok(self.control_command_response(
				command.command,
				ResponseStatus::Succeeded,
				self.help_message(),
			)),
			TelegramControlCommand::Status => {
				self.refresh_session_pending_state(&command.session_id)?;
				let snapshot = self.transport_state.status_snapshot(&command.session_id)?;
				Ok(self.control_command_response(
					command.command,
					ResponseStatus::Succeeded,
					self.status_message(&snapshot),
				))
			}
			TelegramControlCommand::Sessions => {
				self.refresh_session_pending_state(&command.session_id)?;
				let snapshot = self.transport_state.status_snapshot(&command.session_id)?;
				Ok(self.control_command_response(
					command.command,
					ResponseStatus::Succeeded,
					self.sessions_message(&snapshot),
				))
			}
			TelegramControlCommand::Cancel => {
				self.refresh_session_pending_state(&command.session_id)?;
				let snapshot = self.transport_state.status_snapshot(&command.session_id)?;
				if snapshot.pending_run_id.is_none() {
					return Ok(self.control_command_response(
						command.command,
						ResponseStatus::Succeeded,
						self.cancel_message(&snapshot, false),
					));
				}
				// Clear both runtime and session binding so no stale pending loop remains.
				self.service.clear_pending_loop(&command.session_id)?;
				self.transport_state
					.clear_pending_loop_snapshot(&command.session_id)
					.map_err(|error| RuntimeError::new(error.to_string()))?;
				let snapshot = self.transport_state.status_snapshot(&command.session_id)?;
				Ok(self.control_command_response(
					command.command,
					ResponseStatus::Succeeded,
					self.cancel_message(&snapshot, true),
				))
			}
			TelegramControlCommand::Clear => {
				self.service.clear_pending_loop(&command.session_id)?;
				self.transport_state
					.clear_transport_session(&command.session_id)?;
				let snapshot = self.transport_state.status_snapshot(&command.session_id)?;
				Ok(self.control_command_response(
					command.command,
					ResponseStatus::Succeeded,
					self.clear_message(&snapshot),
				))
			}
		}
	}

	fn handle_approval_decision(
		&self,
		approval_id: ApprovalId,
		decision: ApprovalDecision,
	) -> Result<ResponseEnvelope, RuntimeError> {
		self.service.decide_approval(&approval_id, decision)
	}
}

impl RuntimeServiceTelegramHandler {
	/// Reconciles the runtime's pending-loop state with the Telegram session binding.
	///
	/// Status-like commands should call this before reading a session snapshot so Telegram control
	/// views reflect the latest resumable-loop truth instead of stale session metadata.
	fn refresh_session_pending_state(&self, session_id: &str) -> Result<(), RuntimeError> {
		restore_pending_loop_from_session(&self.service, &*self.transport_state, session_id)?;
		sync_pending_loop_to_session(&self.service, &*self.transport_state, session_id)
	}

	/// Builds a response envelope for control commands; request_id is synthetic (tg-control-{command}-{ts}).
	fn control_command_response(
		&self,
		command: TelegramControlCommand,
		status: ResponseStatus,
		message: impl Into<String>,
	) -> ResponseEnvelope {
		ResponseEnvelope {
			request_id: RequestId(format!("tg-control-{}-{}", command.as_str(), now_unix_ms())),
			status,
			message: message.into(),
			artifacts: Vec::new(),
		}
	}

	fn help_message(&self) -> String {
		[
			"Telegram control commands:".to_string(),
			"/help - Show available control commands.".to_string(),
			"/status - Show the current chat session status.".to_string(),
			"/sessions - Show the current active session overview.".to_string(),
			"/cancel - Cancel the current pending loop without clearing chat history.".to_string(),
			"/clear - Clear the current chat session state and pending loop.".to_string(),
			"".to_string(),
			"Send natural language directly to start a task.".to_string(),
		]
		.join("\n")
	}

	fn status_message(&self, snapshot: &TelegramSessionSnapshot) -> String {
		let mut lines = vec!["Current chat session status".to_string()];
		append_session_snapshot_lines(&mut lines, snapshot);
		append_runtime_mode_lines(&mut lines, &self.service.runtime_mode_report());
		lines.join("\n")
	}

	fn sessions_message(&self, snapshot: &TelegramSessionSnapshot) -> String {
		let mut lines = vec!["Active session".to_string()];
		append_session_snapshot_lines(&mut lines, snapshot);
		append_runtime_mode_lines(&mut lines, &self.service.runtime_mode_report());
		lines.push(
			"Multi-session switching is not enabled yet. `/new` will arrive later.".to_string(),
		);
		lines.join("\n")
	}

	fn cancel_message(&self, snapshot: &TelegramSessionSnapshot, cancelled: bool) -> String {
		let mut lines = if cancelled {
			vec![
				"Cancelled the pending runtime loop for this chat session.".to_string(),
				"Conversation history, approvals, tasks, artifacts, and completed observations were left untouched."
					.to_string(),
			]
		} else {
			vec!["No pending runtime loop is currently stored for this chat session.".to_string()]
		};
		append_session_snapshot_lines(&mut lines, snapshot);
		lines.join("\n")
	}

	fn clear_message(&self, snapshot: &TelegramSessionSnapshot) -> String {
		let mut lines = vec!["Cleared the current chat session state.".to_string()];
		append_session_snapshot_lines(&mut lines, snapshot);
		lines.push(
			"Only Telegram session state was cleared. Runtime global config and cross-channel shared state were left untouched."
				.to_string(),
		);
		lines.push("Stored task records and artifacts were not deleted.".to_string());
		lines.join("\n")
	}
}

/// Appends human-readable snapshot lines for `/status` and `/sessions`; order is fixed for tests.
fn append_session_snapshot_lines(lines: &mut Vec<String>, snapshot: &TelegramSessionSnapshot) {
	lines.push(format!("Session: {}", snapshot.session_id));
	match snapshot.pending_run_id.as_deref() {
		Some(run_id) => {
			lines.push("Pending loop: yes".to_string());
			lines.push(format!("Pending run: {run_id}"));
		}
		None => {
			lines.push("Pending loop: no".to_string());
			lines.push("Pending run: none".to_string());
		}
	}
	lines.push(format!("Recent turns: {}", snapshot.recent_turn_count));
	if let Some(activity) = snapshot.latest_activity.as_deref() {
		lines.push(format!("Latest activity: {activity}"));
	}
}

fn append_runtime_mode_lines(lines: &mut Vec<String>, report: &RuntimeModeReport) {
	lines.push(format!(
		"Runtime mode: requested={}, effective={}",
		runtime_execution_mode_label(report.requested),
		runtime_execution_mode_label(report.effective)
	));
	if let Some(reason) = report.fallback_reason.as_deref() {
		lines.push(format!("Runtime fallback: {reason}"));
	}
}

fn runtime_execution_mode_label(mode: RuntimeExecutionMode) -> &'static str {
	mode.as_str()
}

fn latest_activity_summary(turn: &ConversationTurn) -> String {
	format!(
		"{}: {}",
		match turn.role {
			ConversationRole::User => "user",
			ConversationRole::Assistant => "assistant",
			ConversationRole::System => "system",
		},
		truncate_preview(&turn.content, 96)
	)
}

fn truncate_preview(value: &str, max_chars: usize) -> String {
	let truncated: String = value.chars().take(max_chars).collect();
	if value.chars().count() > max_chars {
		format!("{truncated}...")
	} else {
		truncated
	}
}

/// Renders the result of a single telegram-once run as pretty JSON (runtime_status, telegram_message, etc.).
fn render_telegram_preview(
	chat_id: i64,
	response: Result<ResponseEnvelope, RuntimeError>,
) -> Result<String, CommandError> {
	let preview = match response {
		Ok(response) => {
			let outbound = TelegramOutboundMessage::from_response(chat_id, &response);
			json!({
				"runtime_status": format!("{:?}", response.status),
				"request_id": response.request_id.0,
				"telegram_message": outbound.text,
				"parse_mode": telegram_parse_mode_label(outbound.parse_mode),
			})
		}
		Err(error) => {
			let outbound = TelegramOutboundMessage::from_error(chat_id, &error.message);
			json!({
				"runtime_status": "Error",
				"telegram_message": outbound.text,
				"parse_mode": telegram_parse_mode_label(outbound.parse_mode),
				"error": error.message,
			})
		}
	};
	serde_json::to_string_pretty(&preview)
		.map_err(|error| CommandError::OutputEncoding(error.to_string()))
}

fn telegram_parse_mode_label(mode: TelegramParseMode) -> &'static str {
	match mode {
		TelegramParseMode::PlainText => "PlainText",
		TelegramParseMode::MarkdownV2 => "MarkdownV2",
	}
}

/// Telegram-scoped transport state: session state (e.g. pending loop binding) and conversation history.
///
/// Consumed by [`RuntimeServiceTelegramHandler`] to restore/sync pending loop and to load/append
/// turns. Source of truth for Telegram session data; runtime service holds the actual loop state.
pub(crate) struct TelegramTransportState {
	/// Session-scoped transport state (planning mode, pending loop binding); one store per process.
	session_state_store: Mutex<Box<dyn SessionStateBackend + Send>>,
	/// Conversation turns per session_id; one store per process.
	conversation_store: Mutex<Box<dyn ShortTermContinuityBackend + Send>>,
}

impl TelegramTransportState {
	/// Builds state from env using the selected memory entry-registry bundle.
	fn from_env() -> Result<Self, CommandError> {
		let layout = LocalStorageLayout::from_env();
		layout.ensure_dirs().map_err(CommandError::Io)?;
		let runtime_configs = load_runtime_configs(&layout)?;
		let subsystem = resolve_memory_subsystem(&runtime_configs.memory)?;
		let _ = emit_global_log(
			LogRecord::new(
				"roku-cmd",
				LogLevel::Info,
				"using registry-backed telegram transport state",
			)
			.with_field("backend", runtime_configs.memory.backend.as_str())
			.with_field("enabled", runtime_configs.memory.enabled.to_string()),
		);
		Ok(Self::from_memory_subsystem(subsystem))
	}

	/// Constructs state with the given store implementations (used by tests and from_env).
	fn new(
		session_state_store: Box<dyn SessionStateBackend + Send>,
		conversation_store: Box<dyn ShortTermContinuityBackend + Send>,
	) -> Self {
		Self {
			session_state_store: Mutex::new(session_state_store),
			conversation_store: Mutex::new(conversation_store),
		}
	}

	fn from_memory_subsystem(subsystem: ResolvedMemorySubsystem) -> Self {
		Self::new(subsystem.session_state, subsystem.short_term)
	}

	/// Persists transport-owned session state for the given session.
	pub(crate) fn save_session_state(
		&self,
		session_id: &str,
		state: SessionState,
	) -> Result<(), RuntimeError> {
		let mut store = self.lock_session_state_store()?;
		store
			.save_session_state(session_id, state)
			.map_err(runtime_session_state_error)?;
		Ok(())
	}

	/// Loads transport-owned session state for the session, or default when none exists.
	pub(crate) fn load_session_state_or_default(
		&self,
		session_id: &str,
	) -> Result<SessionState, RuntimeError> {
		let store = self.lock_session_state_store()?;
		Ok(store
			.load_session_state(session_id)
			.map_err(runtime_session_state_error)?
			.unwrap_or_default())
	}

	fn append_turn(&self, session_id: &str, turn: ConversationTurn) -> Result<(), RuntimeError> {
		let mut store = self.lock_conversation_store()?;
		store
			.append_continuity_turn(session_id, turn)
			.map_err(runtime_short_term_error)?;
		Ok(())
	}

	fn load_short_term_continuity(
		&self,
		session_id: &str,
		limit: usize,
	) -> Result<Vec<ConversationTurn>, RuntimeError> {
		let store = self.lock_conversation_store()?;
		store
			.load_short_term_continuity(session_id, limit)
			.map_err(runtime_short_term_error)
	}

	/// Clears Telegram session-scoped state for one chat/session.
	///
	/// This intentionally deletes only Telegram-local conversation and session state. It does
	/// not delete persisted tasks, artifacts, approvals, or any runtime-global configuration.
	fn clear_transport_session(&self, session_id: &str) -> Result<(), RuntimeError> {
		self.lock_session_state_store()?
			.delete_session_state(session_id)
			.map_err(runtime_session_state_error)?;
		self.lock_conversation_store()?
			.delete_continuity(session_id)
			.map_err(runtime_short_term_error)?;
		Ok(())
	}

	/// Builds the stable minimal snapshot exposed by `/status` and `/sessions`.
	///
	/// The snapshot is intentionally small and deterministic so Telegram control surfaces can stay
	/// testable even if the underlying repositories evolve.
	fn status_snapshot(&self, session_id: &str) -> Result<TelegramSessionSnapshot, RuntimeError> {
		let session_state = self.load_session_state_or_default(session_id)?;
		let turns = self.load_short_term_continuity(session_id, 12)?;
		Ok(TelegramSessionSnapshot {
			session_id: session_id.to_string(),
			pending_run_id: session_state.pending_loop.map(|binding| binding.run_id),
			recent_turn_count: turns.len(),
			latest_activity: turns.last().map(latest_activity_summary),
		})
	}

	/// Locks the session-state store; returns a runtime error if the mutex is poisoned.
	fn lock_session_state_store(
		&self,
	) -> Result<std::sync::MutexGuard<'_, Box<dyn SessionStateBackend + Send>>, RuntimeError> {
		self.session_state_store
			.lock()
			.map_err(|_| RuntimeError::new("session state store is poisoned"))
	}

	/// Locks the conversation store; returns a runtime error if the mutex is poisoned.
	fn lock_conversation_store(
		&self,
	) -> Result<std::sync::MutexGuard<'_, Box<dyn ShortTermContinuityBackend + Send>>, RuntimeError>
	{
		self.conversation_store
			.lock()
			.map_err(|_| RuntimeError::new("conversation store is poisoned"))
	}
}

impl PendingLoopSnapshotBackend for TelegramTransportState {
	fn load_pending_loop_snapshot(
		&self,
		session_id: &str,
	) -> Result<Option<PendingLoopSnapshot>, PendingLoopSnapshotError> {
		self.load_session_state_or_default(session_id)
			.map(|state| state.pending_loop)
			.map_err(|error| PendingLoopSnapshotError::Backend(error.to_string()))
	}

	fn save_pending_loop_snapshot(
		&self,
		session_id: &str,
		binding: Option<PendingLoopSnapshot>,
	) -> Result<(), PendingLoopSnapshotError> {
		let mut session_state = self
			.load_session_state_or_default(session_id)
			.map_err(|error| PendingLoopSnapshotError::Backend(error.to_string()))?;
		session_state.pending_loop = binding;
		self.save_session_state(session_id, session_state)
			.map_err(|error| PendingLoopSnapshotError::Backend(error.to_string()))
	}
}

impl Default for TelegramTransportState {
	/// In-memory backends only; for tests. Production uses [`TelegramTransportState::from_env`].
	fn default() -> Self {
		Self::new(
			Box::new(InMemorySessionStateBackend::default()),
			Box::new(InMemoryShortTermContinuityBackend::default()),
		)
	}
}

fn runtime_session_state_error(error: SessionStateError) -> RuntimeError {
	RuntimeError::new(error.to_string())
}

fn runtime_short_term_error(error: ShortTermContinuityError) -> RuntimeError {
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
	use std::io::{Cursor, Write};
	use std::sync::Arc;

	use roku_agent_runtime::{
		AskUserPayload, GenericAgentRuntime, IntentFamily, LoopContext, LoopState, LoopStatus,
		RouteDecision, RouteRisk,
	};
	use roku_common_types::{RequestEnvelope, RequestId, ResourceSelector, ResponseStatus};
	use roku_memory::{NoopLongTermMemoryBackend, NoopPendingLoopSnapshotBackend};
	use roku_plugin_llm::{
		GenerationRequest, LlmProvider, LlmRouter, ModelProfile, ProviderCallError,
		ProviderResponse, RiskTier, RoutingPolicy,
	};
	use roku_plugin_skills::{
		DownloadedArchive, SkillArchiveFetcher, SkillRegistry, SkillRegistryError, SkillSource,
	};
	use roku_plugin_telegram::TelegramInteractionHandler;
	use roku_runtime_service::RuntimeService;
	use serde_json::Value;

	use super::*;

	struct SessionAwareLlmProvider;

	#[derive(Clone)]
	struct StaticArchiveFetcher {
		archive: DownloadedArchive,
	}

	impl SkillArchiveFetcher for StaticArchiveFetcher {
		fn fetch(&self, _source: &SkillSource) -> Result<DownloadedArchive, SkillRegistryError> {
			Ok(self.archive.clone())
		}
	}

	impl LlmProvider for SessionAwareLlmProvider {
		fn provider_name(&self) -> &'static str {
			"session-test-provider"
		}

		fn complete(
			&self,
			_model: &ModelProfile,
			request: &GenerationRequest,
		) -> Result<ProviderResponse, ProviderCallError> {
			let output = if request
				.system_prompt
				.as_deref()
				.is_some_and(|prompt| prompt.contains("route classifier"))
			{
				serde_json::json!({
					"intent_family": "chat",
					"confidence": 0.98,
					"requires_multi_step": false,
					"risk": "low",
					"candidate_tools": [],
					"candidate_plugins": [],
					"missing_arguments": [],
					"reason": "plain conversational request"
				})
				.to_string()
			} else if request.system_prompt.as_deref().is_some_and(|prompt| {
				prompt.contains("Return only a JSON object that matches the completion contract")
			}) {
				let final_message = if request.prompt.contains("User request:\n今天周几？") {
					"今天是星期三。".to_string()
				} else if request.prompt.contains("User request:\n沙县小吃是什么？") {
					"沙县小吃是福建沙县起源的一类大众化中式快餐小吃。".to_string()
				} else if request.prompt.contains("User request:\n我刚问了你什么？") {
					let last_user_turn = extract_last_user_turn(&request.prompt)
						.unwrap_or_else(|| "我没有看到上一条用户消息。".to_string());
					format!("你刚才问的是：{last_user_turn}")
				} else {
					"我是Roku。".to_string()
				};
				serde_json::json!({
					"final_message": final_message,
					"completion_kind": "grounded_answer",
					"evidence_status": "grounded",
					"missing_information": [],
				})
				.to_string()
			} else if request.prompt.contains("User request:\n今天周几？") {
				"今天是星期三。".to_string()
			} else if request.prompt.contains("User request:\n沙县小吃是什么？") {
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
				finish_reason: None,
				prompt_tokens: 64,
				output_tokens: 24,
				latency_ms: 10,
			})
		}
	}

	#[test]
	fn telegram_transport_state_uses_resolved_memory_subsystem_seams() {
		let transport_state =
			TelegramTransportState::from_memory_subsystem(ResolvedMemorySubsystem::with_parts(
				Arc::new(NoopLongTermMemoryBackend),
				Box::new(InMemoryShortTermContinuityBackend::default()),
				Box::new(InMemorySessionStateBackend::default()),
				Box::new(NoopPendingLoopSnapshotBackend),
			));
		let session_id = "telegram-seam-session";
		let state = SessionState {
			planning_mode: Some(roku_common_types::PlanningModeHint::TreeSearch),
			pending_loop: None,
		};

		transport_state
			.save_session_state(session_id, state.clone())
			.expect("session state should save through subsystem seam");
		transport_state
			.append_turn(
				session_id,
				ConversationTurn {
					role: ConversationRole::User,
					content: "remember me".to_string(),
					created_at_unix_ms: 1,
				},
			)
			.expect("continuity turn should append through subsystem seam");

		assert_eq!(
			transport_state
				.load_session_state_or_default(session_id)
				.expect("session state should load through subsystem seam"),
			state
		);
		assert_eq!(
			transport_state
				.load_short_term_continuity(session_id, 8)
				.expect("continuity should load through subsystem seam"),
			vec![ConversationTurn {
				role: ConversationRole::User,
				content: "remember me".to_string(),
				created_at_unix_ms: 1,
			}]
		);
	}

	#[test]
	fn telegram_handler_keeps_short_term_continuity_across_regular_requests() {
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
			transport_state: Arc::new(TelegramTransportState::default()),
		};
		let session_id = "telegram-session-1";

		let first = handler
			.handle_request(request(session_id, "今天周几？"))
			.expect("first request should succeed");
		assert_eq!(first.status, ResponseStatus::Succeeded);
		assert_eq!(first.message, "今天是星期三。");

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

		let session_state = handler
			.transport_state
			.load_session_state_or_default(session_id)
			.expect("session state should load");
		assert_eq!(session_state.planning_mode, None);

		let turns = handler
			.transport_state
			.load_short_term_continuity(session_id, 8)
			.expect("turns should load");
		assert_eq!(turns.len(), 6);
		assert_eq!(turns[0].content, "今天周几？");
		assert_eq!(turns[2].content, "沙县小吃是什么？");
		assert_eq!(turns[4].content, "我刚问了你什么？");
	}

	#[test]
	fn telegram_control_cancel_clears_pending_loop_without_clearing_continuity() {
		let handler = test_handler();
		let session_id = "telegram-control-cancel";
		handler
			.transport_state
			.append_turn(
				session_id,
				ConversationTurn {
					role: ConversationRole::User,
					content: "hello".to_string(),
					created_at_unix_ms: 1,
				},
			)
			.expect("conversation turn should save");
		let loop_state = awaiting_user_loop_state(session_id, "loop-cancel-1");
		handler
			.service
			.restore_pending_loop(loop_state)
			.expect("pending loop should restore");
		sync_pending_loop_to_session(&handler.service, &*handler.transport_state, session_id)
			.expect("pending loop should sync");

		let response = handler
			.handle_control_command(control_command(session_id, TelegramControlCommand::Cancel))
			.expect("cancel command should succeed");
		assert_eq!(response.status, ResponseStatus::Succeeded);
		assert!(
			response
				.message
				.contains("Cancelled the pending runtime loop")
		);
		assert!(
			handler
				.service
				.pending_loop(session_id)
				.expect("pending lookup should succeed")
				.is_none()
		);
		assert!(
			handler
				.transport_state
				.load_session_state_or_default(session_id)
				.expect("session state should load")
				.pending_loop
				.is_none()
		);
		assert_eq!(
			handler
				.transport_state
				.load_short_term_continuity(session_id, 8)
				.expect("turns should load")
				.len(),
			1
		);
	}

	#[test]
	fn telegram_control_clear_clears_session_state() {
		let handler = test_handler();
		let session_id = "telegram-control-clear";
		handler
			.transport_state
			.save_session_state(
				session_id,
				SessionState {
					planning_mode: None,
					pending_loop: None,
				},
			)
			.expect("session state should save");
		handler
			.transport_state
			.append_turn(
				session_id,
				ConversationTurn {
					role: ConversationRole::Assistant,
					content: "hi".to_string(),
					created_at_unix_ms: 2,
				},
			)
			.expect("conversation should save");
		let loop_state = awaiting_user_loop_state(session_id, "loop-clear-1");
		handler
			.service
			.restore_pending_loop(loop_state)
			.expect("pending loop should restore");
		sync_pending_loop_to_session(&handler.service, &*handler.transport_state, session_id)
			.expect("pending loop should sync");

		let response = handler
			.handle_control_command(control_command(session_id, TelegramControlCommand::Clear))
			.expect("clear command should succeed");
		assert_eq!(response.status, ResponseStatus::Succeeded);
		assert!(
			response
				.message
				.contains("Cleared the current chat session state.")
		);
		assert!(
			handler
				.service
				.pending_loop(session_id)
				.expect("pending loop lookup should succeed")
				.is_none()
		);
		assert_eq!(
			handler
				.transport_state
				.load_short_term_continuity(session_id, 8)
				.expect("turns should load")
				.len(),
			0
		);
		assert!(
			handler
				.transport_state
				.load_session_state_or_default(session_id)
				.expect("session state should load")
				.pending_loop
				.is_none()
		);
	}

	#[test]
	fn telegram_control_status_reports_current_snapshot() {
		let handler = test_handler();
		let session_id = "telegram-control-status";
		handler
			.transport_state
			.append_turn(
				session_id,
				ConversationTurn {
					role: ConversationRole::Assistant,
					content: "ready".to_string(),
					created_at_unix_ms: 3,
				},
			)
			.expect("conversation should save");
		let loop_state = awaiting_user_loop_state(session_id, "loop-status-1");
		handler
			.service
			.restore_pending_loop(loop_state)
			.expect("pending loop should restore");
		sync_pending_loop_to_session(&handler.service, &*handler.transport_state, session_id)
			.expect("pending loop should sync");

		let response = handler
			.handle_control_command(control_command(session_id, TelegramControlCommand::Status))
			.expect("status command should succeed");
		assert_eq!(response.status, ResponseStatus::Succeeded);
		assert!(response.message.contains("Current chat session status"));
		assert!(
			response
				.message
				.contains("Session: telegram-control-status")
		);
		assert!(response.message.contains("Pending loop: yes"));
		assert!(response.message.contains("Pending run: loop-status-1"));
		assert!(response.message.contains("Recent turns: 1"));
		assert!(response.message.contains("Runtime mode: requested="));
	}

	#[test]
	fn telegram_control_sessions_reports_single_active_session() {
		let handler = test_handler();
		let session_id = "telegram-control-sessions";

		let response = handler
			.handle_control_command(control_command(
				session_id,
				TelegramControlCommand::Sessions,
			))
			.expect("sessions command should succeed");
		assert_eq!(response.status, ResponseStatus::Succeeded);
		assert!(response.message.contains("Active session"));
		assert!(
			response
				.message
				.contains("Session: telegram-control-sessions")
		);
		assert!(
			response
				.message
				.contains("Multi-session switching is not enabled yet.")
		);
	}

	#[test]
	fn telegram_control_help_lists_supported_commands() {
		let handler = test_handler();

		let response = handler
			.handle_control_command(control_command(
				"session-help",
				TelegramControlCommand::Help,
			))
			.expect("help command should succeed");
		assert_eq!(response.status, ResponseStatus::Succeeded);
		assert!(response.message.contains("/cancel"));
		assert!(response.message.contains("/clear"));
		assert!(response.message.contains("/status"));
		assert!(response.message.contains("/help"));
		assert!(response.message.contains("/sessions"));
		assert!(response.message.contains("Send natural language directly"));
	}

	#[test]
	fn recognized_control_commands_do_not_pollute_continuity() {
		let handler = test_handler();
		let session_id = "telegram-control-memory";
		handler
			.handle_request(request(session_id, "今天周几？"))
			.expect("request should succeed");
		let turns_before = handler
			.transport_state
			.load_short_term_continuity(session_id, 8)
			.expect("turns should load");

		handler
			.handle_control_command(control_command(session_id, TelegramControlCommand::Status))
			.expect("status command should succeed");

		let turns_after = handler
			.transport_state
			.load_short_term_continuity(session_id, 8)
			.expect("turns should load");
		assert_eq!(turns_before, turns_after);
	}

	#[test]
	fn control_commands_reject_inline_arguments() {
		let handler = test_handler();
		let response = handler
			.handle_control_command(TelegramControlCommandRequest {
				chat_id: 1,
				session_id: "session-inline".to_string(),
				command: TelegramControlCommand::Clear,
				argument: Some("now".to_string()),
			})
			.expect("command should return a response");
		assert_eq!(response.status, ResponseStatus::Failed);
		assert!(response.message.contains("does not accept extra arguments"));
	}

	#[test]
	fn telegram_handler_surfaces_skill_install_message() {
		let root = tempfile::tempdir().expect("temp root should exist");
		let registry = SkillRegistry::file_backed(root.path().join("skills")).with_fetcher(
			Arc::new(StaticArchiveFetcher {
				archive: DownloadedArchive {
					archive_url: "https://example.com/archive.zip".to_string(),
					bytes: test_skill_archive_bytes(),
					resolved_reference: Some("main".to_string()),
				},
			}),
		);
		let handler = RuntimeServiceTelegramHandler {
			service: Arc::new(RuntimeService::in_memory_with_agent_runtime(
				GenericAgentRuntime::with_skill_registry(registry),
			)),
			transport_state: Arc::new(TelegramTransportState::default()),
		};

		let response = handler
			.handle_request(request(
				"telegram-skill-install",
				"Install skill from https://github.com/anthropics/skills/tree/main/skills/claude-api",
			))
			.expect("skill install request should succeed");
		assert_eq!(response.status, ResponseStatus::Succeeded);
		assert!(response.message.contains("Installed skill `claude-api`"));

		let preview = render_telegram_preview(1001, Ok(response)).expect("preview should render");
		let json: Value = serde_json::from_str(&preview).expect("preview should be valid json");
		assert_eq!(json["runtime_status"], "Succeeded");
		assert!(
			json["telegram_message"]
				.as_str()
				.expect("telegram message should be string")
				.contains("Installed skill `claude-api`")
		);
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

	fn control_command(
		session_id: &str,
		command: TelegramControlCommand,
	) -> TelegramControlCommandRequest {
		TelegramControlCommandRequest {
			chat_id: 1,
			session_id: session_id.to_string(),
			command,
			argument: None,
		}
	}

	fn test_handler() -> RuntimeServiceTelegramHandler {
		RuntimeServiceTelegramHandler {
			service: Arc::new(RuntimeService::in_memory()),
			transport_state: Arc::new(TelegramTransportState::default()),
		}
	}

	fn awaiting_user_loop_state(session_id: &str, run_id: &str) -> LoopState {
		let context = LoopContext {
			request_id: format!("req-{run_id}"),
			session_id: session_id.to_string(),
			goal: "Need a clarification".to_string(),
			workspace_root: "/workspace".to_string(),
			working_directory: "/workspace".to_string(),
			visible_tools: vec!["general.execute".to_string()],
			bound_resources: vec![ResourceSelector::tool("general.execute".to_string())],
			route_decision: RouteDecision::new(
				IntentFamily::Unknown,
				0.8,
				false,
				RouteRisk::Low,
				vec!["general.execute".to_string()],
				Vec::new(),
				Vec::new(),
				"telegram command test",
			),
			last_observation: None,
		};
		let mut loop_state = LoopState::new(run_id, &context);
		loop_state.status = LoopStatus::AwaitingUser;
		loop_state.awaiting_user = Some(AskUserPayload::freeform(
			"Please clarify what you want to do next.".to_string(),
		));
		loop_state
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

	fn test_skill_archive_bytes() -> Vec<u8> {
		let mut cursor = Cursor::new(Vec::new());
		{
			let mut writer = zip::ZipWriter::new(&mut cursor);
			let options = zip::write::SimpleFileOptions::default();
			writer
				.add_directory("skills-main/skills/claude-api/", options)
				.expect("dir should be added");
			writer
				.start_file("skills-main/skills/claude-api/SKILL.md", options)
				.expect("skill file should start");
			writer
				.write_all(
					br#"---
name: claude-api
description: Build apps with the Claude API.
---

# Claude API Skill

Use this skill when the user explicitly asks for Claude API integration help.
"#,
				)
				.expect("skill markdown should write");
			writer.finish().expect("zip should finish");
		}
		cursor.into_inner()
	}
}
