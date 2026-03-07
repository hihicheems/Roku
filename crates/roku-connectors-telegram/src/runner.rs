use std::thread;
use std::time::Duration;

use roku_common_types::{RequestEnvelope, ResponseEnvelope, RuntimeError};

use crate::{
	TelegramBotClient, TelegramBotConfig, TelegramConnector, TelegramConnectorError,
	TelegramOutboundMessage, TelegramTransportError, TelegramUpdate,
};

pub trait TelegramRequestHandler: Send + Sync {
	fn handle(&self, request: RequestEnvelope) -> Result<ResponseEnvelope, RuntimeError>;
}

impl<F> TelegramRequestHandler for F
where
	F: Fn(RequestEnvelope) -> Result<ResponseEnvelope, RuntimeError> + Send + Sync,
{
	fn handle(&self, request: RequestEnvelope) -> Result<ResponseEnvelope, RuntimeError> {
		self(request)
	}
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
		H: TelegramRequestHandler,
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
		H: TelegramRequestHandler,
	{
		let chat_id = update.message.as_ref().map(|message| message.chat.id);
		match self.connector.into_request(update) {
			Ok(request) => {
				let Some(chat_id) = chat_id else {
					return Ok(());
				};
				match handler.handle(request) {
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
			Err(TelegramConnectorError::BotMessageIgnored) => Ok(()),
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
}
