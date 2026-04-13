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
//! session state. It owns chat-local concerns such as conversation history, out-of-band control
//! commands, and Telegram-local session UX. Pending-loop truth stays on the runtime-owned shared
//! substrate; this module only projects that state into Telegram-facing status surfaces.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use roku_agent_runtime::{RunMode, RuntimeExecutionMode, RuntimeModeReport};
use roku_common_types::{
	ApprovalDecision, ApprovalId, ConversationRole, ConversationTurn, RequestEnvelope, RequestId,
	ResponseEnvelope, ResponseStatus, RuntimeError,
};
use roku_common_types::{LogLevel, LogRecord, emit_global_log};
use roku_memory::{
	InMemorySessionManagementBackend, InMemorySessionStateBackend,
	InMemoryShortTermContinuityBackend, ResolvedMemorySubsystem, SESSION_NAME_MAX_CHARS,
	SESSION_NAME_MIN_CHARS, SessionCreateRequest, SessionDeleteMode, SessionDescriptor,
	SessionManagementBackend, SessionManagementError, SessionState, SessionStateBackend,
	SessionStateError, SessionSummary, ShortTermContinuityBackend, ShortTermContinuityError,
	normalize_session_name,
};
use roku_plugin_telegram::{
	TelegramBotClient, TelegramBotConfig, TelegramChat, TelegramConnector, TelegramControlCommand,
	TelegramControlCommandRequest, TelegramHandlerResponse, TelegramInlineKeyboardButton,
	TelegramInteraction, TelegramInteractionHandler, TelegramMessage, TelegramOutboundMessage,
	TelegramParseMode, TelegramReplyMarkup, TelegramRuntimeConfig, TelegramSessionCallbackAction,
	TelegramSessionCallbackKind, TelegramUpdate, TelegramUser, session_delete_cancel_callback_data,
	session_delete_confirm_callback_data, session_page_callback_data, session_select_callback_data,
};
use serde_json::json;

use crate::CommandError;
use crate::conversation::compact_conversation_history;
use crate::entry_registry::resolve_memory_subsystem;
use crate::runtime::ExecutionRequestOptions;
use crate::runtime::{
	apply_request_env_overrides, build_live_runtime_service_from_layout_and_bootstrap,
	build_plugin_bootstrap_from_env, ensure_plugin_enabled_for_command,
};
use crate::runtime_config::load_runtime_configs;
use crate::storage::LocalStorageLayout;
use crate::telegram_session_ux_config::TelegramSessionUxConfig;

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
	let bot_config = telegram_bot_config_from_env(bootstrap.runtime_configs.telegram.clone())?;
	let progress_notices_enabled = bot_config.progress_notices_enabled;
	let bot_client = Arc::new(
		TelegramBotClient::new(bot_config.clone()).map_err(CommandError::TelegramTransport)?,
	);
	let runner = roku_plugin_telegram::TelegramPollingRunner::new(bot_config)?;
	// TODO(issue-165): Telegram should use an async inline-keyboard approval gate.
	// For now it uses None (auto-approve) since there is no interactive stdin available.
	let handler = RuntimeServiceTelegramHandler {
		service: Arc::new(build_live_runtime_service_from_layout_and_bootstrap(
			&layout, bootstrap,
		)?),
		transport_state: Arc::new(TelegramTransportState::from_env()?),
		session_ux_config: TelegramSessionUxConfig::default(),
		pending_session_rename_by_chat: Mutex::new(HashMap::new()),
		bot_client: Some(bot_client),
		progress_notices_enabled,
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
	let _env_override_guard = apply_request_env_overrides(&options);
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
			render_telegram_preview(chat_id, handler.handle_request(request))
		}
		TelegramInteraction::ControlCommand(mut command) => {
			command.session_id = options.session_id;
			render_telegram_preview(chat_id, handler.handle_control_command(command))
		}
		TelegramInteraction::SessionCallback(_) => Err(CommandError::Usage(
			"telegram-once preview should not produce session callbacks".to_string(),
		)),
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
		session_ux_config: TelegramSessionUxConfig::default(),
		pending_session_rename_by_chat: Mutex::new(HashMap::new()),
		bot_client: None,
		progress_notices_enabled: false,
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
/// Owns one shared runtime service and one [`TelegramTransportState`]; each request executes
/// against the resolved provider-neutral session and relies on the runtime-owned pending-loop
/// substrate for resume and sync. Active-session selection stays in the entry + memory session
/// subsystem; runtime only receives the resolved provider-neutral session_id.
struct RuntimeServiceTelegramHandler {
	service: Arc<roku_agent_runtime::RuntimeService>,
	transport_state: Arc<TelegramTransportState>,
	session_ux_config: TelegramSessionUxConfig,
	pending_session_rename_by_chat: Mutex<HashMap<i64, PendingRenameState>>,
	/// Shared bot client used to send tool-progress messages during execution.
	/// `None` when no real Telegram connection is available (e.g. `telegram-once`).
	bot_client: Option<Arc<TelegramBotClient>>,
	/// Mirror of `TelegramPollingRunner::progress_notices_enabled`; gates tool streaming.
	progress_notices_enabled: bool,
}

/// Stable snapshot for Telegram session management commands.
///
/// This is a view-model for `/status` and the active-session summary shown by `/sessions`, not
/// the source of truth. The underlying truth still lives in the runtime service's pending-loop
/// store and the session repositories.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TelegramSessionSnapshot {
	session_id: String,
	session_name: String,
	/// When present, the chat has a resumable pending loop; consumed by `/status` and `/cancel`.
	pending_run_id: Option<String>,
	recent_turn_count: usize,
	/// Last turn summary (e.g. "user: ..." or "assistant: ...") for display only.
	latest_activity: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingRenameState {
	target_session_id: String,
}

/// State returned by the streaming render_task for post-execution edits.
struct StreamingState {
	progress_mid: i64,
	content_mid: i64,
	prompt_tokens: u64,
	output_tokens: u64,
	elapsed: std::time::Duration,
}

/// Truncates text to fit within Telegram's 4096-character message limit.
fn truncate_for_telegram(text: &str) -> String {
	const MAX: usize = 4096;
	if text.len() <= MAX {
		return text.to_string();
	}
	let end = text.ceil_char_boundary(MAX.saturating_sub(4));
	format!("{}...", &text[..end])
}

/// Formats a progress status message showing only execution metadata (no LLM text).
fn format_progress_status(
	step: u32,
	tool: Option<&str>,
	prompt_tokens: u64,
	output_tokens: u64,
) -> String {
	let mut parts: Vec<String> = Vec::new();

	if let Some(tool_name) = tool {
		parts.push(format!(
			"\u{2699}\u{fe0f} Step {} \u{2014} Running {}",
			step, tool_name
		));
	} else if step > 0 {
		parts.push(format!("\u{2699}\u{fe0f} Step {}", step));
	} else {
		parts.push("\u{23f3} Processing...".to_string());
	}

	if prompt_tokens > 0 || output_tokens > 0 {
		parts.push(format!(
			"\u{1f4ca} Tokens: {}/{}",
			prompt_tokens, output_tokens
		));
	}

	parts.join("\n")
}

/// Formats a compact one-line execution summary for the progress message after completion.
///
/// Example: `✅ 12s · ↑6.1k ↓0.2k`
fn format_compact_summary(
	elapsed: std::time::Duration,
	prompt_tokens: u64,
	output_tokens: u64,
) -> String {
	let secs = elapsed.as_secs();
	let duration = if secs >= 60 {
		format!("{}m{}s", secs / 60, secs % 60)
	} else {
		format!("{secs}s")
	};

	let fmt_tokens = |n: u64| -> String {
		if n >= 1000 {
			format!("{:.1}k", n as f64 / 1000.0)
		} else {
			n.to_string()
		}
	};

	let mut parts = vec![format!("\u{2705} {duration}")];
	if prompt_tokens > 0 || output_tokens > 0 {
		parts.push(format!(
			"\u{2191}{} \u{2193}{}",
			fmt_tokens(prompt_tokens),
			fmt_tokens(output_tokens)
		));
	}
	parts.join(" \u{00b7} ")
}

impl roku_plugin_telegram::TelegramInteractionHandler for RuntimeServiceTelegramHandler {
	fn handle_request(
		&self,
		mut request: RequestEnvelope,
	) -> Result<TelegramHandlerResponse, RuntimeError> {
		let binding_id = request.session_id.clone();
		if let Some(chat_id) = binding_chat_id(&binding_id)
			&& let Some(rename_response) =
				self.try_handle_pending_session_rename(chat_id, &binding_id, &request.goal)?
		{
			return Ok(rename_response);
		}

		let active_session = self.resolve_or_bootstrap_active_session(&binding_id)?;
		let session_id = active_session.session_id.clone();
		request.session_id = session_id.clone();
		request.conversation_history = self.transport_state.load_short_term_continuity(
			&session_id,
			self.session_ux_config.short_term_history_turn_limit,
		)?;
		self.transport_state.append_turn(
			&session_id,
			ConversationTurn {
				role: ConversationRole::User,
				content: request.goal.clone(),
				created_at_unix_ms: now_unix_ms(),
			},
		)?;

		// The TelegramInteractionHandler trait is sync but RuntimeService is async.
		// Bridge with a scoped thread + fresh multi-thread runtime to avoid nested
		// runtime context panics (the polling loop runs on the main thread, NOT
		// inside a tokio worker).
		let progress = self.progress_notices_enabled && self.bot_client.is_some();
		let bot_client = self.bot_client.as_ref().map(Arc::clone);
		// Bridge: scoped thread + fresh multi-thread runtime. The future must
		// run on an actual worker thread (via rt.spawn) so that block_in_place
		// inside execute_tool_loop works correctly. rt.block_on alone would run
		// the future on the driver thread where block_in_place panics.
		let service_clone = self.service.clone();
		let (execution, streaming) = std::thread::scope(|s| {
			s.spawn(|| {
				let rt = tokio::runtime::Builder::new_multi_thread()
					.enable_all()
					.build()
					.expect("telegram request runtime");
				let handle = if progress {
					let bot_client = bot_client.expect("bot_client checked above");
					let chat_id = binding_chat_id(&binding_id);
					let service = service_clone;
					rt.spawn(async move {
						let (tx, mut rx) =
							tokio::sync::mpsc::unbounded_channel::<roku_agent_runtime::LoopEvent>();
						// render_task returns (progress_message_id, content_message_id).
						let render_task = tokio::spawn(async move {
							let started_at = std::time::Instant::now();
							let mut progress_mid: i64 = -1;
							let mut content_mid: i64 = -1;

							// Immediately create the progress message.
							if let Some(cid) = chat_id {
								let msg = TelegramOutboundMessage {
									chat_id: cid,
									text: "\u{23f3} Processing...".to_string(),
									parse_mode: TelegramParseMode::PlainText,
									disable_web_page_preview: true,
									reply_markup: None,
								};
								let client = Arc::clone(&bot_client);
								if let Some(mid) = tokio::task::spawn_blocking(move || {
									client.send_message_with_id(&msg).ok()
								})
								.await
								.ok()
								.flatten()
								{
									progress_mid = mid;
								}
							}

							// Spawn a background loop that sends "typing" every 4 s.
							let typing_active = Arc::new(std::sync::atomic::AtomicBool::new(true));
							let typing_handle = if let Some(cid) = chat_id {
								let client = Arc::clone(&bot_client);
								let active = typing_active.clone();
								Some(tokio::task::spawn_blocking(move || {
									let mut ticks: u32 = 0;
									while active.load(std::sync::atomic::Ordering::Relaxed) {
										if ticks.is_multiple_of(8) {
											let _ = client.send_chat_action(cid, "typing");
										}
										std::thread::sleep(std::time::Duration::from_millis(500));
										ticks = ticks.wrapping_add(1);
									}
								}))
							} else {
								None
							};

							// Event consumer: accumulate state, throttle edits to <=1/sec.
							let mut accumulated_text = String::new();
							let mut current_step: u32 = 0;
							let mut current_tool: Option<String> = None;
							let mut prompt_tokens: u64 = 0;
							let mut output_tokens: u64 = 0;
							let mut progress_dirty = false;
							let mut content_dirty = false;
							let mut last_edit =
								std::time::Instant::now() - std::time::Duration::from_secs(2);

							while let Some(event) = rx.recv().await {
								match &event {
									roku_agent_runtime::LoopEvent::LlmTextDelta { text, step } => {
										accumulated_text.push_str(text);
										current_step = *step;
										content_dirty = true;
									}
									roku_agent_runtime::LoopEvent::LlmDecisionComplete {
										..
									} => {
										current_tool = None;
										progress_dirty = true;
									}
									roku_agent_runtime::LoopEvent::ToolStart {
										step,
										tool_name,
										..
									} => {
										current_step = *step;
										current_tool = Some(tool_name.clone());
										progress_dirty = true;
									}
									roku_agent_runtime::LoopEvent::ToolEnd { .. } => {
										current_tool = None;
										progress_dirty = true;
									}
									roku_agent_runtime::LoopEvent::TokenUsage {
										prompt_tokens: p,
										output_tokens: o,
										..
									} => {
										prompt_tokens += p;
										output_tokens += o;
										progress_dirty = true;
									}
									_ => {}
								}

								if let Some(cid) = chat_id
									&& last_edit.elapsed() >= std::time::Duration::from_secs(1)
								{
									// Update progress message (metadata only).
									if progress_dirty && progress_mid > 0 {
										let status = format_progress_status(
											current_step,
											current_tool.as_deref(),
											prompt_tokens,
											output_tokens,
										);
										let client = Arc::clone(&bot_client);
										let mid = progress_mid;
										let _ = tokio::task::spawn_blocking(move || {
											client.edit_message_text(cid, mid, &status, None)
										})
										.await;
										progress_dirty = false;
									}

									// Update content message (LLM text only).
									if content_dirty && !accumulated_text.is_empty() {
										let display_text = roku_plugin_telegram::markdown::strip_tool_call_xml(
											&accumulated_text,
										);
										let truncated = truncate_for_telegram(&display_text);
										let client = Arc::clone(&bot_client);
										if content_mid > 0 {
											let mid = content_mid;
											let _ = tokio::task::spawn_blocking(move || {
												client.edit_message_text(
													cid, mid, &truncated, None,
												)
											})
											.await;
										} else {
											let msg = TelegramOutboundMessage {
												chat_id: cid,
												text: truncated,
												parse_mode: TelegramParseMode::PlainText,
												disable_web_page_preview: true,
												reply_markup: None,
											};
											if let Some(mid) =
												tokio::task::spawn_blocking(move || {
													client.send_message_with_id(&msg).ok()
												})
												.await
												.ok()
												.flatten()
											{
												content_mid = mid;
											}
										}
										content_dirty = false;
									}

									last_edit = std::time::Instant::now();
								}
							}

							// Stop the typing indicator loop.
							typing_active.store(false, std::sync::atomic::Ordering::Relaxed);
							if let Some(handle) = typing_handle {
								let _ = handle.await;
							}

							StreamingState {
								progress_mid,
								content_mid,
								prompt_tokens,
								output_tokens,
								elapsed: started_at.elapsed(),
							}
						});
						let result = service
							.execute_with_mode(request, RunMode::Normal, Some(&tx))
							.await;
						drop(tx);
						let streaming = render_task.await.ok();
						(result, streaming)
					})
				} else {
					let service = service_clone;
					rt.spawn(async move { (service.execute(request).await, None) })
				};
				rt.block_on(handle)
					.expect("telegram request task should not panic")
			})
			.join()
			.expect("telegram request thread should not panic")
		});
		self.transport_state
			.clear_pending_loop_session_mirror(&session_id)?;

		match execution {
			Ok(response) => {
				self.transport_state.append_turn(
					&session_id,
					ConversationTurn {
						role: ConversationRole::Assistant,
						content: response.message.clone(),
						created_at_unix_ms: now_unix_ms(),
					},
				)?;
				// Check if the agent paused and is waiting for the user to reply.
				// If so, annotate the response so the user sees a clear indicator.
				let is_awaiting = self
					.service
					.pending_loop(&session_id)
					.ok()
					.flatten()
					.is_some();
				let mut handler_response = TelegramHandlerResponse::from(response);
				if is_awaiting {
					handler_response.response.message = format!(
						"{}\n\nRoku is waiting for your reply. Send your response to continue.",
						handler_response.response.message
					);
				}

				// Edit-in-place: replace the streaming content message with the
				// clean final answer (Markdown → HTML) so dispatch_response can
				// skip the duplicate. Long responses are chunked at 4096 chars.
				// Also edit the progress message to a compact summary.
				if let Some(ref ss) = streaming
					&& ss.content_mid > 0
					&& matches!(
						handler_response.response.status,
						ResponseStatus::Succeeded | ResponseStatus::Failed
					) && let Some(chat_id) = binding_chat_id(&binding_id)
					&& let Some(client) = self.bot_client.as_ref()
				{
					use roku_plugin_telegram::markdown::{
						chunk_message, markdown_to_telegram_html, TELEGRAM_MAX_MESSAGE_LEN,
					};
					let html =
						markdown_to_telegram_html(&handler_response.response.message);
					let chunks = chunk_message(&html, TELEGRAM_MAX_MESSAGE_LEN);

					// First chunk edits the existing content message.
					let edit_ok = chunks
						.first()
						.map(|first| {
							client
								.edit_message_text(chat_id, ss.content_mid, first, Some("HTML"))
								.is_ok()
						})
						.unwrap_or(false);

					if edit_ok {
						handler_response.delivered_via_streaming = true;
						// Remaining chunks are sent as new messages.
						for chunk in chunks.iter().skip(1) {
							let msg = TelegramOutboundMessage {
								chat_id,
								text: chunk.clone(),
								parse_mode: TelegramParseMode::Html,
								disable_web_page_preview: true,
								reply_markup: None,
							};
							let _ = client.send_message(&msg);
						}

						// Edit progress message to compact execution summary.
						if ss.progress_mid > 0 {
							let summary = format_compact_summary(
								ss.elapsed,
								ss.prompt_tokens,
								ss.output_tokens,
							);
							let _ = client.edit_message_text(
								chat_id,
								ss.progress_mid,
								&summary,
								None,
							);
						}
					}
				}

				Ok(handler_response)
			}
			Err(error) => {
				let error_summary = format!("Task could not be completed: {}", error.message);
				self.transport_state.append_turn(
					&session_id,
					ConversationTurn {
						role: ConversationRole::Assistant,
						content: error_summary.clone(),
						created_at_unix_ms: now_unix_ms(),
					},
				)?;
				Err(RuntimeError::new(error_summary))
			}
		}
	}

	fn handle_control_command(
		&self,
		command: TelegramControlCommandRequest,
	) -> Result<TelegramHandlerResponse, RuntimeError> {
		self.clear_pending_session_rename(command.chat_id)?;
		// Reject inline args for commands that do not allow them (e.g. /clear, /cancel).
		if command.argument.is_some() && !command.command.allows_inline_argument() {
			return Ok(self
				.control_command_response(
					command.command,
					ResponseStatus::Failed,
					format!(
						"`/{}` does not accept extra arguments. Use it on its own.",
						command.command.as_str()
					),
				)
				.into());
		}

		match command.command {
			TelegramControlCommand::Help => Ok(self
				.control_command_response(
					command.command,
					ResponseStatus::Succeeded,
					self.help_message(),
				)
				.into()),
			TelegramControlCommand::Status => {
				let active_session = self
					.transport_state
					.get_active_session(&command.session_id)?;
				let snapshot = match active_session {
					Some(active_session) => Some(self.session_snapshot(&active_session)?),
					None => None,
				};
				Ok(self
					.control_command_response(
						command.command,
						ResponseStatus::Succeeded,
						self.status_message(snapshot.as_ref()),
					)
					.into())
			}
			TelegramControlCommand::Sessions => {
				self.sessions_response(&command.session_id, 0, None, None)
			}
			TelegramControlCommand::Cancel => {
				let Some(active_session) = self
					.transport_state
					.get_active_session(&command.session_id)?
				else {
					return Ok(self
						.control_command_response(
							command.command,
							ResponseStatus::Succeeded,
							self.cancel_without_active_message(),
						)
						.into());
				};
				let snapshot = self.session_snapshot(&active_session)?;
				if snapshot.pending_run_id.is_none() {
					return Ok(self
						.control_command_response(
							command.command,
							ResponseStatus::Succeeded,
							self.cancel_message(Some(&snapshot), false),
						)
						.into());
				}
				self.service
					.clear_pending_loop(&active_session.session_id)?;
				self.transport_state
					.clear_pending_loop_session_mirror(&active_session.session_id)?;
				let snapshot = self.session_snapshot(&active_session)?;
				Ok(self
					.control_command_response(
						command.command,
						ResponseStatus::Succeeded,
						self.cancel_message(Some(&snapshot), true),
					)
					.into())
			}
			TelegramControlCommand::Clear => {
				let Some(active_session) = self
					.transport_state
					.get_active_session(&command.session_id)?
				else {
					return Ok(self
						.control_command_response(
							command.command,
							ResponseStatus::Succeeded,
							self.clear_without_active_message(),
						)
						.into());
				};
				self.service
					.clear_pending_loop(&active_session.session_id)?;
				self.transport_state
					.clear_transport_session(&active_session.session_id)?;
				let snapshot = self.session_snapshot(&active_session)?;
				Ok(self
					.control_command_response(
						command.command,
						ResponseStatus::Succeeded,
						self.clear_message(Some(&snapshot)),
					)
					.into())
			}
			TelegramControlCommand::New => {
				let descriptor = self.create_and_select_session(
					&command.session_id,
					SessionCreateRequest::default(),
				)?;
				Ok(self
					.control_command_response(
						command.command,
						ResponseStatus::Succeeded,
						format!(
							"Created and switched to a new session.\nSession: {}\nName: {}",
							descriptor.session_id, descriptor.name
						),
					)
					.into())
			}
			TelegramControlCommand::Delete => {
				let Some(active_session) = self
					.transport_state
					.get_active_session(&command.session_id)?
				else {
					return Ok(self
						.control_command_response(
							command.command,
							ResponseStatus::Succeeded,
							"No active session is currently selected for this chat.".to_string(),
						)
						.into());
				};
				Ok(TelegramHandlerResponse::from(self.control_command_response(
					command.command,
					ResponseStatus::Succeeded,
					format!(
						"Delete the current active session?\nSession: {}\nName: {}\nThis removes session state, short-term continuity, and pending-loop snapshots for that session. Long-term memory is kept.",
						active_session.session_id, active_session.name
					),
				))
				.with_reply_markup(delete_session_confirmation_markup(
					&active_session.session_id,
				)))
			}
			TelegramControlCommand::SessionSetting => {
				let Some(active_session) = self
					.transport_state
					.get_active_session(&command.session_id)?
				else {
					return Ok(self
						.control_command_response(
							command.command,
							ResponseStatus::Failed,
							"No active session is currently selected for this chat.".to_string(),
						)
						.into());
				};
				self.set_pending_session_rename(
					command.chat_id,
					PendingRenameState {
						target_session_id: active_session.session_id.clone(),
					},
				)?;
				Ok(self
					.control_command_response(
						command.command,
						ResponseStatus::Succeeded,
						format!(
							"Send the new name for the current session.\nCurrent session: {}\nCurrent name: {}\nName length must be {}..={} Unicode characters.",
							active_session.session_id,
							active_session.name,
							SESSION_NAME_MIN_CHARS,
							SESSION_NAME_MAX_CHARS
						),
					)
					.into())
			}
			TelegramControlCommand::Compact => {
				let Some(active_session) = self
					.transport_state
					.get_active_session(&command.session_id)?
				else {
					return Ok(self
						.control_command_response(
							command.command,
							ResponseStatus::Succeeded,
							"No active session is currently selected for this chat.".to_string(),
						)
						.into());
				};
				let session_id = &active_session.session_id;
				// Load all stored turns for the session (no cap — we need the full history).
				let mut history = self
					.transport_state
					.load_short_term_continuity(session_id, usize::MAX)?;
				match compact_conversation_history(&mut history) {
					None => Ok(self
						.control_command_response(
							command.command,
							ResponseStatus::Succeeded,
							format!(
								"Conversation history has {} turns — nothing to compact.",
								history.len()
							),
						)
						.into()),
					Some(result) => {
						// Replace stored turns with the compacted set.
						self.transport_state
							.replace_continuity(session_id, &history)?;
						Ok(self
							.control_command_response(
								command.command,
								ResponseStatus::Succeeded,
								format!(
									"Compacted {} turns into a summary. {} turns remain.",
									result.discarded, result.retained
								),
							)
							.into())
					}
				}
			}
		}
	}

	fn handle_approval_decision(
		&self,
		approval_id: ApprovalId,
		decision: ApprovalDecision,
	) -> Result<TelegramHandlerResponse, RuntimeError> {
		self.service
			.decide_approval(&approval_id, decision)
			.map(Into::into)
	}

	fn handle_session_callback(
		&self,
		action: TelegramSessionCallbackAction,
	) -> Result<TelegramHandlerResponse, RuntimeError> {
		let binding_id = action.chat_id.to_string();
		match action.action {
			TelegramSessionCallbackKind::ShowPage { page } => {
				self.sessions_response(&binding_id, page, None, None)
			}
			TelegramSessionCallbackKind::SelectSession { session_id, page } => {
				let descriptor = self
					.transport_state
					.select_active_session(&binding_id, &session_id)?;
				self.sessions_response(
					&binding_id,
					page,
					Some(format!(
						"Switched active session to {} ({})",
						descriptor.name, descriptor.session_id
					)),
					None,
				)
			}
			TelegramSessionCallbackKind::DeleteCancel => Ok(self
				.control_command_response(
					TelegramControlCommand::Delete,
					ResponseStatus::Succeeded,
					"Session deletion cancelled.".to_string(),
				)
				.into()),
			TelegramSessionCallbackKind::DeleteConfirm { session_id } => {
				let Some(active_session) = self.transport_state.get_active_session(&binding_id)?
				else {
					return Ok(self
						.control_command_response(
							TelegramControlCommand::Delete,
							ResponseStatus::Failed,
							"No active session is currently selected for this chat.".to_string(),
						)
						.into());
				};
				if active_session.session_id != session_id {
					return Ok(self
						.control_command_response(
							TelegramControlCommand::Delete,
							ResponseStatus::Failed,
							"Active session changed before confirmation. Run /delete again."
								.to_string(),
						)
						.into());
				}

				self.transport_state.delete_session(
					&binding_id,
					&active_session.session_id,
					SessionDeleteMode::RetainLongTermMemory,
				)?;
				match self.create_and_select_session(
					&binding_id,
					SessionCreateRequest::default(),
				) {
					Ok(new_session) => Ok(self
						.control_command_response(
							TelegramControlCommand::Delete,
							ResponseStatus::Succeeded,
							format!(
								"Deleted the previous session and switched to a new one.\nSession: {}\nName: {}",
								new_session.session_id, new_session.name
							),
						)
						.into()),
					Err(error) => Ok(self
						.control_command_response(
							TelegramControlCommand::Delete,
							ResponseStatus::Failed,
							format!(
								"Deleted the previous session, but failed to create and select a replacement session: {}\nThe chat currently has no active session. Sending a normal message will bootstrap a new one.",
								error.message
							),
						)
						.into()),
				}
			}
		}
	}
}

impl RuntimeServiceTelegramHandler {
	fn session_snapshot(
		&self,
		descriptor: &SessionDescriptor,
	) -> Result<TelegramSessionSnapshot, RuntimeError> {
		self.transport_state.status_snapshot(
			descriptor,
			&self.session_ux_config,
			self.service
				.pending_loop(&descriptor.session_id)?
				.map(|loop_state| loop_state.run_id),
		)
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
			"/new - Create and switch to a new session for this chat.".to_string(),
			"/status - Show the current chat session status.".to_string(),
			"/sessions - Show and switch the sessions bound to this chat.".to_string(),
			"/session-setting - Rename the current active session.".to_string(),
			"/delete - Delete the current active session after confirmation.".to_string(),
			"/cancel - Cancel the current pending loop without clearing chat history.".to_string(),
			"/clear - Clear the current chat session state and pending loop.".to_string(),
			"/compact - Compact older conversation turns into a summary.".to_string(),
			"".to_string(),
			"Send natural language directly to start a task.".to_string(),
		]
		.join("\n")
	}

	fn status_message(&self, snapshot: Option<&TelegramSessionSnapshot>) -> String {
		let mut lines = vec!["Current chat session status".to_string()];
		match snapshot {
			Some(snapshot) => append_session_snapshot_lines(&mut lines, snapshot),
			None => lines.push("Active session: none".to_string()),
		}
		append_runtime_mode_lines(&mut lines, &self.service.runtime_mode_report());
		lines.join("\n")
	}

	fn sessions_message(
		&self,
		active_snapshot: Option<&TelegramSessionSnapshot>,
		sessions: &[SessionSummary],
		page: usize,
		total_pages: usize,
		header_note: Option<&str>,
	) -> String {
		let mut lines = Vec::new();
		if let Some(header_note) = header_note {
			lines.push(header_note.to_string());
			lines.push(String::new());
		}
		lines.push("Current chat sessions".to_string());
		match active_snapshot {
			Some(snapshot) => append_active_session_summary_lines(&mut lines, snapshot),
			None => lines.push("Active session: none".to_string()),
		}
		lines.push(String::new());
		if sessions.is_empty() {
			lines.push("No sessions exist for this chat yet. Use /new to create one.".to_string());
			return lines.join("\n");
		}
		lines.push(format!(
			"Sessions page {}/{}",
			page.saturating_add(1),
			total_pages.max(1)
		));
		for session in sessions {
			lines.push(format!("• {} ({})", session.name, session.session_id));
		}
		lines.join("\n")
	}

	fn cancel_message(
		&self,
		snapshot: Option<&TelegramSessionSnapshot>,
		cancelled: bool,
	) -> String {
		let mut lines = if cancelled {
			vec![
				"Cancelled the pending runtime loop for this chat session.".to_string(),
				"Conversation history, approvals, tasks, artifacts, and completed observations were left untouched."
					.to_string(),
			]
		} else {
			vec!["No pending runtime loop is currently stored for this chat session.".to_string()]
		};
		if let Some(snapshot) = snapshot {
			append_session_snapshot_lines(&mut lines, snapshot);
		}
		lines.join("\n")
	}

	fn clear_message(&self, snapshot: Option<&TelegramSessionSnapshot>) -> String {
		let mut lines = vec!["Cleared the current chat session state.".to_string()];
		if let Some(snapshot) = snapshot {
			append_session_snapshot_lines(&mut lines, snapshot);
		}
		lines.push(
			"Only Telegram session state was cleared. Runtime global config and cross-channel shared state were left untouched."
				.to_string(),
		);
		lines.push("Stored task records and artifacts were not deleted.".to_string());
		lines.join("\n")
	}

	fn cancel_without_active_message(&self) -> String {
		"No active session is currently selected for this chat, so there is no pending loop to cancel."
			.to_string()
	}

	fn clear_without_active_message(&self) -> String {
		"No active session is currently selected for this chat, so there is no session state to clear."
			.to_string()
	}

	fn resolve_or_bootstrap_active_session(
		&self,
		binding_id: &str,
	) -> Result<SessionDescriptor, RuntimeError> {
		if let Some(active_session) = self.transport_state.get_active_session(binding_id)? {
			return Ok(active_session);
		}
		self.create_and_select_session(binding_id, SessionCreateRequest::default())
	}

	fn create_and_select_session(
		&self,
		binding_id: &str,
		request: SessionCreateRequest,
	) -> Result<SessionDescriptor, RuntimeError> {
		let descriptor = self.transport_state.create_session(binding_id, request)?;
		self.transport_state
			.select_active_session(binding_id, &descriptor.session_id)
	}

	fn sessions_response(
		&self,
		binding_id: &str,
		requested_page: usize,
		header_note: Option<String>,
		override_active_session: Option<SessionDescriptor>,
	) -> Result<TelegramHandlerResponse, RuntimeError> {
		let active_session = match override_active_session {
			Some(active_session) => Some(active_session),
			None => self.transport_state.get_active_session(binding_id)?,
		};
		let active_snapshot = match active_session.as_ref() {
			Some(active_session) => Some(self.session_snapshot(active_session)?),
			None => None,
		};
		let mut sessions = self.transport_state.list_sessions(binding_id)?;
		sessions.sort_by(|left, right| {
			right
				.updated_at_unix_ms
				.cmp(&left.updated_at_unix_ms)
				.then_with(|| left.session_id.cmp(&right.session_id))
		});
		let page_state = paginate_sessions(
			&sessions,
			requested_page,
			self.session_ux_config.sessions_page_size,
		);
		let page_sessions = sessions[page_state.start..page_state.end].to_vec();
		let mut response = TelegramHandlerResponse::from(self.control_command_response(
			TelegramControlCommand::Sessions,
			ResponseStatus::Succeeded,
			self.sessions_message(
				active_snapshot.as_ref(),
				&page_sessions,
				page_state.page,
				page_state.total_pages,
				header_note.as_deref(),
			),
		));
		if let Some(markup) = build_sessions_markup(
			&sessions,
			active_snapshot
				.as_ref()
				.map(|snapshot| snapshot.session_id.as_str()),
			page_state.page,
			self.session_ux_config.sessions_page_size,
		) {
			response = response.with_reply_markup(markup);
		}
		Ok(response)
	}

	fn try_handle_pending_session_rename(
		&self,
		chat_id: i64,
		binding_id: &str,
		candidate_name: &str,
	) -> Result<Option<TelegramHandlerResponse>, RuntimeError> {
		let Some(pending) = self.pending_session_rename(chat_id)? else {
			return Ok(None);
		};
		let normalized_name = match normalize_session_name(candidate_name) {
			Ok(value) => value,
			Err(SessionManagementError::Validation(message)) => {
				return Ok(Some(
					self.control_command_response(
						TelegramControlCommand::SessionSetting,
						ResponseStatus::Failed,
						format!(
							"Session name is invalid: {message}\nSend another text value, or use another slash command to cancel rename."
						),
					)
					.into(),
				));
			}
			Err(error) => return Err(runtime_session_management_error(error)),
		};

		match self.transport_state.rename_session(
			binding_id,
			&pending.target_session_id,
			&normalized_name,
		) {
			Ok(descriptor) => {
				self.clear_pending_session_rename(chat_id)?;
				Ok(Some(
					self.control_command_response(
						TelegramControlCommand::SessionSetting,
						ResponseStatus::Succeeded,
						format!(
							"Renamed the current session.\nSession: {}\nName: {}",
							descriptor.session_id, descriptor.name
						),
					)
					.into(),
				))
			}
			Err(RuntimeError { message })
				if message.contains("validation")
					|| message.contains("must not be empty")
					|| message.contains("must be between") =>
			{
				Ok(Some(
					self.control_command_response(
						TelegramControlCommand::SessionSetting,
						ResponseStatus::Failed,
						format!(
							"Session name is invalid: {message}\nSend another text value, or use another slash command to cancel rename."
						),
					)
					.into(),
				))
			}
			Err(error) => {
				self.clear_pending_session_rename(chat_id)?;
				Ok(Some(
					self.control_command_response(
						TelegramControlCommand::SessionSetting,
						ResponseStatus::Failed,
						format!(
							"Failed to rename the current session: {}\nRename mode was cleared.",
							error.message
						),
					)
					.into(),
				))
			}
		}
	}

	fn set_pending_session_rename(
		&self,
		chat_id: i64,
		state: PendingRenameState,
	) -> Result<(), RuntimeError> {
		self.pending_session_rename_by_chat
			.lock()
			.map_err(|_| RuntimeError::new("telegram session rename state is poisoned"))?
			.insert(chat_id, state);
		Ok(())
	}

	fn clear_pending_session_rename(&self, chat_id: i64) -> Result<(), RuntimeError> {
		self.pending_session_rename_by_chat
			.lock()
			.map_err(|_| RuntimeError::new("telegram session rename state is poisoned"))?
			.remove(&chat_id);
		Ok(())
	}

	fn pending_session_rename(
		&self,
		chat_id: i64,
	) -> Result<Option<PendingRenameState>, RuntimeError> {
		Ok(self
			.pending_session_rename_by_chat
			.lock()
			.map_err(|_| RuntimeError::new("telegram session rename state is poisoned"))?
			.get(&chat_id)
			.cloned())
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SessionPageState {
	page: usize,
	start: usize,
	end: usize,
	total_pages: usize,
}

fn paginate_sessions(
	sessions: &[SessionSummary],
	requested_page: usize,
	page_size: usize,
) -> SessionPageState {
	if sessions.is_empty() || page_size == 0 {
		return SessionPageState {
			page: 0,
			start: 0,
			end: 0,
			total_pages: 1,
		};
	}

	let total_pages = sessions.len().div_ceil(page_size);
	let page = requested_page.min(total_pages.saturating_sub(1));
	let start = page.saturating_mul(page_size);
	let end = (start + page_size).min(sessions.len());
	SessionPageState {
		page,
		start,
		end,
		total_pages,
	}
}

fn build_sessions_markup(
	sessions: &[SessionSummary],
	active_session_id: Option<&str>,
	requested_page: usize,
	page_size: usize,
) -> Option<TelegramReplyMarkup> {
	if sessions.is_empty() {
		return None;
	}
	let page_state = paginate_sessions(sessions, requested_page, page_size);
	let mut inline_keyboard = sessions[page_state.start..page_state.end]
		.iter()
		.map(|session| {
			let marker = if active_session_id.is_some_and(|active| active == session.session_id) {
				"✅ "
			} else {
				""
			};
			vec![TelegramInlineKeyboardButton {
				text: format!("{marker}{}", session.name),
				callback_data: session_select_callback_data(page_state.page, &session.session_id),
			}]
		})
		.collect::<Vec<_>>();
	let mut pager_row = Vec::new();
	if page_state.page > 0 {
		pager_row.push(TelegramInlineKeyboardButton {
			text: "Prev".to_string(),
			callback_data: session_page_callback_data(page_state.page - 1),
		});
	}
	if page_state.page + 1 < page_state.total_pages {
		pager_row.push(TelegramInlineKeyboardButton {
			text: "Next".to_string(),
			callback_data: session_page_callback_data(page_state.page + 1),
		});
	}
	if !pager_row.is_empty() {
		inline_keyboard.push(pager_row);
	}
	Some(TelegramReplyMarkup { inline_keyboard })
}

fn delete_session_confirmation_markup(session_id: &str) -> TelegramReplyMarkup {
	TelegramReplyMarkup {
		inline_keyboard: vec![vec![
			TelegramInlineKeyboardButton {
				text: "✅ Confirm".to_string(),
				callback_data: session_delete_confirm_callback_data(session_id),
			},
			TelegramInlineKeyboardButton {
				text: "❌ Cancel".to_string(),
				callback_data: session_delete_cancel_callback_data(),
			},
		]],
	}
}

fn binding_chat_id(binding_id: &str) -> Option<i64> {
	binding_id.parse::<i64>().ok()
}

/// Appends human-readable snapshot lines for `/status`; order is fixed for tests.
fn append_session_snapshot_lines(lines: &mut Vec<String>, snapshot: &TelegramSessionSnapshot) {
	lines.push(format!("Active session name: {}", snapshot.session_name));
	lines.push(format!("Active session id: {}", snapshot.session_id));
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

/// Appends the minimal active-session summary shown by `/sessions`.
fn append_active_session_summary_lines(
	lines: &mut Vec<String>,
	snapshot: &TelegramSessionSnapshot,
) {
	lines.push(format!("Active session name: {}", snapshot.session_name));
	lines.push(format!("Active session id: {}", snapshot.session_id));
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

fn latest_activity_summary(
	turn: &ConversationTurn,
	session_ux_config: &TelegramSessionUxConfig,
) -> String {
	format!(
		"{}: {}",
		match turn.role {
			ConversationRole::User => "user",
			ConversationRole::Assistant => "assistant",
			ConversationRole::System => "system",
		},
		truncate_preview(
			&turn.content,
			session_ux_config.latest_activity_preview_chars,
		)
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
	response: Result<TelegramHandlerResponse, RuntimeError>,
) -> Result<String, CommandError> {
	let preview = match response {
		Ok(response) => {
			let outbound = TelegramOutboundMessage::from_handler_response(chat_id, &response);
			json!({
				"runtime_status": format!("{:?}", response.response.status),
				"request_id": response.response.request_id.0,
				"telegram_message": outbound.text,
				"parse_mode": telegram_parse_mode_label(outbound.parse_mode),
				"has_reply_markup": outbound.reply_markup.is_some(),
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
		TelegramParseMode::Html => "HTML",
	}
}

/// Telegram-scoped transport state: session state and conversation history.
///
/// Consumed by [`RuntimeServiceTelegramHandler`] to load/append turns and to manage Telegram-owned
/// session metadata. The runtime service owns pending-loop truth on the shared substrate.
pub(crate) struct TelegramTransportState {
	/// Session-scoped transport state; one store per process.
	session_state_store: Mutex<Box<dyn SessionStateBackend + Send>>,
	/// Conversation turns per session_id; one store per process.
	conversation_store: Mutex<Box<dyn ShortTermContinuityBackend + Send>>,
	/// Provider-neutral session-management store used to resolve/create/select sessions.
	session_management_store: Mutex<Box<dyn SessionManagementBackend + Send>>,
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
		session_management_store: Box<dyn SessionManagementBackend + Send>,
	) -> Self {
		Self {
			session_state_store: Mutex::new(session_state_store),
			conversation_store: Mutex::new(conversation_store),
			session_management_store: Mutex::new(session_management_store),
		}
	}

	fn from_memory_subsystem(subsystem: ResolvedMemorySubsystem) -> Self {
		Self::new(
			subsystem.session_state,
			subsystem.short_term,
			subsystem.session_management,
		)
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

	/// Replaces all stored conversation turns for a session with the provided list.
	///
	/// Used by `/compact` to atomically swap the full history with the compacted version.
	fn replace_continuity(
		&self,
		session_id: &str,
		turns: &[ConversationTurn],
	) -> Result<(), RuntimeError> {
		let mut store = self.lock_conversation_store()?;
		store
			.delete_continuity(session_id)
			.map_err(runtime_short_term_error)?;
		for turn in turns {
			store
				.append_continuity_turn(session_id, turn.clone())
				.map_err(runtime_short_term_error)?;
		}
		Ok(())
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

	fn clear_pending_loop_session_mirror(&self, session_id: &str) -> Result<(), RuntimeError> {
		let mut session_state = self.load_session_state_or_default(session_id)?;
		if session_state.pending_loop.is_none() {
			return Ok(());
		}
		session_state.pending_loop = None;
		self.save_session_state(session_id, session_state)
	}

	/// Builds the stable minimal snapshot exposed by `/status` and `/sessions`.
	///
	/// The snapshot is intentionally small and deterministic so Telegram control surfaces can stay
	/// testable even if the underlying repositories evolve.
	fn status_snapshot(
		&self,
		descriptor: &SessionDescriptor,
		session_ux_config: &TelegramSessionUxConfig,
		pending_run_id: Option<String>,
	) -> Result<TelegramSessionSnapshot, RuntimeError> {
		let turns = self.load_short_term_continuity(
			&descriptor.session_id,
			session_ux_config.short_term_history_turn_limit,
		)?;
		Ok(TelegramSessionSnapshot {
			session_id: descriptor.session_id.clone(),
			session_name: descriptor.name.clone(),
			pending_run_id,
			recent_turn_count: turns.len(),
			latest_activity: turns
				.last()
				.map(|turn| latest_activity_summary(turn, session_ux_config)),
		})
	}

	fn create_session(
		&self,
		binding_id: &str,
		request: SessionCreateRequest,
	) -> Result<SessionDescriptor, RuntimeError> {
		self.lock_session_management_store()?
			.create_session(binding_id, request)
			.map_err(runtime_session_management_error)
	}

	fn get_active_session(
		&self,
		binding_id: &str,
	) -> Result<Option<SessionDescriptor>, RuntimeError> {
		self.lock_session_management_store()?
			.get_active_session(binding_id)
			.map_err(runtime_session_management_error)
	}

	fn list_sessions(&self, binding_id: &str) -> Result<Vec<SessionSummary>, RuntimeError> {
		self.lock_session_management_store()?
			.list_sessions(binding_id)
			.map_err(runtime_session_management_error)
	}

	fn rename_session(
		&self,
		binding_id: &str,
		session_id: &str,
		new_name: &str,
	) -> Result<SessionDescriptor, RuntimeError> {
		self.lock_session_management_store()?
			.rename_session(binding_id, session_id, new_name)
			.map_err(runtime_session_management_error)
	}

	fn delete_session(
		&self,
		binding_id: &str,
		session_id: &str,
		mode: SessionDeleteMode,
	) -> Result<(), RuntimeError> {
		self.lock_session_management_store()?
			.delete_session(binding_id, session_id, mode)
			.map_err(runtime_session_management_error)
	}

	fn select_active_session(
		&self,
		binding_id: &str,
		session_id: &str,
	) -> Result<SessionDescriptor, RuntimeError> {
		self.lock_session_management_store()?
			.select_active_session(binding_id, session_id)
			.map_err(runtime_session_management_error)
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

	fn lock_session_management_store(
		&self,
	) -> Result<std::sync::MutexGuard<'_, Box<dyn SessionManagementBackend + Send>>, RuntimeError>
	{
		self.session_management_store
			.lock()
			.map_err(|_| RuntimeError::new("session management store is poisoned"))
	}
}

impl Default for TelegramTransportState {
	/// In-memory backends only; for tests. Production uses [`TelegramTransportState::from_env`].
	fn default() -> Self {
		Self::new(
			Box::new(InMemorySessionStateBackend::default()),
			Box::new(InMemoryShortTermContinuityBackend::default()),
			Box::new(InMemorySessionManagementBackend::default()),
		)
	}
}

fn runtime_session_state_error(error: SessionStateError) -> RuntimeError {
	RuntimeError::new(error.to_string())
}

fn runtime_short_term_error(error: ShortTermContinuityError) -> RuntimeError {
	RuntimeError::new(error.to_string())
}

fn runtime_session_management_error(error: SessionManagementError) -> RuntimeError {
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
	use std::collections::HashMap;
	use std::io::{Cursor, Write};
	use std::sync::{Arc, Mutex};

	use async_trait::async_trait;
	use roku_agent_runtime::RuntimeService;
	use roku_agent_runtime::{
		AskUserPayload, GenericAgentRuntime, IntentFamily, LoopContext, LoopState, LoopStatus,
		RouteDecision, RouteRisk,
	};
	use roku_common_types::{
		RequestEnvelope, RequestId, ResourceSelector, ResponseStatus, RuntimeLoopTrace, TaskId,
		TaskState,
	};
	use roku_memory::{
		NoopLongTermMemoryBackend, NoopPendingLoopSnapshotBackend, PendingLoopSnapshot,
		PendingLoopSnapshotBackend, PendingLoopSnapshotError,
	};
	use roku_plugin_llm::{
		GenerationRequest, LlmProvider, LlmRouter, ModelProfile, ProviderCallError,
		ProviderResponse, RiskTier, RoutingPolicy,
	};
	use roku_plugin_skills::{
		DownloadedArchive, SkillArchiveFetcher, SkillRegistry, SkillRegistryError, SkillSource,
	};
	use roku_plugin_telegram::TelegramInteractionHandler;
	use serde_json::Value;

	use super::*;
	use crate::pending_loop_substrate::MemoryPendingLoopSnapshotStore;
	use crate::test_support::pending_inventory_resume_success_loop_state;

	struct SessionAwareLlmProvider;

	#[derive(Clone, Default)]
	struct RecordingPendingLoopSnapshotBackend {
		snapshots: Arc<Mutex<HashMap<String, PendingLoopSnapshot>>>,
		events: Arc<Mutex<Vec<String>>>,
	}

	impl RecordingPendingLoopSnapshotBackend {
		fn seed(&self, session_id: &str, snapshot: PendingLoopSnapshot) {
			self.snapshots
				.lock()
				.expect("snapshot seed lock should not be poisoned")
				.insert(session_id.to_string(), snapshot);
		}

		fn snapshot(&self, session_id: &str) -> Option<PendingLoopSnapshot> {
			self.snapshots
				.lock()
				.expect("snapshot load lock should not be poisoned")
				.get(session_id)
				.cloned()
		}

		fn events(&self) -> Vec<String> {
			self.events
				.lock()
				.expect("event log lock should not be poisoned")
				.clone()
		}
	}

	impl PendingLoopSnapshotBackend for RecordingPendingLoopSnapshotBackend {
		fn load_pending_loop_snapshot(
			&self,
			session_id: &str,
		) -> Result<Option<PendingLoopSnapshot>, PendingLoopSnapshotError> {
			self.events
				.lock()
				.expect("event log lock should not be poisoned")
				.push(format!("load:{session_id}"));
			Ok(self.snapshot(session_id))
		}

		fn save_pending_loop_snapshot(
			&self,
			session_id: &str,
			snapshot: Option<PendingLoopSnapshot>,
		) -> Result<(), PendingLoopSnapshotError> {
			let event = if snapshot.is_some() { "save" } else { "clear" };
			self.events
				.lock()
				.expect("event log lock should not be poisoned")
				.push(format!("{event}:{session_id}"));
			let mut snapshots = self
				.snapshots
				.lock()
				.expect("snapshot save lock should not be poisoned");
			if let Some(snapshot) = snapshot {
				snapshots.insert(session_id.to_string(), snapshot);
			} else {
				snapshots.remove(session_id);
			}
			Ok(())
		}
	}

	#[derive(Clone)]
	struct StaticArchiveFetcher {
		archive: DownloadedArchive,
	}

	impl SkillArchiveFetcher for StaticArchiveFetcher {
		fn fetch(&self, _source: &SkillSource) -> Result<DownloadedArchive, SkillRegistryError> {
			Ok(self.archive.clone())
		}
	}

	#[async_trait]
	impl LlmProvider for SessionAwareLlmProvider {
		fn provider_name(&self) -> &'static str {
			"session-test-provider"
		}

		async fn complete(
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
				tool_calls: None,
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
				Box::new(InMemorySessionManagementBackend::default()),
			));
		let session_id = "telegram-seam-session";
		let state = SessionState { pending_loop: None };

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

	#[tokio::test(flavor = "multi_thread")]
	#[ignore = "needs mock LLM responses updated for message-based turn loop"]
	async fn telegram_handler_keeps_short_term_continuity_across_regular_requests() {
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
			session_ux_config: TelegramSessionUxConfig::default(),
			pending_session_rename_by_chat: Mutex::new(HashMap::new()),
			bot_client: None,
			progress_notices_enabled: false,
		};
		let binding_id = "telegram-session-1";

		let first = handler
			.handle_request(request(binding_id, "今天周几？"))
			.expect("first request should succeed");
		assert_eq!(first.response.status, ResponseStatus::Succeeded);
		assert_eq!(first.response.message, "今天是星期三。");

		let second = handler
			.handle_request(request(binding_id, "沙县小吃是什么？"))
			.expect("second request should succeed");
		assert_eq!(second.response.status, ResponseStatus::Succeeded);
		assert!(second.response.message.contains("福建沙县"));

		let third = handler
			.handle_request(request(binding_id, "我刚问了你什么？"))
			.expect("third request should succeed");
		assert_eq!(third.response.status, ResponseStatus::Succeeded);
		assert_eq!(third.response.message, "你刚才问的是：沙县小吃是什么？");

		let session = active_session(&handler, binding_id);

		let turns = handler
			.transport_state
			.load_short_term_continuity(&session.session_id, 8)
			.expect("turns should load");
		assert_eq!(turns.len(), 6);
		assert_eq!(turns[0].content, "今天周几？");
		assert_eq!(turns[2].content, "沙县小吃是什么？");
		assert_eq!(turns[4].content, "我刚问了你什么？");

		// LlmRouter holds a blocking tokio runtime. Dropping it inside an async context panics
		// ("Cannot drop a runtime in a context where blocking is not allowed"). Use block_in_place
		// to drop it in a blocking-allowed scope.
		tokio::task::block_in_place(|| drop(handler));
	}

	#[tokio::test(flavor = "multi_thread")]
	async fn telegram_request_flow_resumes_pending_loop_snapshots_from_shared_memory_substrate() {
		let backend = RecordingPendingLoopSnapshotBackend::default();
		let handler = test_handler_with_service(
			RuntimeService::default().with_pending_loop_snapshot_store(Arc::new(
				MemoryPendingLoopSnapshotStore::new(Box::new(backend.clone())),
			)),
		);
		let binding_id = "telegram-shared-pending-loop";
		let session = bootstrap_session(&handler, binding_id);
		let (mut pending_loop, selected_topic) = pending_inventory_resume_success_loop_state();
		pending_loop.session_id = session.session_id.clone();
		backend.seed(
			&session.session_id,
			PendingLoopSnapshot {
				run_id: pending_loop.run_id.clone(),
				loop_state_json: serde_json::to_string(&pending_loop)
					.expect("pending loop should encode"),
			},
		);

		let response = handler
			.handle_request(request(binding_id, &selected_topic))
			.expect("telegram request should resume through the shared substrate");

		assert_eq!(response.response.status, ResponseStatus::Succeeded);
		let task_id = TaskId(format!("task-{}", response.response.request_id.0));
		let task = handler
			.service
			.get_task(&task_id)
			.expect("task lookup should succeed")
			.expect("resumed telegram task should be persisted");
		assert_eq!(task.state, TaskState::Succeeded);
		let last_result = task
			.last_result
			.as_ref()
			.expect("successful resumed telegram task should persist a terminal result");
		let payload = serde_json::from_str::<Value>(&last_result.payload)
			.expect("terminal payload should decode");
		let trace: RuntimeLoopTrace = serde_json::from_value(payload["probe_trace"].clone())
			.expect("probe trace should decode");
		assert_eq!(trace.status, "succeeded");
		assert_eq!(
			trace.final_outcome.terminal_action.as_deref(),
			Some("final_answer")
		);
		assert!(trace.steps.iter().all(|step| {
			step.visible_resources_before.as_ref()
				== Some(&vec![ResourceSelector::tool(
					"inventory.describe".to_string(),
				)])
		}));
		assert!(
			backend.snapshot(&session.session_id).is_none(),
			"Telegram request flow should consume the shared pending loop snapshot"
		);
		assert!(
			handler
				.transport_state
				.load_session_state_or_default(&session.session_id)
				.expect("session state should load")
				.pending_loop
				.is_none(),
			"Telegram request flow should not persist a Telegram-only pending-loop mirror"
		);
		let turns = handler
			.transport_state
			.load_short_term_continuity(&session.session_id, 8)
			.expect("turns should load");
		assert_eq!(turns.len(), 2);
		assert_eq!(turns[0].content, selected_topic);
		assert_eq!(turns[1].content, response.response.message);

		let events = backend.events();
		assert!(
			events.contains(&format!("load:{}", session.session_id)),
			"shared pending loop substrate should be read through the memory adapter"
		);
		assert!(
			events.contains(&format!("clear:{}", session.session_id)),
			"shared pending loop substrate should clear the consumed snapshot"
		);
	}

	#[test]
	fn telegram_control_cancel_clears_pending_loop_without_clearing_continuity() {
		let handler = test_handler();
		let binding_id = "telegram-control-cancel";
		let session = bootstrap_session(&handler, binding_id);
		handler
			.transport_state
			.append_turn(
				&session.session_id,
				ConversationTurn {
					role: ConversationRole::User,
					content: "hello".to_string(),
					created_at_unix_ms: 1,
				},
			)
			.expect("conversation turn should save");
		let loop_state = awaiting_user_loop_state(&session.session_id, "loop-cancel-1");
		handler
			.service
			.restore_pending_loop(loop_state)
			.expect("pending loop should restore");

		let response = handler
			.handle_control_command(control_command(binding_id, TelegramControlCommand::Cancel))
			.expect("cancel command should succeed");
		assert_eq!(response.response.status, ResponseStatus::Succeeded);
		assert!(
			response
				.response
				.message
				.contains("Cancelled the pending runtime loop")
		);
		assert!(
			handler
				.service
				.pending_loop(&session.session_id)
				.expect("pending lookup should succeed")
				.is_none()
		);
		assert!(
			handler
				.transport_state
				.load_session_state_or_default(&session.session_id)
				.expect("session state should load")
				.pending_loop
				.is_none()
		);
		assert_eq!(
			handler
				.transport_state
				.load_short_term_continuity(&session.session_id, 8)
				.expect("turns should load")
				.len(),
			1
		);
	}

	#[test]
	fn telegram_control_clear_clears_session_state() {
		let handler = test_handler();
		let binding_id = "telegram-control-clear";
		let session = bootstrap_session(&handler, binding_id);
		handler
			.transport_state
			.save_session_state(&session.session_id, SessionState { pending_loop: None })
			.expect("session state should save");
		handler
			.transport_state
			.append_turn(
				&session.session_id,
				ConversationTurn {
					role: ConversationRole::Assistant,
					content: "hi".to_string(),
					created_at_unix_ms: 2,
				},
			)
			.expect("conversation should save");
		let loop_state = awaiting_user_loop_state(&session.session_id, "loop-clear-1");
		handler
			.service
			.restore_pending_loop(loop_state)
			.expect("pending loop should restore");

		let response = handler
			.handle_control_command(control_command(binding_id, TelegramControlCommand::Clear))
			.expect("clear command should succeed");
		assert_eq!(response.response.status, ResponseStatus::Succeeded);
		assert!(
			response
				.response
				.message
				.contains("Cleared the current chat session state.")
		);
		assert!(
			handler
				.service
				.pending_loop(&session.session_id)
				.expect("pending loop lookup should succeed")
				.is_none()
		);
		assert_eq!(
			handler
				.transport_state
				.load_short_term_continuity(&session.session_id, 8)
				.expect("turns should load")
				.len(),
			0
		);
		assert!(
			handler
				.transport_state
				.load_session_state_or_default(&session.session_id)
				.expect("session state should load")
				.pending_loop
				.is_none()
		);
	}

	#[test]
	fn telegram_control_status_reports_current_snapshot() {
		let handler = test_handler();
		let binding_id = "telegram-control-status";
		let session = bootstrap_session(&handler, binding_id);
		handler
			.transport_state
			.append_turn(
				&session.session_id,
				ConversationTurn {
					role: ConversationRole::Assistant,
					content: "ready".to_string(),
					created_at_unix_ms: 3,
				},
			)
			.expect("conversation should save");
		let loop_state = awaiting_user_loop_state(&session.session_id, "loop-status-1");
		handler
			.service
			.restore_pending_loop(loop_state)
			.expect("pending loop should restore");

		let response = handler
			.handle_control_command(control_command(binding_id, TelegramControlCommand::Status))
			.expect("status command should succeed");
		assert_eq!(response.response.status, ResponseStatus::Succeeded);
		assert!(
			response
				.response
				.message
				.contains("Current chat session status")
		);
		assert!(
			response
				.response
				.message
				.contains(&format!("Active session id: {}", session.session_id))
		);
		assert!(response.response.message.contains("Pending loop: yes"));
		assert!(
			response
				.response
				.message
				.contains("Pending run: loop-status-1")
		);
		assert!(response.response.message.contains("Recent turns: 1"));
		assert!(
			response
				.response
				.message
				.contains("Runtime mode: requested=")
		);
	}

	#[test]
	fn telegram_status_without_active_session_does_not_bootstrap() {
		let handler = test_handler();
		let binding_id = "status-no-active";

		let response = handler
			.handle_control_command(control_command(binding_id, TelegramControlCommand::Status))
			.expect("status command should succeed");
		assert_eq!(response.response.status, ResponseStatus::Succeeded);
		assert!(response.response.message.contains("Active session: none"));
		assert!(
			handler
				.transport_state
				.get_active_session(binding_id)
				.expect("active session lookup should succeed")
				.is_none()
		);
	}

	#[test]
	fn telegram_control_sessions_reports_current_session_list() {
		let handler = test_handler();
		let binding_id = "telegram-control-sessions";
		let session = bootstrap_session(&handler, binding_id);

		let response = handler
			.handle_control_command(control_command(
				binding_id,
				TelegramControlCommand::Sessions,
			))
			.expect("sessions command should succeed");
		assert_eq!(response.response.status, ResponseStatus::Succeeded);
		assert!(response.response.message.contains("Current chat sessions"));
		assert!(response.response.message.contains(&session.name));
		assert!(
			response
				.response
				.message
				.contains(&format!("Active session id: {}", session.session_id))
		);
		assert!(!response.response.message.contains("Pending loop:"));
		assert!(!response.response.message.contains("Pending run:"));
		assert!(!response.response.message.contains("Recent turns:"));
		assert!(!response.response.message.contains("Runtime mode:"));
		assert!(response.reply_markup.is_some());
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
		assert_eq!(response.response.status, ResponseStatus::Succeeded);
		assert!(response.response.message.contains("/cancel"));
		assert!(response.response.message.contains("/clear"));
		assert!(response.response.message.contains("/status"));
		assert!(response.response.message.contains("/help"));
		assert!(response.response.message.contains("/sessions"));
		assert!(response.response.message.contains("/new"));
		assert!(response.response.message.contains("/delete"));
		assert!(response.response.message.contains("/session-setting"));
		assert!(response.response.message.contains("/compact"));
		assert!(
			response
				.response
				.message
				.contains("Send natural language directly")
		);
	}

	#[test]
	fn telegram_control_compact_reports_nothing_when_history_is_short() {
		let handler = test_handler();
		let binding_id = "telegram-compact-short";
		let session = bootstrap_session(&handler, binding_id);
		// Seed two turns — below the compaction threshold.
		handler
			.transport_state
			.append_turn(
				&session.session_id,
				ConversationTurn {
					role: ConversationRole::User,
					content: "hello".to_string(),
					created_at_unix_ms: 1,
				},
			)
			.expect("turn should append");

		let response = handler
			.handle_control_command(control_command(binding_id, TelegramControlCommand::Compact))
			.expect("compact command should succeed");
		assert_eq!(response.response.status, ResponseStatus::Succeeded);
		assert!(response.response.message.contains("nothing to compact"));
		// History is unchanged.
		assert_eq!(
			handler
				.transport_state
				.load_short_term_continuity(&session.session_id, usize::MAX)
				.expect("turns should load")
				.len(),
			1
		);
	}

	#[test]
	fn telegram_control_compact_compacts_long_history() {
		let handler = test_handler();
		let binding_id = "telegram-compact-long";
		let session = bootstrap_session(&handler, binding_id);
		// Seed 8 turns — above the RETAIN_TAIL=6 threshold.
		for i in 0..8u64 {
			handler
				.transport_state
				.append_turn(
					&session.session_id,
					ConversationTurn {
						role: ConversationRole::User,
						content: format!("msg {i}"),
						created_at_unix_ms: i,
					},
				)
				.expect("turn should append");
		}

		let response = handler
			.handle_control_command(control_command(binding_id, TelegramControlCommand::Compact))
			.expect("compact command should succeed");
		assert_eq!(response.response.status, ResponseStatus::Succeeded);
		assert!(
			response.response.message.contains("Compacted"),
			"response should mention compaction"
		);
		// After compaction: 2 discarded → 1 summary + 6 tail = 7 retained.
		let turns = handler
			.transport_state
			.load_short_term_continuity(&session.session_id, usize::MAX)
			.expect("turns should load");
		assert_eq!(turns.len(), 7);
		assert_eq!(turns[0].role, ConversationRole::System);
	}

	#[test]
	fn telegram_control_compact_without_active_session_returns_graceful_response() {
		let handler = test_handler();
		let response = handler
			.handle_control_command(control_command(
				"compact-no-active",
				TelegramControlCommand::Compact,
			))
			.expect("compact without active session should respond");
		assert_eq!(response.response.status, ResponseStatus::Succeeded);
		assert!(response.response.message.contains("No active session"));
	}

	#[test]
	fn telegram_control_new_creates_and_switches_session() {
		let handler = test_handler();
		let binding_id = "telegram-new";

		let response = handler
			.handle_control_command(control_command(binding_id, TelegramControlCommand::New))
			.expect("new command should succeed");
		assert_eq!(response.response.status, ResponseStatus::Succeeded);
		assert!(
			response
				.response
				.message
				.contains("Created and switched to a new session.")
		);

		let active = active_session(&handler, binding_id);
		assert!(response.response.message.contains(&active.session_id));
		assert_eq!(active.name, active.session_id);
	}

	#[tokio::test(flavor = "multi_thread")]
	async fn telegram_session_setting_waits_for_valid_text_and_renames_session() {
		let handler = test_handler();
		let binding_id = "1";
		let session = bootstrap_session(&handler, binding_id);

		let response = handler
			.handle_control_command(control_command(
				binding_id,
				TelegramControlCommand::SessionSetting,
			))
			.expect("session-setting should succeed");
		assert_eq!(response.response.status, ResponseStatus::Succeeded);
		assert!(
			handler
				.pending_session_rename(1)
				.expect("rename state lookup")
				.is_some()
		);

		let blank = handler
			.handle_request(request(binding_id, "   "))
			.expect("blank rename input should return a response");
		assert_eq!(blank.response.status, ResponseStatus::Failed);
		assert!(
			handler
				.pending_session_rename(1)
				.expect("rename state lookup")
				.is_some()
		);

		let renamed = handler
			.handle_request(request(binding_id, "Renamed Session"))
			.expect("rename input should succeed");
		assert_eq!(renamed.response.status, ResponseStatus::Succeeded);
		assert!(
			renamed
				.response
				.message
				.contains("Renamed the current session.")
		);
		assert!(
			handler
				.pending_session_rename(1)
				.expect("rename state lookup")
				.is_none()
		);

		let active = active_session(&handler, binding_id);
		assert_eq!(active.session_id, session.session_id);
		assert_eq!(active.name, "Renamed Session");
	}

	#[test]
	fn telegram_session_setting_is_cancelled_by_new_slash_command() {
		let handler = test_handler();
		let binding_id = "1";
		let session = bootstrap_session(&handler, binding_id);

		handler
			.handle_control_command(control_command(
				binding_id,
				TelegramControlCommand::SessionSetting,
			))
			.expect("session-setting should succeed");
		assert!(
			handler
				.pending_session_rename(1)
				.expect("rename state lookup")
				.is_some()
		);

		let help = handler
			.handle_control_command(control_command(binding_id, TelegramControlCommand::Help))
			.expect("help command should succeed");
		assert_eq!(help.response.status, ResponseStatus::Succeeded);
		assert!(
			handler
				.pending_session_rename(1)
				.expect("rename state lookup")
				.is_none()
		);
		assert_eq!(active_session(&handler, binding_id).name, session.name);
	}

	#[test]
	fn telegram_session_callback_selects_active_session() {
		let handler = test_handler();
		let binding_id = "1";
		let first = bootstrap_session(&handler, binding_id);
		let second = handler
			.transport_state
			.create_session(binding_id, SessionCreateRequest::default())
			.expect("second session should create");

		let response = handler
			.handle_session_callback(TelegramSessionCallbackAction {
				chat_id: 1,
				callback_query_id: "cb-select".to_string(),
				action: TelegramSessionCallbackKind::SelectSession {
					session_id: second.session_id.clone(),
					page: 0,
				},
			})
			.expect("select callback should succeed");
		assert_eq!(response.response.status, ResponseStatus::Succeeded);
		assert!(
			response
				.response
				.message
				.contains("Switched active session")
		);
		assert_eq!(
			active_session(&handler, binding_id).session_id,
			second.session_id
		);
		assert_ne!(first.session_id, second.session_id);
	}

	#[test]
	fn telegram_delete_flow_requires_confirmation_and_replaces_active_session() {
		let handler = test_handler();
		let binding_id = "1";
		let active = bootstrap_session(&handler, binding_id);

		let prompt = handler
			.handle_control_command(control_command(binding_id, TelegramControlCommand::Delete))
			.expect("delete command should respond");
		assert_eq!(prompt.response.status, ResponseStatus::Succeeded);
		assert!(prompt.reply_markup.is_some());
		assert_eq!(
			active_session(&handler, binding_id).session_id,
			active.session_id
		);

		let cancelled = handler
			.handle_session_callback(TelegramSessionCallbackAction {
				chat_id: 1,
				callback_query_id: "cb-cancel".to_string(),
				action: TelegramSessionCallbackKind::DeleteCancel,
			})
			.expect("delete cancel should succeed");
		assert_eq!(cancelled.response.status, ResponseStatus::Succeeded);
		assert_eq!(
			active_session(&handler, binding_id).session_id,
			active.session_id
		);

		let confirmed = handler
			.handle_session_callback(TelegramSessionCallbackAction {
				chat_id: 1,
				callback_query_id: "cb-confirm".to_string(),
				action: TelegramSessionCallbackKind::DeleteConfirm {
					session_id: active.session_id.clone(),
				},
			})
			.expect("delete confirm should succeed");
		assert_eq!(confirmed.response.status, ResponseStatus::Succeeded);
		assert_ne!(
			active_session(&handler, binding_id).session_id,
			active.session_id
		);
	}

	#[test]
	fn telegram_delete_without_active_session_returns_noop_without_keyboard() {
		let handler = test_handler();
		let response = handler
			.handle_control_command(control_command(
				"delete-no-active",
				TelegramControlCommand::Delete,
			))
			.expect("delete command should succeed");
		assert_eq!(response.response.status, ResponseStatus::Succeeded);
		assert!(response.response.message.contains("No active session"));
		assert!(response.reply_markup.is_none());
	}

	#[tokio::test(flavor = "multi_thread")]
	async fn recognized_control_commands_do_not_pollute_continuity() {
		let handler = test_handler();
		let binding_id = "telegram-control-memory";
		handler
			.handle_request(request(binding_id, "今天周几？"))
			.expect("request should succeed");
		let session = active_session(&handler, binding_id);
		let turns_before = handler
			.transport_state
			.load_short_term_continuity(&session.session_id, 8)
			.expect("turns should load");

		handler
			.handle_control_command(control_command(binding_id, TelegramControlCommand::Status))
			.expect("status command should succeed");

		let turns_after = handler
			.transport_state
			.load_short_term_continuity(&session.session_id, 8)
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
		assert_eq!(response.response.status, ResponseStatus::Failed);
		assert!(
			response
				.response
				.message
				.contains("does not accept extra arguments")
		);
	}

	#[tokio::test(flavor = "multi_thread")]
	#[ignore = "needs mock LLM responses updated for message-based turn loop"]
	async fn telegram_handler_surfaces_skill_install_message() {
		let root = tempfile::tempdir().expect("temp root should exist");
		// SkillRegistry::file_backed builds a reqwest::blocking::Client internally, which creates
		// and immediately drops an internal tokio current-thread runtime. That drop panics when it
		// occurs inside an async context. block_in_place provides a blocking-allowed scope so the
		// client construction and its internal runtime drop can complete without hitting that check.
		let registry = tokio::task::block_in_place(|| {
			SkillRegistry::file_backed(root.path().join("skills")).with_fetcher(Arc::new(
				StaticArchiveFetcher {
					archive: DownloadedArchive {
						archive_url: "https://example.com/archive.zip".to_string(),
						bytes: test_skill_archive_bytes(),
						resolved_reference: Some("main".to_string()),
					},
				},
			))
		});
		let handler = RuntimeServiceTelegramHandler {
			service: Arc::new(RuntimeService::in_memory_with_agent_runtime(
				GenericAgentRuntime::with_skill_registry(registry),
			)),
			transport_state: Arc::new(TelegramTransportState::default()),
			session_ux_config: TelegramSessionUxConfig::default(),
			pending_session_rename_by_chat: Mutex::new(HashMap::new()),
			bot_client: None,
			progress_notices_enabled: false,
		};

		let response = handler
			.handle_request(request(
				"telegram-skill-install",
				"Install skill from https://github.com/anthropics/skills/tree/main/skills/claude-api",
			))
			.expect("skill install request should succeed");
		assert_eq!(response.response.status, ResponseStatus::Succeeded);
		assert!(
			response
				.response
				.message
				.contains("Installed skill `claude-api`")
		);

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
			model_override: None,
			thinking_effort: None,
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
		test_handler_with_service(RuntimeService::in_memory())
	}

	fn test_handler_with_service(service: RuntimeService) -> RuntimeServiceTelegramHandler {
		RuntimeServiceTelegramHandler {
			service: Arc::new(service),
			transport_state: Arc::new(TelegramTransportState::default()),
			session_ux_config: TelegramSessionUxConfig::default(),
			pending_session_rename_by_chat: Mutex::new(HashMap::new()),
			bot_client: None,
			progress_notices_enabled: false,
		}
	}

	fn bootstrap_session(
		handler: &RuntimeServiceTelegramHandler,
		binding_id: &str,
	) -> SessionDescriptor {
		handler
			.create_and_select_session(binding_id, SessionCreateRequest::default())
			.expect("session should bootstrap")
	}

	fn active_session(
		handler: &RuntimeServiceTelegramHandler,
		binding_id: &str,
	) -> SessionDescriptor {
		handler
			.transport_state
			.get_active_session(binding_id)
			.expect("active session lookup should succeed")
			.expect("active session should exist")
	}

	fn awaiting_user_loop_state(session_id: &str, run_id: &str) -> LoopState {
		let context = LoopContext {
			request_id: format!("req-{run_id}"),
			session_id: session_id.to_string(),
			goal: "Need a clarification".to_string(),
			workspace_root: "/workspace".to_string(),
			working_directory: "/workspace".to_string(),
			visible_tools: vec!["inventory.describe".to_string()],
			bound_resources: vec![ResourceSelector::tool("inventory.describe".to_string())],
			route_decision: RouteDecision::new(
				IntentFamily::Unknown,
				0.8,
				false,
				RouteRisk::Low,
				vec!["inventory.describe".to_string()],
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
