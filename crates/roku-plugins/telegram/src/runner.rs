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

use std::thread;
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
pub trait TelegramInteractionHandler: Send + Sync {
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
/// This type is intentionally transport-centric: it polls updates, logs polling health, and
/// renders outbound responses. It should not accumulate Telegram command semantics or session
/// state policy.
pub struct TelegramPollingRunner {
	connector: TelegramConnector,
	client: TelegramBotClient,
	idle_backoff_ms: u64,
	poll_error_log_threshold: u32,
	progress_notices_enabled: bool,
	render_options: TelegramRenderOptions,
}

impl TelegramPollingRunner {
	pub fn from_env() -> Result<Self, TelegramTransportError> {
		Self::new(TelegramBotConfig::from_env()?)
	}

	pub fn new(config: TelegramBotConfig) -> Result<Self, TelegramTransportError> {
		let idle_backoff_ms = config.idle_backoff_ms;
		let poll_error_log_threshold = config.poll_error_log_threshold;
		let progress_notices_enabled = config.progress_notices_enabled;
		let render_options = config.render_options();
		Ok(Self {
			connector: TelegramConnector,
			client: TelegramBotClient::new(config)?,
			idle_backoff_ms,
			poll_error_log_threshold,
			progress_notices_enabled,
			render_options,
		})
	}

	pub fn run<H>(&self, handler: H) -> Result<(), TelegramTransportError>
	where
		H: TelegramInteractionHandler,
	{
		let mut next_offset = None;
		let mut consecutive_poll_failures = 0_u32;
		loop {
			let updates = match self.client.get_updates(next_offset) {
				Ok(updates) => {
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
				Err(error) => {
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
					thread::sleep(Duration::from_millis(self.idle_backoff_ms));
					continue;
				}
			};
			if updates.is_empty() {
				thread::sleep(Duration::from_millis(self.idle_backoff_ms));
				continue;
			}

			for update in updates {
				next_offset = Some(update.update_id.saturating_add(1));
				if let Err(error) = self.process_update(&handler, update) {
					log_telegram(
						LogLevel::Error,
						"failed to process telegram update",
						[("error", error.to_string())],
					);
					thread::sleep(Duration::from_millis(self.idle_backoff_ms));
				}
			}
		}
	}

	fn process_update<H>(
		&self,
		handler: &H,
		update: TelegramUpdate,
	) -> Result<(), TelegramTransportError>
	where
		H: TelegramInteractionHandler,
	{
		let chat_id = update
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

		match self.connector.interaction_from_update(update) {
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
				if self.progress_notices_enabled
					&& let Err(error) = self
						.client
						.send_message(&TelegramOutboundMessage::progress_notice(chat_id, &request))
				{
					log_telegram(
						LogLevel::Warn,
						"failed to send progress notice",
						[
							("chat_id", chat_id.to_string()),
							("request_id", request.request_id.0.clone()),
							("error", error.to_string()),
						],
					);
				}
				self.dispatch_response(chat_id, handler.handle_request(request))
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
				self.dispatch_response(command.chat_id, handler.handle_control_command(command))
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
				let response =
					handler.handle_approval_decision(action.approval_id, action.decision);
				self.client.answer_callback_query(
					&action.callback_query_id,
					callback_acknowledgement(&response),
				)?;
				self.dispatch_response(action.chat_id, response)
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
				let response = handler.handle_session_callback(action.clone());
				self.client.answer_callback_query(
					&action.callback_query_id,
					session_callback_acknowledgement(&response),
				)?;
				self.dispatch_response(action.chat_id, response)
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
				if let Some(chat_id) = chat_id {
					self.client
						.send_message(&TelegramOutboundMessage::from_error(
							chat_id,
							&error.to_string(),
						))
				} else {
					Ok(())
				}
			}
		}
	}

	fn dispatch_response(
		&self,
		chat_id: i64,
		response: Result<TelegramHandlerResponse, RuntimeError>,
	) -> Result<(), TelegramTransportError> {
		match response {
			Ok(handler_response) => {
				if handler_response.delivered_via_streaming {
					log_telegram(
						LogLevel::Info,
						"skipping dispatch: response delivered via streaming edit",
						[
							("chat_id", chat_id.to_string()),
							(
								"request_id",
								handler_response.response.request_id.0.clone(),
							),
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
				self.client.send_message(
					&TelegramOutboundMessage::from_handler_response_with_options(
						chat_id,
						&handler_response,
						self.render_options,
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
				self.client
					.send_message(&TelegramOutboundMessage::from_error(
						chat_id,
						&error.message,
					))
			}
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
	fn runner_captures_progress_notice_and_render_flags_from_config() {
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

		assert!(!runner.progress_notices_enabled);
		assert!(runner.render_options.include_request_metadata);
		assert!(runner.render_options.show_attachments);
	}
}
