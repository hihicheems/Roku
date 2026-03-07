use roku_common_types::{RequestEnvelope, RequestId};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramUpdate {
	pub update_id: u64,
	pub message: Option<TelegramMessage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramMessage {
	pub message_id: i64,
	pub chat: TelegramChat,
	pub from: Option<TelegramUser>,
	pub text: Option<String>,
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

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TelegramConnectorError {
	#[error("telegram update does not contain a message")]
	MissingMessage,
	#[error("telegram message does not contain text")]
	MissingText,
	#[error("telegram bot messages are ignored")]
	BotMessageIgnored,
}

#[derive(Debug, Default)]
pub struct TelegramConnector;

impl TelegramConnector {
	pub fn into_request(
		&self,
		update: TelegramUpdate,
	) -> Result<RequestEnvelope, TelegramConnectorError> {
		let message = update
			.message
			.ok_or(TelegramConnectorError::MissingMessage)?;
		if message.from.as_ref().is_some_and(|user| user.is_bot) {
			return Err(TelegramConnectorError::BotMessageIgnored);
		}
		let text = message.text.ok_or(TelegramConnectorError::MissingText)?;

		Ok(RequestEnvelope {
			request_id: RequestId(format!("tg-{}", update.update_id)),
			session_id: message.chat.id.to_string(),
			goal: text,
		})
	}
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
			})
			.expect_err("bot message should be ignored");

		assert_eq!(error, TelegramConnectorError::BotMessageIgnored);
	}
}
