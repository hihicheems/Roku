//! Telegram connector adapter.

mod client;
mod inbound;
mod outbound;
mod runner;

pub use client::{TelegramBotClient, TelegramBotConfig, TelegramTransportError};
pub use inbound::{
	TelegramChat, TelegramConnector, TelegramConnectorError, TelegramMessage, TelegramUpdate,
	TelegramUser,
};
pub use outbound::{TelegramOutboundMessage, TelegramParseMode};
pub use runner::{TelegramPollingRunner, TelegramRequestHandler};
