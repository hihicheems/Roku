//! Telegram connector adapter.

mod client;
mod inbound;
mod outbound;
mod runner;

pub use client::{TelegramBotClient, TelegramBotConfig, TelegramTransportError};
pub use inbound::{
	TelegramApprovalAction, TelegramCallbackQuery, TelegramChat, TelegramConnector,
	TelegramConnectorError, TelegramInteraction, TelegramMessage, TelegramSessionCommand,
	TelegramSessionCommandRequest, TelegramUpdate, TelegramUser,
};
pub use outbound::{
	TelegramInlineKeyboardButton, TelegramOutboundMessage, TelegramParseMode, TelegramReplyMarkup,
};
pub use runner::{TelegramInteractionHandler, TelegramPollingRunner};
