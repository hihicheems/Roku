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
	Request {
		chat_id: i64,
		request: RequestEnvelope,
	},
	ApprovalDecision(TelegramApprovalAction),
}

#[derive(Debug, Clone)]
pub struct TelegramApprovalAction {
	pub chat_id: i64,
	pub callback_query_id: String,
	pub approval_id: ApprovalId,
	pub decision: ApprovalDecision,
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

#[derive(Debug, Default)]
pub struct TelegramConnector;

impl TelegramConnector {
	pub fn into_interaction(
		&self,
		update: TelegramUpdate,
	) -> Result<TelegramInteraction, TelegramConnectorError> {
		if let Some(callback_query) = update.callback_query {
			return self.into_approval_action(callback_query);
		}

		let chat_id = update.message.as_ref().map(|message| message.chat.id);
		let request = self.into_request(update)?;
		let chat_id = chat_id.ok_or(TelegramConnectorError::MissingMessage)?;

		Ok(TelegramInteraction::Request { chat_id, request })
	}

	pub fn into_request(
		&self,
		update: TelegramUpdate,
	) -> Result<RequestEnvelope, TelegramConnectorError> {
		let message = update
			.message
			.ok_or(TelegramConnectorError::MissingMessage)?;
		if message.from.as_ref().is_some_and(|user| user.is_bot) {
			return Err(TelegramConnectorError::BotOriginIgnored);
		}
		let text = message.text.ok_or(TelegramConnectorError::MissingText)?;

		Ok(RequestEnvelope {
			request_id: RequestId(format!("tg-{}", update.update_id)),
			session_id: message.chat.id.to_string(),
			goal: text,
		})
	}

	fn into_approval_action(
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
		let (approval_id, approved) = parse_approval_callback_data(data)?;
		let message = callback_query
			.message
			.ok_or(TelegramConnectorError::MissingCallbackMessage)?;

		Ok(TelegramInteraction::ApprovalDecision(
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
		))
	}
}

pub(crate) fn approval_callback_data(approval_id: &ApprovalId, approved: bool) -> String {
	let action = if approved { "approve" } else { "reject" };
	format!("approval:{action}:{}", approval_id.0)
}

fn parse_approval_callback_data(data: &str) -> Result<(ApprovalId, bool), TelegramConnectorError> {
	let mut parts = data.splitn(3, ':');
	if parts.next() != Some("approval") {
		return Err(TelegramConnectorError::InvalidCallbackData);
	}

	let approved = match parts.next() {
		Some("approve") => true,
		Some("reject") => false,
		Some(_) => return Err(TelegramConnectorError::UnsupportedCallbackAction),
		None => return Err(TelegramConnectorError::InvalidCallbackData),
	};
	let approval_id = parts
		.next()
		.filter(|value| !value.trim().is_empty())
		.ok_or(TelegramConnectorError::InvalidCallbackData)?;

	Ok((ApprovalId(approval_id.to_string()), approved))
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

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn into_request_maps_text_message() {
		let connector = TelegramConnector;
		let request = connector
			.into_request(TelegramUpdate {
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
	}

	#[test]
	fn into_request_rejects_bot_message() {
		let connector = TelegramConnector;
		let error = connector
			.into_request(TelegramUpdate {
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
			.into_interaction(TelegramUpdate {
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
					data: Some("approval:approve:approval-42".to_string()),
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
		}
	}

	#[test]
	fn into_interaction_rejects_invalid_callback_payload() {
		let connector = TelegramConnector;
		let error = connector
			.into_interaction(TelegramUpdate {
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
					data: Some("approval:hold:approval-42".to_string()),
				}),
			})
			.expect_err("invalid callback action should fail");

		assert_eq!(error, TelegramConnectorError::UnsupportedCallbackAction);
	}
}
