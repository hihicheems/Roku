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

//! Telegram polling and dispatch loop.
//!
//! This module owns transport concerns: polling updates, normalizing them through the inbound
//! connector, and dispatching the resulting structured interaction to one handler entry point.
//! It does not decide business semantics for Telegram commands or request state; that boundary
//! stays in the handler/runtime layer.
//!
//! Updates are dispatched as independent async tasks so that slow requests (multi-step agent
//! execution, context compaction) do not block slash commands or other users.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use roku_common_types::{ApprovalDecision, ApprovalId, RequestEnvelope, RuntimeError};
use roku_common_types::{LogLevel, LogRecord, emit_global_log};

use crate::outbound::TelegramRenderOptions;
use crate::{
	TelegramBotClient, TelegramBotConfig, TelegramConnector, TelegramConnectorError,
	TelegramHandlerResponse, TelegramInteraction, TelegramOutboundMessage,
	TelegramSessionCallbackAction, TelegramTransportError, TelegramUpdate,
};

/// Runtime-facing Telegram interaction adapter.
///
/// The handler receives already-normalized Telegram interactions. In particular, control commands
/// arrive as structured management actions and must not be re-parsed from raw text.
pub trait TelegramInteractionHandler: Send + Sync + 'static {
	fn handle_request(
		&self,
		request: RequestEnvelope,
	) -> Result<TelegramHandlerResponse, RuntimeError>;

	/// Handles an out-of-band Telegram control command such as `/status` or `/clear`.
	fn handle_control_command(
		&self,
		command: crate::TelegramControlCommandRequest,
	) -> Result<TelegramHandlerResponse, RuntimeError>;

	fn handle_approval_decision(
		&self,
		approval_id: ApprovalId,
		decision: ApprovalDecision,
	) -> Result<TelegramHandlerResponse, RuntimeError>;

	fn handle_session_callback(
		&self,
		action: TelegramSessionCallbackAction,
	) -> Result<TelegramHandlerResponse, RuntimeError>;
}

/// Long-running Telegram polling runner.
///
/// Each incoming update is dispatched as an independent async task so that
/// slow requests do not block the polling loop or other updates.
pub struct TelegramPollingRunner {
	connector: TelegramConnector,
	client: Arc<TelegramBotClient>,
	idle_backoff_ms: u64,
	poll_error_log_threshold: u32,
	render_options: TelegramRenderOptions,
}

impl TelegramPollingRunner {
	pub fn from_env() -> Result<Self, TelegramTransportError> {
		Self::new(TelegramBotConfig::from_env()?)
	}

	pub fn new(config: TelegramBotConfig) -> Result<Self, TelegramTransportError> {
		let idle_backoff_ms = config.idle_backoff_ms;
		let poll_error_log_threshold = config.poll_error_log_threshold;
		let render_options = config.render_options();
		Ok(Self {
			connector: TelegramConnector,
			client: Arc::new(TelegramBotClient::new(config)?),
			idle_backoff_ms,
			poll_error_log_threshold,
			render_options,
		})
	}

	/// Start the polling loop. Each incoming update is dispatched as an
	/// independent task so that long-running requests do not block subsequent
	/// updates. This method blocks the calling thread until the process exits.
	pub fn run<H>(self, handler: H) -> Result<(), TelegramTransportError>
	where
		H: TelegramInteractionHandler,
	{
		let rt = tokio::runtime::Builder::new_multi_thread()
			.enable_all()
			.build()
			.map_err(|e| {
				TelegramTransportError::Api(format!("failed to build async runtime: {e}"))
			})?;
		rt.block_on(self.poll_loop(Arc::new(handler)))
	}

	async fn poll_loop<H>(self, handler: Arc<H>) -> Result<(), TelegramTransportError>
	where
		H: TelegramInteractionHandler,
	{
		let mut next_offset: Option<u64> = None;
		let mut consecutive_poll_failures = 0_u32;
		// Per-chat semaphore prevents concurrent handle_request calls for the
		// same chat, which would race on session state. Control commands and
		// callbacks bypass this lock since they are fast and stateless.
		let chat_locks: Arc<Mutex<HashMap<i64, Arc<tokio::sync::Semaphore>>>> =
			Arc::new(Mutex::new(HashMap::new()));

		loop {
			let client = Arc::clone(&self.client);
			let offset = next_offset;
			let updates =
				match tokio::task::spawn_blocking(move || client.get_updates(offset)).await {
					Ok(Ok(updates)) => {
						if consecutive_poll_failures >= self.poll_error_log_threshold
							&& self.poll_error_log_threshold > 0
						{
							log_telegram(
								LogLevel::Info,
								"telegram polling recovered",
								[(
									"consecutive_failures",
									consecutive_poll_failures.to_string(),
								)],
							);
						}
						consecutive_poll_failures = 0;
						updates
					}
					Ok(Err(error)) => {
						consecutive_poll_failures = consecutive_poll_failures.saturating_add(1);
						if should_log_poll_error(
							consecutive_poll_failures,
							self.poll_error_log_threshold,
						) {
							log_telegram(
								LogLevel::Debug,
								"telegram polling retrying after transport failure",
								[
									(
										"consecutive_failures",
										consecutive_poll_failures.to_string(),
									),
									("error", error.to_string()),
								],
							);
						}
						tokio::time::sleep(Duration::from_millis(self.idle_backoff_ms)).await;
						continue;
					}
					Err(join_error) => {
						log_telegram(
							LogLevel::Error,
							"telegram poll task panicked",
							[("error", join_error.to_string())],
						);
						tokio::time::sleep(Duration::from_millis(self.idle_backoff_ms)).await;
						continue;
					}
				};

			if updates.is_empty() {
				tokio::time::sleep(Duration::from_millis(self.idle_backoff_ms)).await;
				continue;
			}

			for update in updates {
				next_offset = Some(update.update_id.saturating_add(1));
				let handler = Arc::clone(&handler);
				let client = Arc::clone(&self.client);
				let connector = self.connector;
				let render_options = self.render_options;
				let locks = Arc::clone(&chat_locks);
				tokio::spawn(async move {
					if let Err(error) =
						dispatch_update(handler, client, connector, render_options, locks, update)
							.await
					{
						log_telegram(
							LogLevel::Error,
							"failed to process telegram update",
							[("error", error.to_string())],
						);
					}
				});
			}
		}
	}
}

/// Process a single Telegram update in its own async task.
async fn dispatch_update<H>(
	handler: Arc<H>,
	client: Arc<TelegramBotClient>,
	connector: TelegramConnector,
	render_options: TelegramRenderOptions,
	chat_locks: Arc<Mutex<HashMap<i64, Arc<tokio::sync::Semaphore>>>>,
	update: TelegramUpdate,
) -> Result<(), TelegramTransportError>
where
	H: TelegramInteractionHandler,
{
	let fallback_chat_id = update
		.message
		.as_ref()
		.map(|message| message.chat.id)
		.or_else(|| {
			update
				.callback_query
				.as_ref()
				.and_then(|callback_query| callback_query.message.as_ref())
				.map(|message| message.chat.id)
		});

	match connector.interaction_from_update(update) {
		Ok(TelegramInteraction::Request { chat_id, request }) => {
			log_telegram(
				LogLevel::Info,
				"received request update",
				[
					("update_type", "request".to_string()),
					("chat_id", chat_id.to_string()),
					("request_id", request.request_id.0.clone()),
					("goal", truncate_for_log(&request.goal, 160)),
				],
			);
			// Try to acquire per-chat semaphore. If another request is already
			// running for this chat, reply "busy" instead of queueing unboundedly.
			let sem = {
				let mut locks = chat_locks.lock().unwrap_or_else(|e| e.into_inner());
				Arc::clone(locks.entry(chat_id).or_insert_with(|| Arc::new(tokio::sync::Semaphore::new(1))))
			};
			let _permit = match sem.try_acquire() {
				Ok(permit) => permit,
				Err(_) => {
					let c = Arc::clone(&client);
					let _ = tokio::task::spawn_blocking(move || {
						c.send_message(&TelegramOutboundMessage::from_error(
							chat_id,
							"A request is already in progress for this chat. Please wait for it to finish.",
						))
					}).await;
					return Ok(());
				}
			};
			let h = handler;
			let result = tokio::task::spawn_blocking(move || h.handle_request(request))
				.await
				.map_err(|e| TelegramTransportError::Api(format!("handler task panicked: {e}")))?;
			dispatch_response_blocking(&client, chat_id, result, render_options).await
		}
		Ok(TelegramInteraction::ControlCommand(command)) => {
			log_telegram(
				LogLevel::Info,
				"received telegram control command",
				[
					("update_type", "control_command".to_string()),
					("chat_id", command.chat_id.to_string()),
					("command", command.command.as_str().to_string()),
					(
						"has_argument",
						command.argument.as_ref().is_some().to_string(),
					),
				],
			);
			let cid = command.chat_id;
			let h = handler;
			let result = tokio::task::spawn_blocking(move || h.handle_control_command(command))
				.await
				.map_err(|e| TelegramTransportError::Api(format!("handler task panicked: {e}")))?;
			dispatch_response_blocking(&client, cid, result, render_options).await
		}
		Ok(TelegramInteraction::ApprovalDecision(action)) => {
			log_telegram(
				LogLevel::Info,
				"received approval decision",
				[
					("update_type", "approval".to_string()),
					("chat_id", action.chat_id.to_string()),
					("approval_id", action.approval_id.0.clone()),
					("actor", action.decision.actor.clone()),
					("approved", action.decision.approved.to_string()),
				],
			);
			let h = handler;
			let aid = action.approval_id;
			let decision = action.decision;
			let cid = action.chat_id;
			let cbq = action.callback_query_id;
			let result =
				tokio::task::spawn_blocking(move || h.handle_approval_decision(aid, decision))
					.await
					.map_err(|e| {
						TelegramTransportError::Api(format!("handler task panicked: {e}"))
					})?;
			let ack = callback_acknowledgement(&result).to_string();
			let c = Arc::clone(&client);
			let cbq_owned = cbq;
			let _ = tokio::task::spawn_blocking(move || c.answer_callback_query(&cbq_owned, &ack))
				.await;
			dispatch_response_blocking(&client, cid, result, render_options).await
		}
		Ok(TelegramInteraction::SessionCallback(action)) => {
			log_telegram(
				LogLevel::Info,
				"received session callback",
				[
					("update_type", "session_callback".to_string()),
					("chat_id", action.chat_id.to_string()),
					("callback_query_id", action.callback_query_id.clone()),
				],
			);
			let h = handler;
			let action_clone = action.clone();
			let result =
				tokio::task::spawn_blocking(move || h.handle_session_callback(action_clone))
					.await
					.map_err(|e| {
						TelegramTransportError::Api(format!("handler task panicked: {e}"))
					})?;
			let ack = session_callback_acknowledgement(&result).to_string();
			let c = Arc::clone(&client);
			let cbq_owned = action.callback_query_id.clone();
			let _ = tokio::task::spawn_blocking(move || c.answer_callback_query(&cbq_owned, &ack))
				.await;
			dispatch_response_blocking(&client, action.chat_id, result, render_options).await
		}
		Err(TelegramConnectorError::BotOriginIgnored) => {
			log_telegram(
				LogLevel::Debug,
				"ignored bot-originated update",
				std::iter::empty(),
			);
			Ok(())
		}
		Err(error) => {
			log_telegram(
				LogLevel::Warn,
				"failed to normalize telegram update",
				[("error", error.to_string())],
			);
			if let Some(chat_id) = fallback_chat_id {
				let err_msg = error.to_string();
				let c = Arc::clone(&client);
				let _ = tokio::task::spawn_blocking(move || {
					c.send_message(&TelegramOutboundMessage::from_error(chat_id, &err_msg))
				})
				.await;
				Ok(())
			} else {
				Ok(())
			}
		}
	}
}

/// Async wrapper that runs `dispatch_response` on a blocking thread so that
/// synchronous Telegram HTTP calls do not occupy tokio worker threads.
async fn dispatch_response_blocking(
	client: &Arc<TelegramBotClient>,
	chat_id: i64,
	response: Result<TelegramHandlerResponse, RuntimeError>,
	render_options: TelegramRenderOptions,
) -> Result<(), TelegramTransportError> {
	let c = Arc::clone(client);
	tokio::task::spawn_blocking(move || dispatch_response(&c, chat_id, response, render_options))
		.await
		.map_err(|e| TelegramTransportError::Api(format!("dispatch task panicked: {e}")))?
}

fn dispatch_response(
	client: &TelegramBotClient,
	chat_id: i64,
	response: Result<TelegramHandlerResponse, RuntimeError>,
	render_options: TelegramRenderOptions,
) -> Result<(), TelegramTransportError> {
	match response {
		Ok(handler_response) => {
			if handler_response.delivered_via_streaming {
				log_telegram(
					LogLevel::Info,
					"skipping dispatch: response delivered via streaming edit",
					[
						("chat_id", chat_id.to_string()),
						("request_id", handler_response.response.request_id.0.clone()),
					],
				);
				return Ok(());
			}
			let response = &handler_response.response;
			log_telegram(
				LogLevel::Info,
				"dispatching telegram response",
				[
					("chat_id", chat_id.to_string()),
					("request_id", response.request_id.0.clone()),
					("status", format!("{:?}", response.status)),
					("message", truncate_for_log(&response.message, 200)),
				],
			);
			client.send_message(
				&TelegramOutboundMessage::from_handler_response_with_options(
					chat_id,
					&handler_response,
					render_options,
				),
			)
		}
		Err(error) => {
			log_telegram(
				LogLevel::Error,
				"dispatching telegram error response",
				[
					("chat_id", chat_id.to_string()),
					("status", "error".to_string()),
					("message", truncate_for_log(&error.message, 200)),
				],
			);
			client.send_message(&TelegramOutboundMessage::from_error(
				chat_id,
				&error.message,
			))
		}
	}
}

fn callback_acknowledgement(response: &Result<TelegramHandlerResponse, RuntimeError>) -> &str {
	match response {
		Ok(response) => match response.response.status {
			roku_common_types::ResponseStatus::Succeeded => "Approval recorded",
			roku_common_types::ResponseStatus::PendingApproval => "Still waiting on approval",
			roku_common_types::ResponseStatus::Failed => "Decision processed with failure",
		},
		Err(_) => "Approval decision failed",
	}
}

fn session_callback_acknowledgement(
	response: &Result<TelegramHandlerResponse, RuntimeError>,
) -> &str {
	match response {
		Ok(response) => match response.response.status {
			roku_common_types::ResponseStatus::Succeeded => "Session action recorded",
			roku_common_types::ResponseStatus::PendingApproval => "Session action pending",
			roku_common_types::ResponseStatus::Failed => "Session action failed",
		},
		Err(_) => "Session action failed",
	}
}

fn truncate_for_log(value: &str, max_chars: usize) -> String {
	let mut chars = value.chars();
	let truncated = chars.by_ref().take(max_chars).collect::<String>();
	if chars.next().is_some() {
		format!("{truncated}...")
	} else {
		truncated
	}
}

fn should_log_poll_error(consecutive_failures: u32, threshold: u32) -> bool {
	if threshold == 0 {
		return false;
	}

	consecutive_failures >= threshold && consecutive_failures.is_multiple_of(threshold)
}

fn log_telegram(
	level: LogLevel,
	message: &str,
	fields: impl IntoIterator<Item = (&'static str, String)>,
) {
	let record = fields.into_iter().fold(
		LogRecord::new("roku-plugin-telegram", level, message),
		|record: LogRecord, (key, value)| record.with_field(key, value),
	);
	let _ = emit_global_log(record);
}

#[cfg(test)]
mod tests {
	use super::should_log_poll_error;
	use crate::client::TelegramBotConfig;

	#[test]
	fn poll_error_logging_is_suppressed_before_threshold() {
		assert!(!should_log_poll_error(1, 5));
		assert!(!should_log_poll_error(4, 5));
	}

	#[test]
	fn poll_error_logging_emits_on_threshold_boundaries() {
		assert!(should_log_poll_error(5, 5));
		assert!(should_log_poll_error(10, 5));
		assert!(!should_log_poll_error(7, 5));
		assert!(!should_log_poll_error(3, 0));
	}

	#[test]
	fn runner_captures_render_flags_from_config() {
		let runner = super::TelegramPollingRunner::new(TelegramBotConfig {
			token: "token".to_string(),
			api_base_url: "https://api.telegram.org".to_string(),
			poll_timeout_seconds: 30,
			idle_backoff_ms: 250,
			poll_error_log_threshold: 5,
			progress_notices_enabled: false,
			include_request_metadata: true,
			show_attachments: true,
		})
		.expect("runner should build");

		assert!(runner.render_options.include_request_metadata);
		assert!(runner.render_options.show_attachments);
	}
}
