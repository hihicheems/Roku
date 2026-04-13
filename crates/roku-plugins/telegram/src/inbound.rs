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

//! Telegram inbound normalization.
//!
//! This module is the single transport-facing parser that turns raw Telegram payloads into one of
//! three runtime-ready interaction kinds:
//! - ordinary user requests,
//! - out-of-band control commands,
//! - approval callback decisions.
//!
//! The important boundary here is that slash-command interpretation happens exactly once in this
//! module. Recognized control commands become `TelegramInteraction::ControlCommand`; all other
//! slash-prefixed text falls through as ordinary request text. Downstream runner and handler code
//! must not re-parse or reclassify those messages.

use roku_common_types::{ApprovalDecision, ApprovalId, RequestEnvelope, RequestId};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramUpdate {
	pub update_id: u64,
	#[serde(default)]
	pub message: Option<TelegramMessage>,
	#[serde(default)]
	pub callback_query: Option<TelegramCallbackQuery>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramMessage {
	pub message_id: i64,
	pub chat: TelegramChat,
	pub from: Option<TelegramUser>,
	pub text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramCallbackQuery {
	pub id: String,
	pub from: TelegramUser,
	#[serde(default)]
	pub message: Option<TelegramMessage>,
	#[serde(default)]
	pub data: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramChat {
	pub id: i64,
	#[serde(default)]
	pub title: Option<String>,
	#[serde(rename = "type")]
	pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramUser {
	pub id: i64,
	#[serde(default)]
	pub is_bot: bool,
	#[serde(default)]
	pub username: Option<String>,
}

#[derive(Debug, Clone)]
pub enum TelegramInteraction {
	/// A normal natural-language request that should enter the agent/runtime path.
	Request {
		chat_id: i64,
		request: RequestEnvelope,
	},
	/// A Telegram-side session control command handled outside the normal agent loop.
	ControlCommand(TelegramControlCommandRequest),
	/// An approval callback mapped from Telegram inline keyboard actions.
	ApprovalDecision(TelegramApprovalAction),
	/// A session-management callback mapped from Telegram inline keyboard actions.
	SessionCallback(TelegramSessionCallbackAction),
}

#[derive(Debug, Clone)]
pub struct TelegramApprovalAction {
	pub chat_id: i64,
	pub callback_query_id: String,
	pub approval_id: ApprovalId,
	pub decision: ApprovalDecision,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelegramSessionCallbackAction {
	pub chat_id: i64,
	pub callback_query_id: String,
	pub action: TelegramSessionCallbackKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TelegramSessionCallbackKind {
	SelectSession { session_id: String, page: usize },
	ShowPage { page: usize },
	DeleteConfirm { session_id: String },
	DeleteCancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TelegramControlCommand {
	/// Cancel the current chat's pending loop without clearing conversation state.
	Cancel,
	/// Clear Telegram session-scoped state for the current chat.
	Clear,
	/// Report the current chat's session snapshot.
	Status,
	/// Show supported Telegram control commands.
	Help,
	/// Show the currently active Telegram session overview.
	Sessions,
	/// Create and switch to a new session for the current chat.
	New,
	/// Delete the current active session after explicit confirmation.
	Delete,
	/// Rename the current active session via a follow-up text input.
	SessionSetting,
	/// Compact older conversation turns into a summary for the current active session.
	Compact,
}

/// Parsed Telegram-side control command.
///
/// This is already normalized to the current chat/session boundary, so downstream code should
/// treat it as a resolved management action instead of trying to infer command semantics from the
/// original message text again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelegramControlCommandRequest {
	pub chat_id: i64,
	pub session_id: String,
	pub command: TelegramControlCommand,
	pub argument: Option<String>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TelegramConnectorError {
	#[error("telegram update does not contain a message")]
	MissingMessage,
	#[error("telegram callback query does not contain a callback payload")]
	MissingCallbackData,
	#[error("telegram callback query does not contain an originating message")]
	MissingCallbackMessage,
	#[error("telegram message does not contain text")]
	MissingText,
	#[error("telegram callback payload is invalid")]
	InvalidCallbackData,
	#[error("telegram callback action is unsupported")]
	UnsupportedCallbackAction,
	#[error("telegram bot-originated interactions are ignored")]
	BotOriginIgnored,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct TelegramConnector;

impl TelegramConnector {
	/// Normalizes one Telegram update into a single runtime interaction.
	///
	/// Control-command detection is intentionally resolved here before request construction so the
	/// rest of the Telegram pipeline can dispatch on structured variants instead of repeating
	/// slash-command heuristics.
	pub fn interaction_from_update(
		&self,
		update: TelegramUpdate,
	) -> Result<TelegramInteraction, TelegramConnectorError> {
		if let Some(callback_query) = update.callback_query {
			return self.callback_interaction_from_callback(callback_query);
		}

		if let Some(command) = self.control_command_from_message(&update)? {
			return Ok(TelegramInteraction::ControlCommand(command));
		}

		let chat_id = update.message.as_ref().map(|message| message.chat.id);
		let request = self.request_from_update(update)?;
		let chat_id = chat_id.ok_or(TelegramConnectorError::MissingMessage)?;

		Ok(TelegramInteraction::Request { chat_id, request })
	}

	pub fn request_from_update(
		&self,
		update: TelegramUpdate,
	) -> Result<RequestEnvelope, TelegramConnectorError> {
		let message = update
			.message
			.ok_or(TelegramConnectorError::MissingMessage)?;
		let text = message
			.text
			.clone()
			.ok_or(TelegramConnectorError::MissingText)?;
		self.request_from_message(update.update_id, message, text)
	}

	fn request_from_message(
		&self,
		update_id: u64,
		message: TelegramMessage,
		goal: String,
	) -> Result<RequestEnvelope, TelegramConnectorError> {
		if message.from.as_ref().is_some_and(|user| user.is_bot) {
			return Err(TelegramConnectorError::BotOriginIgnored);
		}
		Ok(RequestEnvelope {
			request_id: RequestId(format!("tg-{update_id}")),
			session_id: message.chat.id.to_string(),
			goal,
			planning_mode_hint: None,
			conversation_history: Vec::new(),
			model_override: None,
			thinking_effort: None,
		})
	}

	fn control_command_from_message(
		&self,
		update: &TelegramUpdate,
	) -> Result<Option<TelegramControlCommandRequest>, TelegramConnectorError> {
		let message = update
			.message
			.as_ref()
			.ok_or(TelegramConnectorError::MissingMessage)?;
		if message.from.as_ref().is_some_and(|user| user.is_bot) {
			return Err(TelegramConnectorError::BotOriginIgnored);
		}
		let text = message
			.text
			.as_deref()
			.ok_or(TelegramConnectorError::MissingText)?;

		Ok(parse_control_command(message.chat.id, text))
	}

	fn callback_interaction_from_callback(
		&self,
		callback_query: TelegramCallbackQuery,
	) -> Result<TelegramInteraction, TelegramConnectorError> {
		if callback_query.from.is_bot {
			return Err(TelegramConnectorError::BotOriginIgnored);
		}

		let data = callback_query
			.data
			.as_deref()
			.ok_or(TelegramConnectorError::MissingCallbackData)?;
		let message = callback_query
			.message
			.ok_or(TelegramConnectorError::MissingCallbackMessage)?;
		if data.starts_with("ap:") {
			let (approval_id, approved) = parse_approval_callback_data(data)?;
			return Ok(TelegramInteraction::ApprovalDecision(
				TelegramApprovalAction {
					chat_id: message.chat.id,
					callback_query_id: callback_query.id,
					approval_id,
					decision: ApprovalDecision {
						approved,
						actor: callback_actor(&callback_query.from),
						comment: None,
					},
				},
			));
		}
		if data.starts_with("sc:") {
			return Ok(TelegramInteraction::SessionCallback(
				TelegramSessionCallbackAction {
					chat_id: message.chat.id,
					callback_query_id: callback_query.id,
					action: parse_session_callback_data(data)?,
				},
			));
		}

		Err(TelegramConnectorError::UnsupportedCallbackAction)
	}
}

pub(crate) fn approval_callback_data(approval_id: &ApprovalId, approved: bool) -> String {
	let action = if approved { "a" } else { "r" };
	format!("ap:{action}:{}", approval_id.0)
}

pub fn session_select_callback_data(page: usize, session_id: &str) -> String {
	format!("sc:s:{page}:{session_id}")
}

pub fn session_page_callback_data(page: usize) -> String {
	format!("sc:p:{page}")
}

pub fn session_delete_confirm_callback_data(session_id: &str) -> String {
	format!("sc:d:{session_id}")
}

pub fn session_delete_cancel_callback_data() -> String {
	"sc:c".to_string()
}

fn parse_approval_callback_data(data: &str) -> Result<(ApprovalId, bool), TelegramConnectorError> {
	let mut parts = data.splitn(3, ':');
	if parts.next() != Some("ap") {
		return Err(TelegramConnectorError::InvalidCallbackData);
	}

	let approved = match parts.next() {
		Some("a") => true,
		Some("r") => false,
		Some(_) => return Err(TelegramConnectorError::UnsupportedCallbackAction),
		None => return Err(TelegramConnectorError::InvalidCallbackData),
	};
	let approval_id = parts
		.next()
		.filter(|value| !value.trim().is_empty())
		.ok_or(TelegramConnectorError::InvalidCallbackData)?;

	Ok((ApprovalId(approval_id.to_string()), approved))
}

fn parse_session_callback_data(
	data: &str,
) -> Result<TelegramSessionCallbackKind, TelegramConnectorError> {
	let mut parts = data.splitn(4, ':');
	if parts.next() != Some("sc") {
		return Err(TelegramConnectorError::InvalidCallbackData);
	}

	match parts.next() {
		Some("s") => {
			let page = parse_callback_page(parts.next())?;
			let session_id = parts
				.next()
				.filter(|value| !value.trim().is_empty())
				.ok_or(TelegramConnectorError::InvalidCallbackData)?;
			Ok(TelegramSessionCallbackKind::SelectSession {
				session_id: session_id.to_string(),
				page,
			})
		}
		Some("p") => Ok(TelegramSessionCallbackKind::ShowPage {
			page: parse_callback_page(parts.next())?,
		}),
		Some("d") => {
			let session_id = parts
				.next()
				.filter(|value| !value.trim().is_empty())
				.ok_or(TelegramConnectorError::InvalidCallbackData)?;
			Ok(TelegramSessionCallbackKind::DeleteConfirm {
				session_id: session_id.to_string(),
			})
		}
		Some("c") => Ok(TelegramSessionCallbackKind::DeleteCancel),
		Some(_) => Err(TelegramConnectorError::UnsupportedCallbackAction),
		None => Err(TelegramConnectorError::InvalidCallbackData),
	}
}

fn parse_callback_page(value: Option<&str>) -> Result<usize, TelegramConnectorError> {
	value
		.ok_or(TelegramConnectorError::InvalidCallbackData)?
		.parse::<usize>()
		.map_err(|_| TelegramConnectorError::InvalidCallbackData)
}

fn callback_actor(user: &TelegramUser) -> String {
	if let Some(username) = user
		.username
		.as_ref()
		.filter(|value| !value.trim().is_empty())
	{
		return format!("telegram:{username}");
	}

	format!("telegram-user-{}", user.id)
}

#[derive(Debug, Clone, Copy)]
struct TelegramControlCommandSpec {
	name: &'static str,
	command: TelegramControlCommand,
	allows_inline_argument: bool,
}

const TELEGRAM_CONTROL_COMMANDS: &[TelegramControlCommandSpec] = &[
	TelegramControlCommandSpec {
		name: "cancel",
		command: TelegramControlCommand::Cancel,
		allows_inline_argument: false,
	},
	TelegramControlCommandSpec {
		name: "clear",
		command: TelegramControlCommand::Clear,
		allows_inline_argument: false,
	},
	TelegramControlCommandSpec {
		name: "status",
		command: TelegramControlCommand::Status,
		allows_inline_argument: false,
	},
	TelegramControlCommandSpec {
		name: "help",
		command: TelegramControlCommand::Help,
		allows_inline_argument: false,
	},
	TelegramControlCommandSpec {
		name: "sessions",
		command: TelegramControlCommand::Sessions,
		allows_inline_argument: false,
	},
	TelegramControlCommandSpec {
		name: "new",
		command: TelegramControlCommand::New,
		allows_inline_argument: false,
	},
	TelegramControlCommandSpec {
		name: "delete",
		command: TelegramControlCommand::Delete,
		allows_inline_argument: false,
	},
	TelegramControlCommandSpec {
		name: "sessionsetting",
		command: TelegramControlCommand::SessionSetting,
		allows_inline_argument: false,
	},
	TelegramControlCommandSpec {
		name: "compact",
		command: TelegramControlCommand::Compact,
		allows_inline_argument: false,
	},
];

impl TelegramControlCommand {
	pub fn as_str(self) -> &'static str {
		control_command_spec(self).name
	}

	pub fn allows_inline_argument(self) -> bool {
		control_command_spec(self).allows_inline_argument
	}
}

fn control_command_spec(command: TelegramControlCommand) -> &'static TelegramControlCommandSpec {
	TELEGRAM_CONTROL_COMMANDS
		.iter()
		.find(|spec| spec.command == command)
		.expect("control command spec should exist")
}

/// Parses a Telegram slash command once at the transport boundary.
///
/// Unknown slash-prefixed text deliberately returns `None` so it can continue through the normal
/// request path. This helper must stay a thin registry lookup, not a second natural-language
/// router.
fn parse_control_command(chat_id: i64, text: &str) -> Option<TelegramControlCommandRequest> {
	let trimmed = text.trim();
	let slash_command = trimmed.split_whitespace().next()?;
	let slash_command = slash_command.strip_prefix('/')?;
	let command_name = slash_command.split('@').next().unwrap_or(slash_command);
	let normalized = normalize_command(command_name);
	let spec = TELEGRAM_CONTROL_COMMANDS
		.iter()
		.find(|candidate| candidate.name == normalized)?;
	let command_prefix_len = slash_command.len().saturating_add(1);
	let argument = trimmed
		.get(command_prefix_len..)
		.map(str::trim)
		.filter(|value| !value.is_empty())
		.map(std::string::ToString::to_string);

	Some(TelegramControlCommandRequest {
		chat_id,
		session_id: chat_id.to_string(),
		command: spec.command,
		argument,
	})
}

fn normalize_command(command: &str) -> String {
	command
		.chars()
		.filter(|character| character.is_ascii_alphanumeric())
		.collect::<String>()
		.to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn into_request_maps_text_message() {
		let connector = TelegramConnector;
		let request = connector
			.request_from_update(TelegramUpdate {
				update_id: 42,
				message: Some(TelegramMessage {
					message_id: 7,
					chat: TelegramChat {
						id: 1001,
						title: Some("alpha".to_string()),
						kind: "group".to_string(),
					},
					from: Some(TelegramUser {
						id: 9,
						is_bot: false,
						username: Some("jojo".to_string()),
					}),
					text: Some("run research".to_string()),
				}),
				callback_query: None,
			})
			.expect("text message should map to request");

		assert_eq!(request.request_id.0, "tg-42");
		assert_eq!(request.session_id, "1001");
		assert_eq!(request.goal, "run research");
		assert_eq!(request.planning_mode_hint, None);
		assert!(request.conversation_history.is_empty());
	}

	#[test]
	fn into_request_rejects_bot_message() {
		let connector = TelegramConnector;
		let error = connector
			.request_from_update(TelegramUpdate {
				update_id: 42,
				message: Some(TelegramMessage {
					message_id: 7,
					chat: TelegramChat {
						id: 1001,
						title: None,
						kind: "private".to_string(),
					},
					from: Some(TelegramUser {
						id: 9,
						is_bot: true,
						username: None,
					}),
					text: Some("ignored".to_string()),
				}),
				callback_query: None,
			})
			.expect_err("bot message should be ignored");

		assert_eq!(error, TelegramConnectorError::BotOriginIgnored);
	}

	#[test]
	fn into_interaction_maps_callback_query_to_approval_action() {
		let connector = TelegramConnector;
		let interaction = connector
			.interaction_from_update(TelegramUpdate {
				update_id: 42,
				message: None,
				callback_query: Some(TelegramCallbackQuery {
					id: "callback-1".to_string(),
					from: TelegramUser {
						id: 77,
						is_bot: false,
						username: Some("jojo".to_string()),
					},
					message: Some(TelegramMessage {
						message_id: 7,
						chat: TelegramChat {
							id: 1001,
							title: None,
							kind: "private".to_string(),
						},
						from: Some(TelegramUser {
							id: 9,
							is_bot: true,
							username: Some("kiki".to_string()),
						}),
						text: Some("approve".to_string()),
					}),
					data: Some("ap:a:approval-42".to_string()),
				}),
			})
			.expect("callback query should map to approval action");

		match interaction {
			TelegramInteraction::ApprovalDecision(action) => {
				assert_eq!(action.chat_id, 1001);
				assert_eq!(action.callback_query_id, "callback-1");
				assert_eq!(action.approval_id.0, "approval-42");
				assert!(action.decision.approved);
				assert_eq!(action.decision.actor, "telegram:jojo");
				assert!(action.decision.comment.is_none());
			}
			TelegramInteraction::Request { .. } => {
				panic!("callback query should not become a request")
			}
			TelegramInteraction::ControlCommand(_) => {
				panic!("callback query should not become a control command")
			}
			TelegramInteraction::SessionCallback(_) => {
				panic!("approval callback should not become a session callback")
			}
		}
	}

	#[test]
	fn into_interaction_maps_session_callback_query() {
		let connector = TelegramConnector;
		let interaction = connector
			.interaction_from_update(TelegramUpdate {
				update_id: 48,
				message: None,
				callback_query: Some(TelegramCallbackQuery {
					id: "callback-2".to_string(),
					from: TelegramUser {
						id: 78,
						is_bot: false,
						username: Some("jojo".to_string()),
					},
					message: Some(TelegramMessage {
						message_id: 8,
						chat: TelegramChat {
							id: 1006,
							title: None,
							kind: "private".to_string(),
						},
						from: None,
						text: Some("sessions".to_string()),
					}),
					data: Some(session_select_callback_data(2, "session-9")),
				}),
			})
			.expect("session callback should be recognized");

		match interaction {
			TelegramInteraction::SessionCallback(action) => {
				assert_eq!(action.chat_id, 1006);
				assert_eq!(action.callback_query_id, "callback-2");
				assert_eq!(
					action.action,
					TelegramSessionCallbackKind::SelectSession {
						session_id: "session-9".to_string(),
						page: 2,
					}
				);
			}
			other => panic!("expected session callback, got {other:?}"),
		}
	}

	#[test]
	fn into_interaction_rejects_invalid_callback_payload() {
		let connector = TelegramConnector;
		let error = connector
			.interaction_from_update(TelegramUpdate {
				update_id: 42,
				message: None,
				callback_query: Some(TelegramCallbackQuery {
					id: "callback-1".to_string(),
					from: TelegramUser {
						id: 77,
						is_bot: false,
						username: None,
					},
					message: Some(TelegramMessage {
						message_id: 7,
						chat: TelegramChat {
							id: 1001,
							title: None,
							kind: "private".to_string(),
						},
						from: None,
						text: None,
					}),
					data: Some("ap:x:approval-42".to_string()),
				}),
			})
			.expect_err("invalid callback action should fail");

		assert_eq!(error, TelegramConnectorError::UnsupportedCallbackAction);
	}

	#[test]
	fn into_interaction_maps_case_insensitive_control_command() {
		let connector = TelegramConnector;
		let interaction = connector
			.interaction_from_update(TelegramUpdate {
				update_id: 43,
				message: Some(TelegramMessage {
					message_id: 8,
					chat: TelegramChat {
						id: 1001,
						title: None,
						kind: "private".to_string(),
					},
					from: Some(TelegramUser {
						id: 10,
						is_bot: false,
						username: Some("jojo".to_string()),
					}),
					text: Some("/StAtUs".to_string()),
				}),
				callback_query: None,
			})
			.expect("control command should be recognized");

		match interaction {
			TelegramInteraction::ControlCommand(command) => {
				assert_eq!(command.chat_id, 1001);
				assert_eq!(command.session_id, "1001");
				assert_eq!(command.command, TelegramControlCommand::Status);
				assert_eq!(command.argument, None);
			}
			other => panic!("expected control command, got {other:?}"),
		}
	}

	#[test]
	fn recognized_control_commands_preserve_inline_arguments_for_handler_validation() {
		let connector = TelegramConnector;
		let interaction = connector
			.interaction_from_update(TelegramUpdate {
				update_id: 44,
				message: Some(TelegramMessage {
					message_id: 9,
					chat: TelegramChat {
						id: 1002,
						title: None,
						kind: "private".to_string(),
					},
					from: Some(TelegramUser {
						id: 11,
						is_bot: false,
						username: None,
					}),
					text: Some("/clear now".to_string()),
				}),
				callback_query: None,
			})
			.expect("control command should be recognized");

		match interaction {
			TelegramInteraction::ControlCommand(command) => {
				assert_eq!(command.command, TelegramControlCommand::Clear);
				assert_eq!(command.argument.as_deref(), Some("now"));
			}
			other => panic!("expected control command, got {other:?}"),
		}
	}

	#[test]
	fn control_commands_support_group_bot_mentions() {
		let connector = TelegramConnector;
		let interaction = connector
			.interaction_from_update(TelegramUpdate {
				update_id: 45,
				message: Some(TelegramMessage {
					message_id: 10,
					chat: TelegramChat {
						id: 1003,
						title: None,
						kind: "private".to_string(),
					},
					from: Some(TelegramUser {
						id: 12,
						is_bot: false,
						username: Some("jojo".to_string()),
					}),
					text: Some("/sessions@roku_bot".to_string()),
				}),
				callback_query: None,
			})
			.expect("control command should be recognized");

		match interaction {
			TelegramInteraction::ControlCommand(command) => {
				assert_eq!(command.chat_id, 1003);
				assert_eq!(command.session_id, "1003");
				assert_eq!(command.command, TelegramControlCommand::Sessions);
				assert_eq!(command.argument, None);
			}
			other => panic!("expected control command, got {other:?}"),
		}
	}

	#[test]
	fn removed_planning_commands_fall_back_to_plain_requests() {
		let connector = TelegramConnector;
		let interaction = connector
			.interaction_from_update(TelegramUpdate {
				update_id: 46,
				message: Some(TelegramMessage {
					message_id: 11,
					chat: TelegramChat {
						id: 1004,
						title: None,
						kind: "private".to_string(),
					},
					from: Some(TelegramUser {
						id: 13,
						is_bot: false,
						username: None,
					}),
					text: Some("/react".to_string()),
				}),
				callback_query: None,
			})
			.expect("unknown slash text should fall back to a plain request");

		match interaction {
			TelegramInteraction::Request { request, .. } => {
				assert_eq!(request.goal, "/react");
				assert_eq!(request.planning_mode_hint, None);
			}
			other => panic!("expected plain request, got {other:?}"),
		}
	}

	#[test]
	fn unknown_slash_commands_also_fall_back_to_plain_requests() {
		let connector = TelegramConnector;
		let interaction = connector
			.interaction_from_update(TelegramUpdate {
				update_id: 47,
				message: Some(TelegramMessage {
					message_id: 12,
					chat: TelegramChat {
						id: 1005,
						title: None,
						kind: "private".to_string(),
					},
					from: Some(TelegramUser {
						id: 14,
						is_bot: false,
						username: None,
					}),
					text: Some("/foo bar".to_string()),
				}),
				callback_query: None,
			})
			.expect("unknown slash text should fall back to a plain request");

		match interaction {
			TelegramInteraction::Request { request, .. } => {
				assert_eq!(request.goal, "/foo bar");
				assert_eq!(request.planning_mode_hint, None);
			}
			other => panic!("expected plain request, got {other:?}"),
		}
	}
}
