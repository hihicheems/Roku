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

use std::env;

use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::outbound::TelegramRenderOptions;
use crate::{TelegramOutboundMessage, TelegramParseMode, TelegramReplyMarkup, TelegramUpdate};

const DEFAULT_TELEGRAM_BASE_URL: &str = "https://api.telegram.org";
const HARD_MAX_POLL_TIMEOUT_SECONDS: u16 = 300;
const HARD_MAX_IDLE_BACKOFF_MS: u64 = 60_000;
const HARD_MAX_POLL_ERROR_LOG_THRESHOLD: u32 = 1_000;

/// Effective non-secret Telegram transport/runtime configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelegramRuntimeConfig {
	pub api_base_url: String,
	pub poll_timeout_seconds: u16,
	pub idle_backoff_ms: u64,
	pub poll_error_log_threshold: u32,
	pub progress_notices_enabled: bool,
	pub include_request_metadata: bool,
	pub show_attachments: bool,
}

/// Partial overrides for [`TelegramRuntimeConfig`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelegramRuntimeConfigPatch {
	pub api_base_url: Option<String>,
	pub poll_timeout_seconds: Option<u16>,
	pub idle_backoff_ms: Option<u64>,
	pub poll_error_log_threshold: Option<u32>,
	pub progress_notices_enabled: Option<bool>,
	pub include_request_metadata: Option<bool>,
	pub show_attachments: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelegramBotConfig {
	pub token: String,
	pub api_base_url: String,
	pub poll_timeout_seconds: u16,
	pub idle_backoff_ms: u64,
	pub poll_error_log_threshold: u32,
	pub progress_notices_enabled: bool,
	pub include_request_metadata: bool,
	pub show_attachments: bool,
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
		let mut runtime_config = TelegramRuntimeConfig::default();
		runtime_config.apply_env_overrides()?;
		runtime_config.validate_and_clamp()?;
		Ok(runtime_config.with_token(token))
	}

	pub(crate) fn render_options(&self) -> TelegramRenderOptions {
		TelegramRenderOptions {
			include_request_metadata: self.include_request_metadata,
			show_attachments: self.show_attachments,
		}
	}
}

impl Default for TelegramRuntimeConfig {
	fn default() -> Self {
		Self {
			api_base_url: DEFAULT_TELEGRAM_BASE_URL.to_string(),
			poll_timeout_seconds: 30,
			idle_backoff_ms: 500,
			poll_error_log_threshold: 5,
			progress_notices_enabled: false,
			include_request_metadata: false,
			show_attachments: false,
		}
	}
}

impl TelegramRuntimeConfig {
	pub fn apply_patch(&mut self, patch: TelegramRuntimeConfigPatch) {
		if let Some(value) = patch.api_base_url {
			self.api_base_url = value;
		}
		if let Some(value) = patch.poll_timeout_seconds {
			self.poll_timeout_seconds = value;
		}
		if let Some(value) = patch.idle_backoff_ms {
			self.idle_backoff_ms = value;
		}
		if let Some(value) = patch.poll_error_log_threshold {
			self.poll_error_log_threshold = value;
		}
		if let Some(value) = patch.progress_notices_enabled {
			self.progress_notices_enabled = value;
		}
		if let Some(value) = patch.include_request_metadata {
			self.include_request_metadata = value;
		}
		if let Some(value) = patch.show_attachments {
			self.show_attachments = value;
		}
	}

	pub fn apply_env_overrides(&mut self) -> Result<(), TelegramTransportError> {
		if let Some(value) = env_override_string("TELEGRAM_API_BASE_URL") {
			self.api_base_url = value;
		}
		if let Some(value) = env_var_u16("TELEGRAM_POLL_TIMEOUT_SECONDS")? {
			self.poll_timeout_seconds = value;
		}
		if let Some(value) = env_var_u64("TELEGRAM_IDLE_BACKOFF_MS")? {
			self.idle_backoff_ms = value;
		}
		if let Some(value) = env_var_u32("TELEGRAM_POLL_ERROR_LOG_THRESHOLD")? {
			self.poll_error_log_threshold = value;
		}
		if let Some(value) = env_var_bool("TELEGRAM_PROGRESS_NOTICES")? {
			self.progress_notices_enabled = value;
		}
		if let Some(value) = env_var_bool("TELEGRAM_INCLUDE_REQUEST_METADATA")? {
			self.include_request_metadata = value;
		}
		if let Some(value) = env_var_bool("TELEGRAM_SHOW_ATTACHMENTS")? {
			self.show_attachments = value;
		}
		if let Some(value) = env_override_string("ROKU_RUNTIME__TELEGRAM__API_BASE_URL") {
			self.api_base_url = value;
		}
		if let Some(value) = env_var_u16("ROKU_RUNTIME__TELEGRAM__POLL_TIMEOUT_SECONDS")? {
			self.poll_timeout_seconds = value;
		}
		if let Some(value) = env_var_u64("ROKU_RUNTIME__TELEGRAM__IDLE_BACKOFF_MS")? {
			self.idle_backoff_ms = value;
		}
		if let Some(value) = env_var_u32("ROKU_RUNTIME__TELEGRAM__POLL_ERROR_LOG_THRESHOLD")? {
			self.poll_error_log_threshold = value;
		}
		if let Some(value) = env_var_bool("ROKU_RUNTIME__TELEGRAM__PROGRESS_NOTICES_ENABLED")? {
			self.progress_notices_enabled = value;
		}
		if let Some(value) = env_var_bool("ROKU_RUNTIME__TELEGRAM__INCLUDE_REQUEST_METADATA")? {
			self.include_request_metadata = value;
		}
		if let Some(value) = env_var_bool("ROKU_RUNTIME__TELEGRAM__SHOW_ATTACHMENTS")? {
			self.show_attachments = value;
		}
		Ok(())
	}

	pub fn validate_and_clamp(&mut self) -> Result<(), TelegramTransportError> {
		self.api_base_url = self.api_base_url.trim().to_string();
		if self.api_base_url.is_empty() {
			return Err(TelegramTransportError::InvalidEnv {
				key: "TELEGRAM_API_BASE_URL",
				message: "value cannot be empty".to_string(),
			});
		}
		if self.poll_timeout_seconds == 0 {
			return Err(TelegramTransportError::InvalidEnv {
				key: "TELEGRAM_POLL_TIMEOUT_SECONDS",
				message: "value must be greater than zero".to_string(),
			});
		}
		if self.idle_backoff_ms == 0 {
			return Err(TelegramTransportError::InvalidEnv {
				key: "TELEGRAM_IDLE_BACKOFF_MS",
				message: "value must be greater than zero".to_string(),
			});
		}
		self.poll_timeout_seconds = self.poll_timeout_seconds.min(HARD_MAX_POLL_TIMEOUT_SECONDS);
		self.idle_backoff_ms = self.idle_backoff_ms.min(HARD_MAX_IDLE_BACKOFF_MS);
		self.poll_error_log_threshold = self
			.poll_error_log_threshold
			.min(HARD_MAX_POLL_ERROR_LOG_THRESHOLD);
		Ok(())
	}

	pub fn with_token(self, token: String) -> TelegramBotConfig {
		TelegramBotConfig {
			token,
			api_base_url: self.api_base_url,
			poll_timeout_seconds: self.poll_timeout_seconds,
			idle_backoff_ms: self.idle_backoff_ms,
			poll_error_log_threshold: self.poll_error_log_threshold,
			progress_notices_enabled: self.progress_notices_enabled,
			include_request_metadata: self.include_request_metadata,
			show_attachments: self.show_attachments,
		}
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

fn env_override_string(key: &str) -> Option<String> {
	env::var(key)
		.ok()
		.map(|value| value.trim().to_string())
		.filter(|value| !value.is_empty())
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

fn env_var_u32(key: &'static str) -> Result<Option<u32>, TelegramTransportError> {
	match env::var(key) {
		Ok(value) if !value.trim().is_empty() => {
			value
				.parse::<u32>()
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

fn env_var_bool(key: &'static str) -> Result<Option<bool>, TelegramTransportError> {
	match env::var(key) {
		Ok(value) if !value.trim().is_empty() => parse_bool_env_value(key, &value).map(Some),
		Ok(_) | Err(env::VarError::NotPresent) => Ok(None),
		Err(error) => Err(TelegramTransportError::InvalidEnv {
			key,
			message: error.to_string(),
		}),
	}
}

fn parse_bool_env_value(key: &'static str, value: &str) -> Result<bool, TelegramTransportError> {
	match value.trim().to_ascii_lowercase().as_str() {
		"1" | "true" | "yes" | "on" => Ok(true),
		"0" | "false" | "no" | "off" => Ok(false),
		_ => Err(TelegramTransportError::InvalidEnv {
			key,
			message: "expected one of true/false/1/0/yes/no/on/off".to_string(),
		}),
	}
}

#[cfg(test)]
mod tests {
	use super::{TelegramBotConfig, parse_bool_env_value};

	#[test]
	fn parse_bool_env_value_accepts_truthy_values() {
		assert!(parse_bool_env_value("KEY", "true").expect("truthy value should parse"));
		assert!(parse_bool_env_value("KEY", "YES").expect("truthy value should parse"));
		assert!(parse_bool_env_value("KEY", "1").expect("truthy value should parse"));
	}

	#[test]
	fn parse_bool_env_value_accepts_falsy_values() {
		assert!(!parse_bool_env_value("KEY", "false").expect("falsy value should parse"));
		assert!(!parse_bool_env_value("KEY", "Off").expect("falsy value should parse"));
		assert!(!parse_bool_env_value("KEY", "0").expect("falsy value should parse"));
	}

	#[test]
	fn parse_bool_env_value_rejects_unknown_values() {
		let error = parse_bool_env_value("KEY", "maybe").expect_err("invalid value should fail");
		assert!(
			error
				.to_string()
				.contains("expected one of true/false/1/0/yes/no/on/off")
		);
	}

	#[test]
	fn render_options_follow_config_flags() {
		let config = TelegramBotConfig {
			token: "token".to_string(),
			api_base_url: "https://api.telegram.org".to_string(),
			poll_timeout_seconds: 30,
			idle_backoff_ms: 500,
			poll_error_log_threshold: 5,
			progress_notices_enabled: false,
			include_request_metadata: true,
			show_attachments: true,
		};

		let options = config.render_options();
		assert!(options.include_request_metadata);
		assert!(options.show_attachments);
	}
}
