use std::env;

use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{TelegramOutboundMessage, TelegramParseMode, TelegramReplyMarkup, TelegramUpdate};

const DEFAULT_TELEGRAM_BASE_URL: &str = "https://api.telegram.org";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelegramBotConfig {
	pub token: String,
	pub api_base_url: String,
	pub poll_timeout_seconds: u16,
	pub idle_backoff_ms: u64,
}

impl TelegramBotConfig {
	pub fn from_env() -> Result<Self, TelegramTransportError> {
		let token = env::var("TELOXIDE_TOKEN")
			.ok()
			.filter(|value| !value.trim().is_empty())
			.or_else(|| {
				env::var("TELEGRAM_BOT_TOKEN")
					.ok()
					.filter(|value| !value.trim().is_empty())
			})
			.ok_or(TelegramTransportError::MissingBotToken)?;

		let api_base_url = env::var("TELEGRAM_API_BASE_URL")
			.ok()
			.filter(|value| !value.trim().is_empty())
			.unwrap_or_else(|| DEFAULT_TELEGRAM_BASE_URL.to_string());
		let poll_timeout_seconds = env_var_u16("TELEGRAM_POLL_TIMEOUT_SECONDS")?.unwrap_or(30);
		let idle_backoff_ms = env_var_u64("TELEGRAM_IDLE_BACKOFF_MS")?.unwrap_or(500);

		Ok(Self {
			token,
			api_base_url,
			poll_timeout_seconds,
			idle_backoff_ms,
		})
	}
}

pub struct TelegramBotClient {
	client: Client,
	config: TelegramBotConfig,
}

impl TelegramBotClient {
	pub fn new(config: TelegramBotConfig) -> Result<Self, TelegramTransportError> {
		let client = Client::builder()
			.build()
			.map_err(TelegramTransportError::HttpClientBuild)?;
		Ok(Self { client, config })
	}

	pub fn get_updates(
		&self,
		offset: Option<u64>,
	) -> Result<Vec<TelegramUpdate>, TelegramTransportError> {
		let response = self
			.client
			.post(self.endpoint("getUpdates"))
			.json(&GetUpdatesRequest {
				offset,
				timeout: self.config.poll_timeout_seconds,
				allowed_updates: vec!["message".to_string(), "callback_query".to_string()],
			})
			.send()
			.map_err(TelegramTransportError::HttpRequest)?;
		parse_api_response::<Vec<TelegramUpdate>>(response)
	}

	pub fn send_message(
		&self,
		message: &TelegramOutboundMessage,
	) -> Result<(), TelegramTransportError> {
		let response = self
			.client
			.post(self.endpoint("sendMessage"))
			.json(&SendMessageRequest {
				chat_id: message.chat_id,
				text: message.text.clone(),
				parse_mode: parse_mode_label(message.parse_mode).map(str::to_string),
				disable_web_page_preview: message.disable_web_page_preview,
				reply_markup: message.reply_markup.clone(),
			})
			.send()
			.map_err(TelegramTransportError::HttpRequest)?;
		let _: serde_json::Value = parse_api_response(response)?;
		Ok(())
	}

	pub fn answer_callback_query(
		&self,
		callback_query_id: &str,
		text: &str,
	) -> Result<(), TelegramTransportError> {
		let response = self
			.client
			.post(self.endpoint("answerCallbackQuery"))
			.json(&AnswerCallbackQueryRequest {
				callback_query_id: callback_query_id.to_string(),
				text: Some(text.to_string()),
			})
			.send()
			.map_err(TelegramTransportError::HttpRequest)?;
		let _: serde_json::Value = parse_api_response(response)?;
		Ok(())
	}

	fn endpoint(&self, method: &str) -> String {
		format!(
			"{}/bot{}/{}",
			self.config.api_base_url.trim_end_matches('/'),
			self.config.token,
			method
		)
	}
}

#[derive(Debug, Error)]
pub enum TelegramTransportError {
	#[error("missing required telegram bot token in TELOXIDE_TOKEN or TELEGRAM_BOT_TOKEN")]
	MissingBotToken,
	#[error("invalid environment variable {key}: {message}")]
	InvalidEnv { key: &'static str, message: String },
	#[error("failed to construct telegram http client: {0}")]
	HttpClientBuild(reqwest::Error),
	#[error("telegram http request failed: {0}")]
	HttpRequest(reqwest::Error),
	#[error("telegram api returned an error: {0}")]
	Api(String),
}

#[derive(Debug, Serialize)]
struct GetUpdatesRequest {
	#[serde(skip_serializing_if = "Option::is_none")]
	offset: Option<u64>,
	timeout: u16,
	allowed_updates: Vec<String>,
}

#[derive(Debug, Serialize)]
struct SendMessageRequest {
	chat_id: i64,
	text: String,
	#[serde(skip_serializing_if = "Option::is_none")]
	parse_mode: Option<String>,
	disable_web_page_preview: bool,
	#[serde(skip_serializing_if = "Option::is_none")]
	reply_markup: Option<TelegramReplyMarkup>,
}

#[derive(Debug, Serialize)]
struct AnswerCallbackQueryRequest {
	callback_query_id: String,
	#[serde(skip_serializing_if = "Option::is_none")]
	text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TelegramApiResponse<T> {
	ok: bool,
	result: Option<T>,
	description: Option<String>,
}

fn parse_api_response<T: for<'de> Deserialize<'de>>(
	response: reqwest::blocking::Response,
) -> Result<T, TelegramTransportError> {
	let status = response.status();
	let body = response
		.text()
		.map_err(TelegramTransportError::HttpRequest)?;
	if !status.is_success() {
		return Err(TelegramTransportError::Api(format!(
			"http status {status}: {body}"
		)));
	}

	let parsed: TelegramApiResponse<T> = serde_json::from_str(&body).map_err(|error| {
		TelegramTransportError::Api(format!("failed to parse telegram response: {error}"))
	})?;
	if !parsed.ok {
		return Err(TelegramTransportError::Api(
			parsed
				.description
				.unwrap_or_else(|| "telegram response was not ok".to_string()),
		));
	}

	parsed.result.ok_or_else(|| {
		TelegramTransportError::Api("telegram response contained no result".to_string())
	})
}

fn parse_mode_label(mode: TelegramParseMode) -> Option<&'static str> {
	match mode {
		TelegramParseMode::PlainText => None,
		TelegramParseMode::MarkdownV2 => Some("MarkdownV2"),
	}
}

fn env_var_u16(key: &'static str) -> Result<Option<u16>, TelegramTransportError> {
	match env::var(key) {
		Ok(value) if !value.trim().is_empty() => {
			value
				.parse::<u16>()
				.map(Some)
				.map_err(|error| TelegramTransportError::InvalidEnv {
					key,
					message: error.to_string(),
				})
		}
		Ok(_) | Err(env::VarError::NotPresent) => Ok(None),
		Err(error) => Err(TelegramTransportError::InvalidEnv {
			key,
			message: error.to_string(),
		}),
	}
}

fn env_var_u64(key: &'static str) -> Result<Option<u64>, TelegramTransportError> {
	match env::var(key) {
		Ok(value) if !value.trim().is_empty() => {
			value
				.parse::<u64>()
				.map(Some)
				.map_err(|error| TelegramTransportError::InvalidEnv {
					key,
					message: error.to_string(),
				})
		}
		Ok(_) | Err(env::VarError::NotPresent) => Ok(None),
		Err(error) => Err(TelegramTransportError::InvalidEnv {
			key,
			message: error.to_string(),
		}),
	}
}
