use std::thread;
use std::time::Duration;

use roku_common_types::{
	ApprovalDecision, ApprovalId, RequestEnvelope, ResponseEnvelope, RuntimeError,
};

use crate::{
	TelegramBotClient, TelegramBotConfig, TelegramConnector, TelegramConnectorError,
	TelegramInteraction, TelegramOutboundMessage, TelegramTransportError, TelegramUpdate,
};

pub trait TelegramInteractionHandler: Send + Sync {
	fn handle_request(&self, request: RequestEnvelope) -> Result<ResponseEnvelope, RuntimeError>;

	fn handle_approval_decision(
		&self,
		approval_id: ApprovalId,
		decision: ApprovalDecision,
	) -> Result<ResponseEnvelope, RuntimeError>;
}

pub struct TelegramPollingRunner {
	connector: TelegramConnector,
	client: TelegramBotClient,
	idle_backoff_ms: u64,
}

impl TelegramPollingRunner {
	pub fn from_env() -> Result<Self, TelegramTransportError> {
		Self::new(TelegramBotConfig::from_env()?)
	}

	pub fn new(config: TelegramBotConfig) -> Result<Self, TelegramTransportError> {
		let idle_backoff_ms = config.idle_backoff_ms;
		Ok(Self {
			connector: TelegramConnector,
			client: TelegramBotClient::new(config)?,
			idle_backoff_ms,
		})
	}

	pub fn run<H>(&self, handler: H) -> Result<(), TelegramTransportError>
	where
		H: TelegramInteractionHandler,
	{
		let mut next_offset = None;
		loop {
			let updates = self.client.get_updates(next_offset)?;
			if updates.is_empty() {
				thread::sleep(Duration::from_millis(self.idle_backoff_ms));
				continue;
			}

			for update in updates {
				next_offset = Some(update.update_id.saturating_add(1));
				self.process_update(&handler, update)?;
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

		match self.connector.into_interaction(update) {
			Ok(TelegramInteraction::Request { chat_id, request }) => {
				self.dispatch_response(chat_id, handler.handle_request(request))
			}
			Ok(TelegramInteraction::ApprovalDecision(action)) => {
				let response =
					handler.handle_approval_decision(action.approval_id, action.decision);
				self.client.answer_callback_query(
					&action.callback_query_id,
					callback_acknowledgement(&response),
				)?;
				self.dispatch_response(action.chat_id, response)
			}
			Err(TelegramConnectorError::BotOriginIgnored) => Ok(()),
			Err(error) => {
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
		response: Result<ResponseEnvelope, RuntimeError>,
	) -> Result<(), TelegramTransportError> {
		match response {
			Ok(response) => self
				.client
				.send_message(&TelegramOutboundMessage::from_response(chat_id, &response)),
			Err(error) => self
				.client
				.send_message(&TelegramOutboundMessage::from_error(
					chat_id,
					&error.message,
				)),
		}
	}
}

fn callback_acknowledgement(response: &Result<ResponseEnvelope, RuntimeError>) -> &str {
	match response {
		Ok(response) => match response.status {
			roku_common_types::ResponseStatus::Succeeded => "Approval recorded",
			roku_common_types::ResponseStatus::PendingApproval => "Still waiting on approval",
			roku_common_types::ResponseStatus::Failed => "Decision processed with failure",
		},
		Err(_) => "Approval decision failed",
	}
}
