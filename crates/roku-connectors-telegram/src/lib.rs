//! Telegram connector adapter.

mod inbound;
mod outbound;

pub use inbound::{
	TelegramChat, TelegramConnector, TelegramConnectorError, TelegramMessage, TelegramUpdate,
	TelegramUser,
};
pub use outbound::{TelegramOutboundMessage, TelegramParseMode};
