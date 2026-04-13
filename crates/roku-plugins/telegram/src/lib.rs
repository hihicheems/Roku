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

//! Telegram connector adapter.

mod client;
mod inbound;
pub mod markdown;
mod outbound;
mod runner;

pub use client::{
	TelegramBotClient, TelegramBotConfig, TelegramRuntimeConfig, TelegramRuntimeConfigPatch,
	TelegramTransportError,
};
pub use inbound::{
	TelegramApprovalAction, TelegramCallbackQuery, TelegramChat, TelegramConnector,
	TelegramConnectorError, TelegramControlCommand, TelegramControlCommandRequest,
	TelegramInteraction, TelegramMessage, TelegramSessionCallbackAction,
	TelegramSessionCallbackKind, TelegramUpdate, TelegramUser, session_delete_cancel_callback_data,
	session_delete_confirm_callback_data, session_page_callback_data, session_select_callback_data,
};
pub use outbound::{
	TelegramHandlerResponse, TelegramInlineKeyboardButton, TelegramOutboundMessage,
	TelegramParseMode, TelegramReplyMarkup,
};
pub use runner::{TelegramInteractionHandler, TelegramPollingRunner};
