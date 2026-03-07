use roku_common_types::{ResponseEnvelope, ResponseStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelegramParseMode {
	MarkdownV2,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelegramOutboundMessage {
	pub chat_id: i64,
	pub text: String,
	pub parse_mode: TelegramParseMode,
	pub disable_web_page_preview: bool,
}

impl TelegramOutboundMessage {
	pub fn from_response(chat_id: i64, response: &ResponseEnvelope) -> Self {
		let mut lines = vec![
			format!("Status: {}", status_label(response.status)),
			format!("Request: {}", response.request_id.0),
			format!("Message: {}", response.message),
		];
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
			parse_mode: TelegramParseMode::MarkdownV2,
			disable_web_page_preview: true,
		}
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
		assert_eq!(message.parse_mode, TelegramParseMode::MarkdownV2);
	}
}
