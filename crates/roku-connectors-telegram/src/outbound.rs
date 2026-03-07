use serde::Serialize;

use roku_common_types::{ApprovalId, ResponseEnvelope, ResponseStatus};

use crate::inbound::approval_callback_data;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelegramParseMode {
	PlainText,
	MarkdownV2,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelegramOutboundMessage {
	pub chat_id: i64,
	pub text: String,
	pub parse_mode: TelegramParseMode,
	pub disable_web_page_preview: bool,
	pub reply_markup: Option<TelegramReplyMarkup>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TelegramReplyMarkup {
	pub inline_keyboard: Vec<Vec<TelegramInlineKeyboardButton>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TelegramInlineKeyboardButton {
	pub text: String,
	pub callback_data: String,
}

impl TelegramOutboundMessage {
	pub fn from_response(chat_id: i64, response: &ResponseEnvelope) -> Self {
		let mut lines = vec![
			format!("Status: {}", status_label(response.status)),
			format!("Request: {}", response.request_id.0),
			format!("Message: {}", response.message),
		];
		let approval_id = approval_id_from_response(response);
		if let Some(approval_id) = approval_id.as_ref() {
			lines.push(format!("Approval: {}", approval_id.0));
			lines.push("Action: choose Approve or Reject below.".to_string());
		}
		if !response.artifacts.is_empty() {
			lines.push("Artifacts:".to_string());
			lines.extend(
				response
					.artifacts
					.iter()
					.map(|artifact| format!("- {}", artifact)),
			);
		}

		Self {
			chat_id,
			text: lines.join("\n"),
			parse_mode: TelegramParseMode::PlainText,
			disable_web_page_preview: true,
			reply_markup: approval_id.map(|approval_id| approval_markup(&approval_id)),
		}
	}

	pub fn from_error(chat_id: i64, message: &str) -> Self {
		Self {
			chat_id,
			text: format!("Status: failed\nMessage: {message}"),
			parse_mode: TelegramParseMode::PlainText,
			disable_web_page_preview: true,
			reply_markup: None,
		}
	}
}

fn approval_id_from_response(response: &ResponseEnvelope) -> Option<ApprovalId> {
	if response.status != ResponseStatus::PendingApproval {
		return None;
	}

	response
		.artifacts
		.iter()
		.find_map(|artifact| artifact.strip_prefix("approval://"))
		.filter(|approval_id| !approval_id.trim().is_empty())
		.map(|approval_id| ApprovalId(approval_id.to_string()))
}

fn approval_markup(approval_id: &ApprovalId) -> TelegramReplyMarkup {
	TelegramReplyMarkup {
		inline_keyboard: vec![vec![
			TelegramInlineKeyboardButton {
				text: "Approve".to_string(),
				callback_data: approval_callback_data(approval_id, true),
			},
			TelegramInlineKeyboardButton {
				text: "Reject".to_string(),
				callback_data: approval_callback_data(approval_id, false),
			},
		]],
	}
}

fn status_label(status: ResponseStatus) -> &'static str {
	match status {
		ResponseStatus::Succeeded => "succeeded",
		ResponseStatus::PendingApproval => "pending_approval",
		ResponseStatus::Failed => "failed",
	}
}

#[cfg(test)]
mod tests {
	use roku_common_types::{RequestId, ResponseEnvelope};

	use super::*;

	#[test]
	fn outbound_message_formats_response() {
		let message = TelegramOutboundMessage::from_response(
			1001,
			&ResponseEnvelope {
				request_id: RequestId("req-1".to_string()),
				status: ResponseStatus::Succeeded,
				message: "task succeeded".to_string(),
				artifacts: vec!["artifact://task/result".to_string()],
			},
		);

		assert_eq!(message.chat_id, 1001);
		assert!(message.text.contains("Status: succeeded"));
		assert!(message.text.contains("artifact://task/result"));
		assert_eq!(message.parse_mode, TelegramParseMode::PlainText);
		assert!(message.reply_markup.is_none());
	}

	#[test]
	fn outbound_message_adds_approval_markup_for_pending_approval() {
		let message = TelegramOutboundMessage::from_response(
			1001,
			&ResponseEnvelope {
				request_id: RequestId("req-1".to_string()),
				status: ResponseStatus::PendingApproval,
				message: "approval required".to_string(),
				artifacts: vec!["approval://approval-42".to_string()],
			},
		);

		assert!(message.text.contains("Approval: approval-42"));
		let markup = message
			.reply_markup
			.expect("pending approval should render inline keyboard");
		assert_eq!(markup.inline_keyboard.len(), 1);
		assert_eq!(markup.inline_keyboard[0].len(), 2);
		assert_eq!(
			markup.inline_keyboard[0][0].callback_data,
			"approval:approve:approval-42"
		);
		assert_eq!(
			markup.inline_keyboard[0][1].callback_data,
			"approval:reject:approval-42"
		);
	}

	#[test]
	fn outbound_message_formats_error_as_plain_text() {
		let message = TelegramOutboundMessage::from_error(1001, "runtime exploded");
		assert_eq!(message.chat_id, 1001);
		assert!(message.text.contains("Status: failed"));
		assert!(message.text.contains("runtime exploded"));
		assert_eq!(message.parse_mode, TelegramParseMode::PlainText);
		assert!(message.reply_markup.is_none());
	}
}
