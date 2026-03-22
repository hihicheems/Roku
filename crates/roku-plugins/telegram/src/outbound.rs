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

use serde::Serialize;

use roku_common_types::{ApprovalId, RequestEnvelope, ResponseEnvelope, ResponseStatus};

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

#[derive(Debug, Clone)]
pub struct TelegramHandlerResponse {
	pub response: ResponseEnvelope,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct TelegramRenderOptions {
	pub include_request_metadata: bool,
	pub show_attachments: bool,
}

impl TelegramOutboundMessage {
	pub fn progress_notice(chat_id: i64, request: &RequestEnvelope) -> Self {
		Self {
			chat_id,
			text: [
				"*Status:* running".to_string(),
				format!("*Request:* {}", escape_markdown_v2(&request.request_id.0)),
				"*Message:* Processing your request\\. I will send the final result in a separate message\\."
					.to_string(),
				format!("*Goal:* {}", escape_markdown_v2(&request.goal)),
			]
			.join("\n"),
			parse_mode: TelegramParseMode::MarkdownV2,
			disable_web_page_preview: true,
			reply_markup: None,
		}
	}

	pub fn from_response(chat_id: i64, response: &ResponseEnvelope) -> Self {
		Self::from_response_with_options(chat_id, response, TelegramRenderOptions::default())
	}

	pub fn from_handler_response(chat_id: i64, handler_response: &TelegramHandlerResponse) -> Self {
		Self::from_handler_response_with_options(
			chat_id,
			handler_response,
			TelegramRenderOptions::default(),
		)
	}

	pub(crate) fn from_response_with_options(
		chat_id: i64,
		response: &ResponseEnvelope,
		options: TelegramRenderOptions,
	) -> Self {
		let attachments = classify_attachments(response);
		match response.status {
			ResponseStatus::Succeeded => plain_response_message(
				chat_id,
				&response.request_id.0,
				&response.message,
				options,
				Some(&attachments),
			),
			ResponseStatus::Failed => plain_response_message(
				chat_id,
				&response.request_id.0,
				&response.message,
				options,
				Some(&attachments),
			),
			ResponseStatus::PendingApproval => {
				let mut lines = vec![response.message.clone()];
				if options.include_request_metadata {
					lines.insert(0, format!("Request: {}", response.request_id.0));
				}
				if options.show_attachments {
					append_attachment_lines(&mut lines, &attachments);
				}

				Self {
					chat_id,
					text: lines.join("\n"),
					parse_mode: TelegramParseMode::PlainText,
					disable_web_page_preview: true,
					reply_markup: attachments.approval_id.as_ref().map(approval_markup),
				}
			}
		}
	}

	pub(crate) fn from_handler_response_with_options(
		chat_id: i64,
		handler_response: &TelegramHandlerResponse,
		options: TelegramRenderOptions,
	) -> Self {
		let mut message =
			Self::from_response_with_options(chat_id, &handler_response.response, options);
		if let Some(reply_markup) = handler_response.reply_markup.clone() {
			message.reply_markup = Some(reply_markup);
		}
		message
	}

	pub fn from_error(chat_id: i64, message: &str) -> Self {
		Self {
			chat_id,
			text: message.to_string(),
			parse_mode: TelegramParseMode::PlainText,
			disable_web_page_preview: true,
			reply_markup: None,
		}
	}
}

impl From<ResponseEnvelope> for TelegramHandlerResponse {
	fn from(response: ResponseEnvelope) -> Self {
		Self {
			response,
			reply_markup: None,
		}
	}
}

impl TelegramHandlerResponse {
	pub fn with_reply_markup(mut self, reply_markup: TelegramReplyMarkup) -> Self {
		self.reply_markup = Some(reply_markup);
		self
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

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct ResponseAttachments {
	approval_id: Option<ApprovalId>,
	artifacts: Vec<String>,
	experiments: Vec<String>,
	references: Vec<String>,
}

fn classify_attachments(response: &ResponseEnvelope) -> ResponseAttachments {
	let approval_id = approval_id_from_response(response);
	let mut attachments = ResponseAttachments {
		approval_id,
		artifacts: Vec::new(),
		experiments: Vec::new(),
		references: Vec::new(),
	};
	for artifact in &response.artifacts {
		if artifact.starts_with("approval://") {
			continue;
		}
		if artifact.starts_with("artifact://") {
			attachments.artifacts.push(artifact.clone());
			continue;
		}
		if artifact.starts_with("experiment://") {
			attachments.experiments.push(artifact.clone());
			continue;
		}
		attachments.references.push(artifact.clone());
	}

	attachments
}

fn attachment_line(kind: &str, value: &str) -> String {
	let label = display_label(value);
	format!("• {kind} {label} ({value})")
}

fn display_label(value: &str) -> String {
	value.rsplit('/').next().unwrap_or(value).to_string()
}

fn approval_markup(approval_id: &ApprovalId) -> TelegramReplyMarkup {
	TelegramReplyMarkup {
		inline_keyboard: vec![vec![
			TelegramInlineKeyboardButton {
				text: "✅ Approve".to_string(),
				callback_data: approval_callback_data(approval_id, true),
			},
			TelegramInlineKeyboardButton {
				text: "❌ Reject".to_string(),
				callback_data: approval_callback_data(approval_id, false),
			},
		]],
	}
}

fn escape_markdown_v2(value: &str) -> String {
	let mut escaped = String::with_capacity(value.len());
	for ch in value.chars() {
		match ch {
			'_' | '*' | '[' | ']' | '(' | ')' | '~' | '`' | '>' | '#' | '+' | '-' | '=' | '|'
			| '{' | '}' | '.' | '!' | '\\' => {
				escaped.push('\\');
				escaped.push(ch);
			}
			_ => escaped.push(ch),
		}
	}
	escaped
}

fn plain_response_message(
	chat_id: i64,
	request_id: &str,
	message: &str,
	options: TelegramRenderOptions,
	attachments: Option<&ResponseAttachments>,
) -> TelegramOutboundMessage {
	let mut lines = Vec::new();
	if options.include_request_metadata {
		lines.push(format!("Request: {request_id}"));
	}
	lines.push(message.to_string());
	if options.show_attachments
		&& let Some(attachments) = attachments
	{
		append_attachment_lines(&mut lines, attachments);
	}

	TelegramOutboundMessage {
		chat_id,
		text: lines.join("\n"),
		parse_mode: TelegramParseMode::PlainText,
		disable_web_page_preview: true,
		reply_markup: None,
	}
}

fn append_attachment_lines(lines: &mut Vec<String>, attachments: &ResponseAttachments) {
	if !attachments.artifacts.is_empty() {
		lines.push("Artifacts:".to_string());
		lines.extend(
			attachments
				.artifacts
				.iter()
				.map(|artifact| attachment_line("artifact", artifact)),
		);
	}
	if !attachments.experiments.is_empty() {
		lines.push("Experiments:".to_string());
		lines.extend(
			attachments
				.experiments
				.iter()
				.map(|experiment| attachment_line("experiment", experiment)),
		);
	}
	if !attachments.references.is_empty() {
		lines.push("References:".to_string());
		lines.extend(
			attachments
				.references
				.iter()
				.map(|reference| attachment_line("reference", reference)),
		);
	}
}

#[cfg(test)]
mod tests {
	use roku_common_types::{RequestEnvelope, RequestId, ResponseEnvelope};

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
		assert_eq!(message.text, "task succeeded");
		assert_eq!(message.parse_mode, TelegramParseMode::PlainText);
		assert!(message.reply_markup.is_none());
		assert!(!message.text.contains("artifact://"));
	}

	#[test]
	fn outbound_message_formats_progress_notice() {
		let message = TelegramOutboundMessage::progress_notice(
			1001,
			&RequestEnvelope {
				request_id: RequestId("req-9".to_string()),
				session_id: "chat-1".to_string(),
				goal: "analyze the latest artifacts".to_string(),
				planning_mode_hint: None,
				conversation_history: Vec::new(),
			},
		);

		assert_eq!(message.chat_id, 1001);
		assert_eq!(message.parse_mode, TelegramParseMode::MarkdownV2);
		assert!(message.text.contains("*Status:* running"));
		assert!(message.text.contains("*Request:* req\\-9"));
		assert!(
			message
				.text
				.contains("*Goal:* analyze the latest artifacts")
		);
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

		assert_eq!(message.text, "approval required");
		let markup = message
			.reply_markup
			.expect("pending approval should render inline keyboard");
		assert_eq!(markup.inline_keyboard.len(), 1);
		assert_eq!(markup.inline_keyboard[0].len(), 2);
		assert_eq!(markup.inline_keyboard[0][0].text, "✅ Approve");
		assert_eq!(markup.inline_keyboard[0][1].text, "❌ Reject");
		assert_eq!(
			markup.inline_keyboard[0][0].callback_data,
			"ap:a:approval-42"
		);
		assert_eq!(
			markup.inline_keyboard[0][1].callback_data,
			"ap:r:approval-42"
		);
	}

	#[test]
	fn outbound_message_pending_approval_uses_runtime_message_as_authority() {
		let message = TelegramOutboundMessage::from_response_with_options(
			1001,
			&ResponseEnvelope {
				request_id: RequestId("req-approval".to_string()),
				status: ResponseStatus::PendingApproval,
				message: "approval required: Run command rm -rf tmp from /workspace".to_string(),
				artifacts: vec!["approval://approval-77".to_string()],
			},
			TelegramRenderOptions {
				include_request_metadata: false,
				show_attachments: false,
			},
		);

		assert_eq!(
			message.text,
			"approval required: Run command rm -rf tmp from /workspace"
		);
	}

	#[test]
	fn outbound_message_formats_error_as_plain_text() {
		let message = TelegramOutboundMessage::from_error(1001, "runtime exploded");
		assert_eq!(message.chat_id, 1001);
		assert_eq!(message.text, "runtime exploded");
		assert_eq!(message.parse_mode, TelegramParseMode::PlainText);
		assert!(message.reply_markup.is_none());
	}

	#[test]
	fn outbound_message_groups_experiments_and_references() {
		let message = TelegramOutboundMessage::from_response_with_options(
			1001,
			&ResponseEnvelope {
				request_id: RequestId("req-2".to_string()),
				status: ResponseStatus::Succeeded,
				message: "experiment completed".to_string(),
				artifacts: vec![
					"experiment://run-7".to_string(),
					"https://example.com/report".to_string(),
				],
			},
			TelegramRenderOptions {
				include_request_metadata: false,
				show_attachments: true,
			},
		);

		assert!(message.text.contains("Experiments:"));
		assert!(message.text.contains("run-7 (experiment://run-7)"));
		assert!(message.text.contains("References:"));
		assert!(message.text.contains("https://example.com/report"));
	}

	#[test]
	fn outbound_message_can_include_request_metadata_when_enabled() {
		let message = TelegramOutboundMessage::from_response_with_options(
			1001,
			&ResponseEnvelope {
				request_id: RequestId("req-3".to_string()),
				status: ResponseStatus::Succeeded,
				message: "done".to_string(),
				artifacts: Vec::new(),
			},
			TelegramRenderOptions {
				include_request_metadata: true,
				show_attachments: false,
			},
		);

		assert_eq!(message.text, "Request: req-3\ndone");
	}
}
