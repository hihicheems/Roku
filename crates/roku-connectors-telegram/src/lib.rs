//! Telegram connector adapter.

use roku_common_types::{RequestEnvelope, RequestId, ResponseEnvelope};

#[derive(Debug, Clone)]
pub struct TelegramUpdate {
	pub update_id: u64,
	pub chat_id: i64,
	pub text: String,
}

#[derive(Debug, Default)]
pub struct TelegramConnector;

impl TelegramConnector {
	pub fn into_request(&self, update: TelegramUpdate) -> RequestEnvelope {
		RequestEnvelope {
			request_id: RequestId(format!("tg-{}", update.update_id)),
			session_id: update.chat_id.to_string(),
			goal: update.text,
		}
	}

	pub fn into_response_text(&self, response: &ResponseEnvelope) -> String {
		response.message.clone()
	}
}
