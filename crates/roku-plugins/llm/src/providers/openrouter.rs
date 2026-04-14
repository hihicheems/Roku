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
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use reqwest::Client;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use roku_common_types::{LogLevel, LogRecord, Metrics, emit_global_log};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio::sync::mpsc;

use crate::router::{LlmProvider, LlmRouter};
use crate::types::{
	GenerationRequest, Message, ModelProfile, ProviderCallError, ProviderResponse, RiskTier,
	RoutingPolicy, StreamChunk, ThinkingEffort, ToolCallBlock, estimate_prompt_tokens,
};

const OPENROUTER_PROVIDER: &str = "openrouter";
const DEFAULT_OPENROUTER_URL: &str = "https://openrouter.ai/api/v1/chat/completions";
const DEFAULT_OPENROUTER_PRIMARY_MODEL: &str = "deepseek-chat";
const DEFAULT_OPENROUTER_FALLBACK_MODELS: [&str; 1] = ["gemini-2.0-flash"];
const HARD_MAX_CONTEXT_TOKENS: u64 = 1_000_000;
const HARD_MAX_REQUEST_COST_USD: f64 = 100.0;
const HARD_MAX_LATENCY_MS: u64 = 300_000;

/// Effective non-secret OpenRouter runtime configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenRouterRuntimeConfig {
	pub primary_model: String,
	pub fallback_models: Vec<String>,
	pub app_name: Option<String>,
	pub site_url: Option<String>,
	pub base_url: String,
	pub max_context_tokens: u64,
	pub cost_per_1k_tokens_usd: f64,
	pub max_request_cost_usd: f64,
	pub max_latency_ms: u64,
}

/// Partial overrides for [`OpenRouterRuntimeConfig`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenRouterRuntimeConfigPatch {
	pub primary_model: Option<String>,
	pub fallback_models: Option<Vec<String>>,
	pub app_name: Option<String>,
	pub site_url: Option<String>,
	pub base_url: Option<String>,
	pub max_context_tokens: Option<u64>,
	pub cost_per_1k_tokens_usd: Option<f64>,
	pub max_request_cost_usd: Option<f64>,
	pub max_latency_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OpenRouterConfig {
	pub api_key: String,
	pub primary_model: String,
	pub fallback_models: Vec<String>,
	pub app_name: Option<String>,
	pub site_url: Option<String>,
	pub base_url: String,
	pub max_context_tokens: u64,
	pub cost_per_1k_tokens_usd: f64,
	pub max_request_cost_usd: f64,
	pub max_latency_ms: u64,
}

impl OpenRouterConfig {
	pub fn from_env() -> Result<Self, OpenRouterBootstrapError> {
		let api_key = env_var_required("OPENROUTER_API_KEY")?;
		let mut runtime_config = OpenRouterRuntimeConfig::default();
		runtime_config.apply_env_overrides()?;
		runtime_config.validate_and_clamp()?;
		Ok(runtime_config.with_api_key(api_key))
	}

	fn request_fallback_chain(&self, selected_model: &str) -> Vec<String> {
		let mut ordered_models = Vec::with_capacity(self.fallback_models.len().saturating_add(1));
		ordered_models.push(self.primary_model.clone());
		ordered_models.extend(self.fallback_models.iter().cloned());

		let start_index = ordered_models
			.iter()
			.position(|model| model == selected_model)
			.map(|index| index.saturating_add(1))
			.unwrap_or_default();

		dedupe_model_chain(
			selected_model,
			ordered_models.into_iter().skip(start_index).collect(),
		)
	}
}

impl Default for OpenRouterRuntimeConfig {
	fn default() -> Self {
		Self {
			primary_model: normalize_model_id(DEFAULT_OPENROUTER_PRIMARY_MODEL),
			fallback_models: default_fallback_models(),
			app_name: None,
			site_url: None,
			base_url: DEFAULT_OPENROUTER_URL.to_string(),
			max_context_tokens: 128_000,
			cost_per_1k_tokens_usd: 0.0,
			max_request_cost_usd: 1.0,
			max_latency_ms: 60_000,
		}
	}
}

impl OpenRouterRuntimeConfig {
	pub fn apply_patch(&mut self, patch: OpenRouterRuntimeConfigPatch) {
		if let Some(value) = patch.primary_model {
			self.primary_model = value;
		}
		if let Some(value) = patch.fallback_models {
			self.fallback_models = value;
		}
		if let Some(value) = patch.app_name {
			self.app_name = Some(value);
		}
		if let Some(value) = patch.site_url {
			self.site_url = Some(value);
		}
		if let Some(value) = patch.base_url {
			self.base_url = value;
		}
		if let Some(value) = patch.max_context_tokens {
			self.max_context_tokens = value;
		}
		if let Some(value) = patch.cost_per_1k_tokens_usd {
			self.cost_per_1k_tokens_usd = value;
		}
		if let Some(value) = patch.max_request_cost_usd {
			self.max_request_cost_usd = value;
		}
		if let Some(value) = patch.max_latency_ms {
			self.max_latency_ms = value;
		}
	}

	pub fn apply_env_overrides(&mut self) -> Result<(), OpenRouterBootstrapError> {
		if let Some(value) = env_override_string("OPENROUTER_PRIMARY_MODEL")
			.or_else(|| env_override_string("OPENROUTER_MODEL"))
		{
			self.primary_model = value;
		}
		if let Some(value) = env_override_string("OPENROUTER_FALLBACK_MODELS") {
			self.fallback_models = parse_model_list(&value);
		}
		if let Some(value) = env_override_string("OPENROUTER_APP_NAME") {
			self.app_name = Some(value);
		}
		if let Some(value) = env_override_string("OPENROUTER_SITE_URL") {
			self.site_url = Some(value);
		}
		if let Some(value) = env_override_string("OPENROUTER_BASE_URL") {
			self.base_url = value;
		}
		if let Some(value) = env_var_u64("OPENROUTER_MAX_CONTEXT_TOKENS")? {
			self.max_context_tokens = value;
		}
		if let Some(value) = env_var_f64("OPENROUTER_COST_PER_1K_TOKENS_USD")? {
			self.cost_per_1k_tokens_usd = value;
		}
		if let Some(value) = env_var_f64("OPENROUTER_MAX_REQUEST_COST_USD")? {
			self.max_request_cost_usd = value;
		}
		if let Some(value) = env_var_u64("OPENROUTER_MAX_LATENCY_MS")? {
			self.max_latency_ms = value;
		}
		if let Some(value) = env_override_string("ROKU_RUNTIME__LLM__OPENROUTER__PRIMARY_MODEL") {
			self.primary_model = value;
		}
		if let Some(value) = env_override_string("ROKU_RUNTIME__LLM__OPENROUTER__FALLBACK_MODELS") {
			self.fallback_models = parse_model_list(&value);
		}
		if let Some(value) = env_override_string("ROKU_RUNTIME__LLM__OPENROUTER__APP_NAME") {
			self.app_name = Some(value);
		}
		if let Some(value) = env_override_string("ROKU_RUNTIME__LLM__OPENROUTER__SITE_URL") {
			self.site_url = Some(value);
		}
		if let Some(value) = env_override_string("ROKU_RUNTIME__LLM__OPENROUTER__BASE_URL") {
			self.base_url = value;
		}
		if let Some(value) = env_override_u64("ROKU_RUNTIME__LLM__OPENROUTER__MAX_CONTEXT_TOKENS")?
		{
			self.max_context_tokens = value;
		}
		if let Some(value) =
			env_override_f64("ROKU_RUNTIME__LLM__OPENROUTER__COST_PER_1K_TOKENS_USD")?
		{
			self.cost_per_1k_tokens_usd = value;
		}
		if let Some(value) =
			env_override_f64("ROKU_RUNTIME__LLM__OPENROUTER__MAX_REQUEST_COST_USD")?
		{
			self.max_request_cost_usd = value;
		}
		if let Some(value) = env_override_u64("ROKU_RUNTIME__LLM__OPENROUTER__MAX_LATENCY_MS")? {
			self.max_latency_ms = value;
		}
		Ok(())
	}

	pub fn validate_and_clamp(&mut self) -> Result<(), OpenRouterBootstrapError> {
		self.primary_model = normalize_model_id(&self.primary_model);
		self.fallback_models =
			dedupe_model_chain(&self.primary_model, self.fallback_models.clone());
		self.base_url = self.base_url.trim().to_string();
		self.app_name = self
			.app_name
			.take()
			.map(|value| value.trim().to_string())
			.filter(|value| !value.is_empty());
		self.site_url = self
			.site_url
			.take()
			.map(|value| value.trim().to_string())
			.filter(|value| !value.is_empty());
		if self.base_url.is_empty() {
			return Err(OpenRouterBootstrapError::InvalidEnv {
				key: "OPENROUTER_BASE_URL",
				message: "value cannot be empty".to_string(),
			});
		}
		if self.max_context_tokens == 0 {
			return Err(OpenRouterBootstrapError::InvalidEnv {
				key: "OPENROUTER_MAX_CONTEXT_TOKENS",
				message: "value must be greater than zero".to_string(),
			});
		}
		if self.cost_per_1k_tokens_usd < 0.0 {
			return Err(OpenRouterBootstrapError::InvalidEnv {
				key: "OPENROUTER_COST_PER_1K_TOKENS_USD",
				message: "value must be non-negative".to_string(),
			});
		}
		if self.max_request_cost_usd <= 0.0 {
			return Err(OpenRouterBootstrapError::InvalidEnv {
				key: "OPENROUTER_MAX_REQUEST_COST_USD",
				message: "value must be greater than zero".to_string(),
			});
		}
		if self.max_latency_ms == 0 {
			return Err(OpenRouterBootstrapError::InvalidEnv {
				key: "OPENROUTER_MAX_LATENCY_MS",
				message: "value must be greater than zero".to_string(),
			});
		}
		self.max_context_tokens = self.max_context_tokens.min(HARD_MAX_CONTEXT_TOKENS);
		self.max_request_cost_usd = self.max_request_cost_usd.min(HARD_MAX_REQUEST_COST_USD);
		self.max_latency_ms = self.max_latency_ms.min(HARD_MAX_LATENCY_MS);
		Ok(())
	}

	pub fn with_api_key(self, api_key: String) -> OpenRouterConfig {
		OpenRouterConfig {
			api_key,
			primary_model: self.primary_model,
			fallback_models: self.fallback_models,
			app_name: self.app_name,
			site_url: self.site_url,
			base_url: self.base_url,
			max_context_tokens: self.max_context_tokens,
			cost_per_1k_tokens_usd: self.cost_per_1k_tokens_usd,
			max_request_cost_usd: self.max_request_cost_usd,
			max_latency_ms: self.max_latency_ms,
		}
	}
}

#[derive(Debug, Error)]
pub enum OpenRouterBootstrapError {
	#[error("missing required environment variable: {0}")]
	MissingEnv(&'static str),
	#[error("invalid environment variable {key}: {message}")]
	InvalidEnv { key: &'static str, message: String },
	#[error("failed to construct openrouter http client: {0}")]
	HttpClient(#[from] reqwest::Error),
}

pub struct OpenRouterProvider {
	client: Client,
	config: OpenRouterConfig,
}

impl OpenRouterProvider {
	pub fn new(config: OpenRouterConfig) -> Result<Self, OpenRouterBootstrapError> {
		let client = Client::builder()
			.connect_timeout(std::time::Duration::from_secs(30))
			.build()?;
		Ok(Self { client, config })
	}

	/// Stream a generation request, sending [`StreamChunk`] events through
	/// `tx`.  A [`StreamChunk::Done`] is always sent as the final event.
	pub async fn stream(
		&self,
		model: &ModelProfile,
		request: &GenerationRequest,
		tx: mpsc::Sender<StreamChunk>,
	) -> Result<ProviderResponse, ProviderCallError> {
		let attempt_models = attempt_model_sequence(&self.config, &model.model_id);
		let headers = build_headers(&self.config)?;

		// Use the first model only for streaming (no explicit fallback loop in
		// the streaming path; the router handles provider-level retries).
		let requested_model = attempt_models
			.first()
			.ok_or_else(|| ProviderCallError::non_retryable("no models available for streaming"))?
			.clone();

		self.stream_once(&headers, &requested_model, request, tx)
			.await
	}

	async fn stream_once(
		&self,
		headers: &HeaderMap,
		requested_model: &str,
		request: &GenerationRequest,
		tx: mpsc::Sender<StreamChunk>,
	) -> Result<ProviderResponse, ProviderCallError> {
		let body = build_streaming_request_body(requested_model, &[], request);
		let started_at = Instant::now();

		let http_response = self
			.client
			.post(&self.config.base_url)
			.headers(headers.clone())
			.json(&body)
			.send()
			.await
			.map_err(classify_request_error)?;

		let status = http_response.status();
		if !status.is_success() {
			let response_body = http_response.text().await.unwrap_or_default();
			log_openrouter(
				LogLevel::Warn,
				"provider returned non-success status on streaming request",
				[
					("provider", OPENROUTER_PROVIDER.to_string()),
					("model", requested_model.to_string()),
					("status", status.to_string()),
					("body", truncate_for_log(&response_body, 800)),
				],
			);
			return Err(classify_status_error(status.as_u16(), response_body));
		}

		let mut stream = http_response.bytes_stream().eventsource();
		let mut full_text = String::new();
		let mut finish_reason: Option<String> = None;
		let mut prompt_tokens: u64 = 0;
		let mut output_tokens: u64 = 0;

		// Track stream errors to report after sending Done.
		let mut stream_error: Option<ProviderCallError> = None;
		// Track index → id mapping for streaming tool_calls.
		// OpenAI sends id only in the first chunk; subsequent deltas use index.
		let mut tool_call_index_to_id: std::collections::HashMap<u64, String> =
			std::collections::HashMap::new();

		let event_timeout = std::time::Duration::from_secs(120);

		loop {
			let event_result = match tokio::time::timeout(event_timeout, stream.next()).await {
				Ok(Some(result)) => result,
				Ok(None) => break,
				Err(_) => {
					stream_error = Some(ProviderCallError::retryable(
						"SSE stream timed out waiting for next event".to_string(),
					));
					break;
				}
			};
			let event = match event_result {
				Ok(event) => event,
				Err(error) => {
					stream_error = Some(ProviderCallError::retryable(format!(
						"SSE stream error: {error}"
					)));
					break;
				}
			};

			if event.data == "[DONE]" {
				break;
			}

			let chunk: Value = match serde_json::from_str(&event.data) {
				Ok(value) => value,
				Err(error) => {
					stream_error = Some(ProviderCallError::retryable(format!(
						"failed to parse SSE chunk: {error}"
					)));
					break;
				}
			};

			// Extract usage from chunks that carry it (last chunk or usage chunk).
			if let Some(usage) = chunk.get("usage") {
				prompt_tokens = usage
					.get("prompt_tokens")
					.and_then(Value::as_u64)
					.unwrap_or(prompt_tokens);
				output_tokens = usage
					.get("completion_tokens")
					.and_then(Value::as_u64)
					.unwrap_or(output_tokens);
			}

			let choice = chunk
				.get("choices")
				.and_then(Value::as_array)
				.and_then(|choices| choices.first());

			if let Some(choice) = choice {
				if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str)
					&& !reason.is_empty()
				{
					finish_reason = Some(reason.to_string());
				}

				if let Some(delta) = choice.get("delta") {
					// Handle text content.
					let delta_text = delta.get("content").and_then(Value::as_str).unwrap_or("");

					if !delta_text.is_empty() {
						full_text.push_str(delta_text);
						if tx
							.send(StreamChunk::TextDelta {
								text: delta_text.to_string(),
							})
							.await
							.is_err()
						{
							break;
						}
					}

					// Handle streaming tool_calls (OpenAI format).
					if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
						for tc_delta in tool_calls {
							let index = tc_delta.get("index").and_then(Value::as_u64).unwrap_or(0);
							let id = tc_delta.get("id").and_then(Value::as_str).map(String::from);
							let function = tc_delta.get("function");
							let name = function
								.and_then(|f| f.get("name"))
								.and_then(Value::as_str)
								.map(String::from);
							let arguments_chunk = function
								.and_then(|f| f.get("arguments"))
								.and_then(Value::as_str)
								.unwrap_or("");

							// First chunk for a tool_call has id + name → ToolCallStart.
							if let (Some(id), Some(name)) = (id, name) {
								tool_call_index_to_id.insert(index, id.clone());
								let _ = tx
									.send(StreamChunk::ToolCallStart {
										id: id.clone(),
										name,
									})
									.await;
								if !arguments_chunk.is_empty() {
									let _ = tx
										.send(StreamChunk::ToolCallDelta {
											id,
											arguments_chunk: arguments_chunk.to_string(),
										})
										.await;
								}
							} else if !arguments_chunk.is_empty() {
								// Subsequent chunks use index to correlate.
								if let Some(id) = tool_call_index_to_id.get(&index) {
									let _ = tx
										.send(StreamChunk::ToolCallDelta {
											id: id.clone(),
											arguments_chunk: arguments_chunk.to_string(),
										})
										.await;
								}
							}
						}
					}
				}
			}
		}

		// Send ToolCallDone for all pending tool calls when stream ends.
		for id in tool_call_index_to_id.values() {
			let _ = tx.send(StreamChunk::ToolCallDone { id: id.clone() }).await;
		}

		let latency_ms = u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX);

		// Populate token counts from estimates when the provider did not send usage.
		if prompt_tokens == 0 {
			prompt_tokens = estimate_prompt_tokens(&full_text);
		}
		if output_tokens == 0 {
			output_tokens = estimate_prompt_tokens(&full_text);
		}

		// Done is always sent, even on error, so consumers can rely on it as
		// a stream termination signal.
		let _ = tx
			.send(StreamChunk::Done {
				finish_reason: finish_reason.clone(),
				prompt_tokens,
				output_tokens,
			})
			.await;

		// Propagate stream error after sending Done.
		if let Some(error) = stream_error {
			return Err(error);
		}

		log_openrouter(
			LogLevel::Info,
			"provider streaming request completed",
			[
				("provider", OPENROUTER_PROVIDER.to_string()),
				("requested_model", requested_model.to_string()),
				("status", "ok".to_string()),
				("latency_ms", latency_ms.to_string()),
				("prompt_tokens", prompt_tokens.to_string()),
				("output_tokens", output_tokens.to_string()),
				(
					"finish_reason",
					finish_reason.clone().unwrap_or_else(|| "none".to_string()),
				),
			],
		);

		Ok(ProviderResponse {
			output: full_text,
			finish_reason,
			prompt_tokens,
			output_tokens,
			latency_ms,
			tool_calls: None,
		})
	}
}

#[async_trait]
impl LlmProvider for OpenRouterProvider {
	fn provider_name(&self) -> &'static str {
		OPENROUTER_PROVIDER
	}

	async fn complete(
		&self,
		model: &ModelProfile,
		request: &GenerationRequest,
	) -> Result<ProviderResponse, ProviderCallError> {
		let attempt_models = attempt_model_sequence(&self.config, &model.model_id);
		let headers = build_headers(&self.config)?;

		let mut last_error = None;
		for (index, requested_model) in attempt_models.iter().enumerate() {
			let fallback_models = attempt_models
				.iter()
				.skip(index + 1)
				.cloned()
				.collect::<Vec<_>>();
			match self
				.complete_once(&headers, requested_model, &fallback_models, request)
				.await
			{
				Ok(response) => return Ok(response),
				Err(error) => {
					let has_next_candidate = index + 1 < attempt_models.len();
					if has_next_candidate && should_try_explicit_fallback(&error) {
						log_openrouter(
							LogLevel::Warn,
							"retrying with explicit fallback model after unreadable response",
							[
								("provider", OPENROUTER_PROVIDER.to_string()),
								("requested_model", requested_model.clone()),
								("next_model", attempt_models[index + 1].clone()),
								("reason", error.to_string()),
							],
						);
						last_error = Some(error);
						continue;
					}
					return Err(error);
				}
			}
		}

		Err(last_error.unwrap_or_else(|| {
			ProviderCallError::retryable("openrouter exhausted explicit model fallback attempts")
		}))
	}

	async fn stream(
		&self,
		model: &ModelProfile,
		request: &GenerationRequest,
		tx: tokio::sync::mpsc::Sender<StreamChunk>,
	) -> Result<ProviderResponse, ProviderCallError> {
		// Delegate to the existing inherent stream() method.
		OpenRouterProvider::stream(self, model, request, tx).await
	}
}

impl OpenRouterProvider {
	async fn complete_once(
		&self,
		headers: &HeaderMap,
		requested_model: &str,
		fallback_models: &[String],
		request: &GenerationRequest,
	) -> Result<ProviderResponse, ProviderCallError> {
		let body = build_request_body(requested_model, fallback_models, request);
		let started_at = Instant::now();
		let response = self
			.client
			.post(&self.config.base_url)
			.headers(headers.clone())
			.json(&body)
			.send()
			.await
			.map_err(classify_request_error)?;
		let status = response.status();
		let response_body = response.text().await.map_err(|error| {
			if error.is_timeout() || error.is_connect() {
				ProviderCallError::retryable(format!("failed to read response body: {error}"))
			} else {
				ProviderCallError::non_retryable(format!("failed to read response body: {error}"))
			}
		})?;
		if !status.is_success() {
			log_openrouter(
				LogLevel::Warn,
				"provider returned non-success status",
				[
					("provider", OPENROUTER_PROVIDER.to_string()),
					("model", requested_model.to_string()),
					("status", status.to_string()),
					("body", truncate_for_log(&response_body, 800)),
				],
			);
			return Err(classify_status_error(status.as_u16(), response_body));
		}

		let parsed = parse_response(&response_body)
			.inspect_err(|error| {
				log_openrouter(
					LogLevel::Warn,
					"failed to parse provider response",
					[
						("provider", OPENROUTER_PROVIDER.to_string()),
						("model", requested_model.to_string()),
						("parse_error", error.to_string()),
						("body", truncate_for_log(&response_body, 800)),
					],
				);
			})
			.map_err(classify_parse_error)?;
		let latency_ms = u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
		let served_model = parsed
			.served_model_id
			.as_deref()
			.unwrap_or(requested_model)
			.to_string();
		log_openrouter(
			LogLevel::Info,
			"provider request completed",
			[
				("provider", OPENROUTER_PROVIDER.to_string()),
				("requested_model", requested_model.to_string()),
				("served_model", served_model),
				("status", "ok".to_string()),
				("latency_ms", latency_ms.to_string()),
				("prompt_tokens", parsed.prompt_tokens.to_string()),
				("output_tokens", parsed.output_tokens.to_string()),
			],
		);
		Ok(ProviderResponse {
			output: parsed.output,
			finish_reason: parsed.finish_reason,
			prompt_tokens: parsed.prompt_tokens,
			output_tokens: parsed.output_tokens,
			latency_ms,
			tool_calls: parsed.tool_calls,
		})
	}
}

/// Build the shared HTTP headers for OpenRouter requests.
fn build_headers(config: &OpenRouterConfig) -> Result<HeaderMap, ProviderCallError> {
	let mut headers = HeaderMap::new();
	headers.insert(
		reqwest::header::AUTHORIZATION,
		HeaderValue::from_str(&format!("Bearer {}", config.api_key)).map_err(|error| {
			ProviderCallError::non_retryable(format!("invalid authorization header: {error}"))
		})?,
	);
	headers.insert(
		reqwest::header::CONTENT_TYPE,
		HeaderValue::from_static("application/json"),
	);
	if let Some(site_url) = &config.site_url {
		headers.insert(
			HeaderName::from_static("http-referer"),
			HeaderValue::from_str(site_url).map_err(|error| {
				ProviderCallError::non_retryable(format!("invalid HTTP-Referer header: {error}"))
			})?,
		);
	}
	if let Some(app_name) = &config.app_name {
		headers.insert(
			HeaderName::from_static("x-openrouter-title"),
			HeaderValue::from_str(app_name).map_err(|error| {
				ProviderCallError::non_retryable(format!(
					"invalid X-OpenRouter-Title header: {error}"
				))
			})?,
		);
	}
	Ok(headers)
}

fn classify_request_error(error: reqwest::Error) -> ProviderCallError {
	if error.is_timeout() || error.is_connect() {
		ProviderCallError::retryable(format!("request failed: {error}"))
	} else {
		ProviderCallError::non_retryable(format!("request failed: {error}"))
	}
}

fn classify_status_error(status_code: u16, response_body: String) -> ProviderCallError {
	let message = format!("openrouter returned status {status_code}: {response_body}");
	match status_code {
		408 | 409 | 429 | 500..=599 => ProviderCallError::retryable(message),
		_ => ProviderCallError::non_retryable(message),
	}
}

fn classify_parse_error(message: String) -> ProviderCallError {
	ProviderCallError::retryable(format!("unreadable provider response: {message}"))
}

fn should_try_explicit_fallback(error: &ProviderCallError) -> bool {
	match error {
		ProviderCallError::Retryable { message } => {
			message.contains("unreadable provider response")
				|| message.contains("provider_unreadable_content:")
				|| message.contains("provider_content_null:")
				|| message.contains("provider_finish_reason_length:")
				|| message.contains("no readable assistant content")
		}
		ProviderCallError::NonRetryable { .. } => false,
	}
}

pub fn build_openrouter_router(
	config: OpenRouterConfig,
) -> Result<LlmRouter, OpenRouterBootstrapError> {
	build_openrouter_router_with_metrics(config, Arc::new(Metrics::default()))
}

pub fn build_openrouter_router_with_metrics(
	config: OpenRouterConfig,
	metrics: Arc<Metrics>,
) -> Result<LlmRouter, OpenRouterBootstrapError> {
	let mut router = LlmRouter::new(RoutingPolicy {
		max_request_cost_usd: config.max_request_cost_usd,
		max_latency_ms: config.max_latency_ms,
	})
	.with_metrics(metrics);
	router.register_provider(OpenRouterProvider::new(config.clone())?);
	for (index, model_id) in attempt_model_sequence(&config, &config.primary_model)
		.into_iter()
		.enumerate()
	{
		router.register_model(ModelProfile {
			model_id,
			provider: OPENROUTER_PROVIDER.to_string(),
			max_context_tokens: config.max_context_tokens,
			cost_per_1k_tokens_usd: config.cost_per_1k_tokens_usd,
			max_risk_tier: RiskTier::Critical,
			route_priority: 100u8.saturating_sub(u8::try_from(index).unwrap_or(u8::MAX)),
		});
	}
	Ok(router)
}

fn build_request_body<'a>(
	model_id: &'a str,
	fallback_models: &'a [String],
	request: &'a GenerationRequest,
) -> OpenAiChatCompletionRequest<'a> {
	build_request_body_inner(model_id, fallback_models, request, false)
}

fn build_streaming_request_body<'a>(
	model_id: &'a str,
	fallback_models: &'a [String],
	request: &'a GenerationRequest,
) -> OpenAiChatCompletionRequest<'a> {
	build_request_body_inner(model_id, fallback_models, request, true)
}

fn message_to_openai_value(msg: &Message) -> Value {
	match msg {
		Message::User { content } => serde_json::json!({ "role": "user", "content": content }),
		Message::Assistant { text, tool_calls } => {
			if tool_calls.is_empty() {
				serde_json::json!({ "role": "assistant", "content": text })
			} else {
				let calls: Vec<Value> = tool_calls
					.iter()
					.map(|tc| {
						serde_json::json!({
							"id": tc.id,
							"type": "function",
							"function": {
								"name": tc.name,
								"arguments": tc.arguments.to_string(),
							}
						})
					})
					.collect();
				serde_json::json!({
					"role": "assistant",
					"content": text,
					"tool_calls": calls,
				})
			}
		}
		Message::ToolResult {
			tool_use_id,
			content,
			..
		} => {
			serde_json::json!({
				"role": "tool",
				"tool_call_id": tool_use_id,
				"content": content,
			})
		}
	}
}

fn build_request_body_inner<'a>(
	model_id: &'a str,
	fallback_models: &'a [String],
	request: &'a GenerationRequest,
	stream: bool,
) -> OpenAiChatCompletionRequest<'a> {
	let mut messages: Vec<Value> = Vec::with_capacity(2);
	if let Some(system_prompt) = request.system_prompt.as_deref() {
		messages.push(serde_json::json!({ "role": "system", "content": system_prompt }));
	}

	if let Some(turns) = request.messages.as_deref() {
		for msg in turns {
			messages.push(message_to_openai_value(msg));
		}
	} else {
		messages.push(serde_json::json!({ "role": "user", "content": &request.prompt }));
	}

	let tools = request.tools.as_ref().map(|tool_defs| {
		tool_defs
			.iter()
			.map(|tool| OpenAiToolDefinition {
				r#type: "function",
				function: OpenAiFunctionDefinition {
					name: tool.name.clone(),
					description: tool.description.clone(),
					parameters: tool.parameters.clone(),
				},
			})
			.collect::<Vec<_>>()
	});

	OpenAiChatCompletionRequest {
		model: model_id,
		models: fallback_models.to_vec(),
		messages,
		max_tokens: request.expected_output_tokens,
		reasoning: reasoning_config_for_model(model_id, request.thinking_effort),
		stream,
		tools: tools.filter(|t| !t.is_empty()),
	}
}

#[derive(Debug)]
struct ParsedOpenRouterResponse {
	output: String,
	finish_reason: Option<String>,
	prompt_tokens: u64,
	output_tokens: u64,
	served_model_id: Option<String>,
	tool_calls: Option<Vec<ToolCallBlock>>,
}

fn parse_response(response_body: &str) -> Result<ParsedOpenRouterResponse, String> {
	let response: Value = serde_json::from_str(response_body)
		.map_err(|error| format!("invalid response json: {error}"))?;
	let choice = response
		.get("choices")
		.and_then(Value::as_array)
		.and_then(|choices| choices.first())
		.ok_or_else(|| "openrouter response contained no choice payload".to_string())?;
	let finish_reason = choice
		.get("finish_reason")
		.and_then(Value::as_str)
		.map(str::to_string);
	if finish_reason.as_deref() == Some("length") {
		return Err(
			"provider_finish_reason_length: openrouter response was truncated by finish_reason=length"
				.to_string(),
		);
	}

	// Parse tool_calls from the message before deciding how to handle content.
	let tool_calls = parse_tool_calls_from_message(choice);
	let has_tool_calls = tool_calls.as_ref().is_some_and(|tc| !tc.is_empty());

	// When content is null and no tool_calls are present, treat as an error.
	// When tool_calls are present, null content is expected — use empty string.
	if choice
		.get("message")
		.and_then(Value::as_object)
		.and_then(|message| message.get("content"))
		.is_some_and(Value::is_null)
		&& !has_tool_calls
	{
		return Err("provider_content_null: openrouter response content is null".to_string());
	}

	let output = if has_tool_calls {
		// Content may be null or absent when tool_calls are present — that is fine.
		choice
			.get("message")
			.and_then(extract_message_text)
			.or_else(|| {
				choice
					.get("text")
					.and_then(Value::as_str)
					.map(str::to_string)
			})
			.unwrap_or_default()
	} else {
		choice
			.get("message")
			.and_then(extract_message_text)
			.or_else(|| {
				choice
					.get("text")
					.and_then(Value::as_str)
					.map(str::to_string)
			})
			.ok_or_else(|| {
				format!(
					"provider_unreadable_content: openrouter response contained no readable assistant content: {}",
					truncate_for_log(&choice.to_string(), 400)
				)
			})?
	};

	let prompt_tokens = response
		.get("usage")
		.and_then(|usage| usage.get("prompt_tokens"))
		.and_then(Value::as_u64)
		.unwrap_or_else(|| estimate_prompt_tokens(&output));
	let output_tokens = response
		.get("usage")
		.and_then(|usage| usage.get("completion_tokens"))
		.and_then(Value::as_u64)
		.unwrap_or_else(|| estimate_prompt_tokens(&output));
	let served_model_id = response
		.get("model")
		.and_then(Value::as_str)
		.map(str::to_string);

	Ok(ParsedOpenRouterResponse {
		output,
		finish_reason,
		prompt_tokens,
		output_tokens,
		served_model_id,
		tool_calls,
	})
}

/// Parse the `tool_calls` array from a response message value.
///
/// OpenAI format:
/// `[{"id": "call_xxx", "type": "function", "function": {"name": "...", "arguments": "{...}"}}]`
///
/// Returns `None` when no tool_calls are present or the array is empty.
fn parse_tool_calls_from_message(choice: &Value) -> Option<Vec<ToolCallBlock>> {
	let tool_calls_json = choice
		.get("message")
		.and_then(Value::as_object)
		.and_then(|message| message.get("tool_calls"))
		.and_then(Value::as_array)?;

	let parsed: Vec<ToolCallBlock> = tool_calls_json
		.iter()
		.filter_map(|entry| {
			let id = entry.get("id").and_then(Value::as_str)?.to_string();
			let function = entry.get("function").and_then(Value::as_object)?;
			let name = function.get("name").and_then(Value::as_str)?.to_string();
			// `arguments` is a JSON-encoded string; parse it back into a Value.
			let arguments = function
				.get("arguments")
				.and_then(Value::as_str)
				.and_then(|s| serde_json::from_str::<Value>(s).ok())
				.unwrap_or(Value::Null);
			Some(ToolCallBlock {
				id,
				name,
				arguments,
			})
		})
		.collect();

	if parsed.is_empty() {
		None
	} else {
		Some(parsed)
	}
}

#[derive(Debug, Serialize)]
struct OpenAiChatCompletionRequest<'a> {
	model: &'a str,
	#[serde(skip_serializing_if = "Vec::is_empty")]
	models: Vec<String>,
	messages: Vec<Value>,
	max_tokens: u64,
	reasoning: OpenRouterReasoningConfig,
	/// When `true` the provider returns a server-sent event stream.
	#[serde(skip_serializing_if = "std::ops::Not::not")]
	stream: bool,
	/// Tool definitions for native function calling. Omitted when empty.
	#[serde(skip_serializing_if = "Option::is_none")]
	tools: Option<Vec<OpenAiToolDefinition>>,
}

/// OpenAI-format tool definition wrapper sent in the request `tools` array.
#[derive(Debug, Serialize)]
struct OpenAiToolDefinition {
	r#type: &'static str,
	function: OpenAiFunctionDefinition,
}

/// The inner function definition inside an [`OpenAiToolDefinition`].
#[derive(Debug, Serialize)]
struct OpenAiFunctionDefinition {
	name: String,
	description: String,
	parameters: Value,
}

#[derive(Debug, Serialize)]
struct OpenRouterReasoningConfig {
	exclude: bool,
	#[serde(skip_serializing_if = "Option::is_none")]
	effort: Option<&'static str>,
}

fn reasoning_config_for_model(
	model_id: &str,
	thinking_effort: Option<ThinkingEffort>,
) -> OpenRouterReasoningConfig {
	// stepfun models do not support the effort override parameter.
	let is_stepfun = model_id.starts_with("stepfun/step-3.5-flash");

	match thinking_effort {
		Some(ThinkingEffort::Low) => OpenRouterReasoningConfig {
			exclude: false,
			effort: if is_stepfun { None } else { Some("low") },
		},
		Some(ThinkingEffort::Medium) => OpenRouterReasoningConfig {
			exclude: false,
			effort: if is_stepfun { None } else { Some("medium") },
		},
		Some(ThinkingEffort::High) => OpenRouterReasoningConfig {
			exclude: false,
			effort: if is_stepfun { None } else { Some("high") },
		},
		None | Some(ThinkingEffort::None) => OpenRouterReasoningConfig {
			exclude: true,
			effort: if is_stepfun { None } else { Some("none") },
		},
	}
}

fn attempt_model_sequence(config: &OpenRouterConfig, selected_model: &str) -> Vec<String> {
	let mut attempts = Vec::new();
	attempts.push(selected_model.to_string());
	attempts.extend(config.request_fallback_chain(selected_model));
	dedupe_preserving_order(attempts)
}

fn dedupe_preserving_order(models: Vec<String>) -> Vec<String> {
	let mut deduped = Vec::new();
	for model in models {
		if deduped.iter().any(|existing| existing == &model) {
			continue;
		}
		deduped.push(model);
	}
	deduped
}

fn extract_message_text(message: &Value) -> Option<String> {
	match message {
		Value::Object(object) => object
			.get("content")
			.and_then(extract_content_text)
			.or_else(|| {
				object
					.get("refusal")
					.and_then(Value::as_str)
					.map(str::to_string)
			})
			.or_else(|| {
				object
					.get("text")
					.and_then(Value::as_str)
					.map(str::to_string)
			}),
		other => extract_content_text(other),
	}
}

fn extract_content_text(content: &Value) -> Option<String> {
	match content {
		Value::String(text) => Some(text.clone()),
		Value::Array(parts) => {
			let text = parts
				.iter()
				.filter_map(extract_content_text)
				.collect::<Vec<_>>()
				.join("");
			if text.trim().is_empty() {
				None
			} else {
				Some(text)
			}
		}
		Value::Object(object) => object
			.get("text")
			.and_then(Value::as_str)
			.map(str::to_string)
			.or_else(|| object.get("content").and_then(extract_content_text))
			.or_else(|| {
				object
					.get("refusal")
					.and_then(Value::as_str)
					.map(str::to_string)
			}),
		_ => None,
	}
}

fn truncate_for_log(value: &str, max_chars: usize) -> String {
	let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
	let mut chars = normalized.chars();
	let truncated = chars.by_ref().take(max_chars).collect::<String>();
	if chars.next().is_some() {
		format!("{truncated}...")
	} else {
		truncated
	}
}

fn log_openrouter(
	level: LogLevel,
	message: &str,
	fields: impl IntoIterator<Item = (&'static str, String)>,
) {
	let record = fields.into_iter().fold(
		LogRecord::new("roku-plugin-llm", level, message),
		|record, (key, value)| record.with_field(key, value),
	);
	let _ = emit_global_log(record);
}

fn default_fallback_models() -> Vec<String> {
	DEFAULT_OPENROUTER_FALLBACK_MODELS
		.iter()
		.map(|model| normalize_model_id(model))
		.collect()
}

fn parse_model_list(value: &str) -> Vec<String> {
	value
		.split(',')
		.map(str::trim)
		.filter(|model| !model.is_empty())
		.map(normalize_model_id)
		.collect()
}

fn dedupe_model_chain(primary_model: &str, candidates: Vec<String>) -> Vec<String> {
	let mut deduped = Vec::new();
	for candidate in candidates {
		if candidate == primary_model || deduped.iter().any(|model| model == &candidate) {
			continue;
		}
		deduped.push(candidate);
	}
	deduped
}

fn normalize_model_id(model_id: &str) -> String {
	match model_id.trim() {
		"step-3.5-flash" => "stepfun/step-3.5-flash".to_string(),
		"step-3.5-flash:free" => "stepfun/step-3.5-flash:free".to_string(),
		"deepseek-chat" => "deepseek/deepseek-chat".to_string(),
		"gemini-2.0-flash" => "google/gemini-2.0-flash-001".to_string(),
		other => other.to_string(),
	}
}

fn env_override_string(key: &str) -> Option<String> {
	env::var(key)
		.ok()
		.map(|value| value.trim().to_string())
		.filter(|value| !value.is_empty())
}

fn env_var_required(key: &'static str) -> Result<String, OpenRouterBootstrapError> {
	let value = env::var(key).map_err(|_| OpenRouterBootstrapError::MissingEnv(key))?;
	if value.trim().is_empty() {
		return Err(OpenRouterBootstrapError::MissingEnv(key));
	}
	Ok(value)
}

fn env_var_u64(key: &'static str) -> Result<Option<u64>, OpenRouterBootstrapError> {
	match env::var(key) {
		Ok(value) if !value.trim().is_empty() => {
			value
				.parse::<u64>()
				.map(Some)
				.map_err(|error| OpenRouterBootstrapError::InvalidEnv {
					key,
					message: error.to_string(),
				})
		}
		Ok(_) | Err(env::VarError::NotPresent) => Ok(None),
		Err(error) => Err(OpenRouterBootstrapError::InvalidEnv {
			key,
			message: error.to_string(),
		}),
	}
}

fn env_override_u64(key: &'static str) -> Result<Option<u64>, OpenRouterBootstrapError> {
	match env_override_string(key) {
		Some(value) => {
			value
				.parse::<u64>()
				.map(Some)
				.map_err(|error| OpenRouterBootstrapError::InvalidEnv {
					key,
					message: error.to_string(),
				})
		}
		None => Ok(None),
	}
}

fn env_var_f64(key: &'static str) -> Result<Option<f64>, OpenRouterBootstrapError> {
	match env::var(key) {
		Ok(value) if !value.trim().is_empty() => {
			value
				.parse::<f64>()
				.map(Some)
				.map_err(|error| OpenRouterBootstrapError::InvalidEnv {
					key,
					message: error.to_string(),
				})
		}
		Ok(_) | Err(env::VarError::NotPresent) => Ok(None),
		Err(error) => Err(OpenRouterBootstrapError::InvalidEnv {
			key,
			message: error.to_string(),
		}),
	}
}

fn env_override_f64(key: &'static str) -> Result<Option<f64>, OpenRouterBootstrapError> {
	match env_override_string(key) {
		Some(value) => {
			value
				.parse::<f64>()
				.map(Some)
				.map_err(|error| OpenRouterBootstrapError::InvalidEnv {
					key,
					message: error.to_string(),
				})
		}
		None => Ok(None),
	}
}

#[cfg(test)]
mod tests {
	use serde_json::json;

	use crate::types::{GenerationRequest, RiskTier};

	use super::*;

	fn sample_request() -> GenerationRequest {
		GenerationRequest {
			system_prompt: None,
			prompt: "reply with a short greeting".to_string(),
			messages: None,
			expected_output_tokens: 64,
			risk_tier: RiskTier::Low,
			preferred_provider: None,
			budget_tokens_remaining: 1024,
			budget_cost_remaining_usd: 0.1,
			tools: None,
			model_override: None,
			thinking_effort: None,
		}
	}

	#[test]
	fn request_body_uses_primary_model_and_fallback_chain_by_default() {
		let config = OpenRouterConfig {
			api_key: "test-key".to_string(),
			primary_model: normalize_model_id(DEFAULT_OPENROUTER_PRIMARY_MODEL),
			fallback_models: default_fallback_models(),
			app_name: None,
			site_url: None,
			base_url: DEFAULT_OPENROUTER_URL.to_string(),
			max_context_tokens: 128_000,
			cost_per_1k_tokens_usd: 0.0,
			max_request_cost_usd: 1.0,
			max_latency_ms: 60_000,
		};
		let body = serde_json::to_value(build_request_body(
			&config.primary_model,
			&config.fallback_models,
			&sample_request(),
		))
		.expect("request body should serialize");

		assert_eq!(body["model"], "deepseek/deepseek-chat");
		assert_eq!(body["models"], json!(vec!["google/gemini-2.0-flash-001"]),);
		assert_eq!(body["messages"][0]["role"], "user");
	}

	#[test]
	fn request_body_includes_system_message_when_present() {
		let config = OpenRouterConfig {
			api_key: "test-key".to_string(),
			primary_model: normalize_model_id(DEFAULT_OPENROUTER_PRIMARY_MODEL),
			fallback_models: default_fallback_models(),
			app_name: None,
			site_url: None,
			base_url: DEFAULT_OPENROUTER_URL.to_string(),
			max_context_tokens: 128_000,
			cost_per_1k_tokens_usd: 0.0,
			max_request_cost_usd: 1.0,
			max_latency_ms: 60_000,
		};
		let body = serde_json::to_value(build_request_body(
			&config.primary_model,
			&config.fallback_models,
			&GenerationRequest {
				system_prompt: Some("You are Roku.".to_string()),
				..sample_request()
			},
		))
		.expect("request body should serialize");

		assert_eq!(body["messages"][0]["role"], "system");
		assert_eq!(body["messages"][0]["content"], "You are Roku.");
		assert_eq!(body["messages"][1]["role"], "user");
		assert_eq!(body["reasoning"]["exclude"], true);
		assert_eq!(body["reasoning"]["effort"], "none");
	}

	#[test]
	fn parse_response_reads_string_content() {
		let parsed = parse_response(
			r#"{
				"choices":[{"message":{"content":"hello from openrouter"}}],
				"usage":{"prompt_tokens":10,"completion_tokens":4}
			}"#,
		)
		.expect("response should parse");

		assert_eq!(parsed.output, "hello from openrouter");
		assert_eq!(parsed.prompt_tokens, 10);
		assert_eq!(parsed.output_tokens, 4);
	}

	#[test]
	fn parse_response_reads_text_parts() {
		let parsed = parse_response(
			r#"{
				"choices":[{"message":{"content":[{"text":"hello "},{"text":"world"}]}}],
				"usage":{"prompt_tokens":12,"completion_tokens":6}
			}"#,
		)
		.expect("response should parse");

		assert_eq!(parsed.output, "hello world");
		assert_eq!(parsed.prompt_tokens, 12);
		assert_eq!(parsed.output_tokens, 6);
	}

	#[test]
	fn parse_response_reads_object_content_payload() {
		let parsed = parse_response(
			r#"{
				"choices":[{"message":{"content":{"type":"text","text":"hello from object payload"}}}],
				"usage":{"prompt_tokens":14,"completion_tokens":5}
			}"#,
		)
		.expect("object-shaped content should parse");

		assert_eq!(parsed.output, "hello from object payload");
		assert_eq!(parsed.prompt_tokens, 14);
		assert_eq!(parsed.output_tokens, 5);
	}

	#[test]
	fn parse_response_rejects_reasoning_only_payloads() {
		let error = parse_response(
			r#"{
				"model":"deepseek/deepseek-chat",
				"choices":[{"message":{"role":"assistant","content":null,"reasoning":"hello from reasoning"}}],
				"usage":{"prompt_tokens":9,"completion_tokens":3}
			}"#,
		)
		.expect_err("reasoning-only payloads must not be surfaced as assistant output");

		assert!(error.contains("provider_content_null"));
	}

	#[test]
	fn truncate_for_log_compacts_blank_lines_and_whitespace() {
		assert_eq!(
			truncate_for_log("line one\n\n\n   line two\t\tline three", 80),
			"line one line two line three"
		);
	}

	#[test]
	fn normalize_model_id_maps_supported_aliases() {
		assert_eq!(
			normalize_model_id("step-3.5-flash:free"),
			"stepfun/step-3.5-flash:free",
		);
		assert_eq!(
			normalize_model_id("deepseek-chat"),
			"deepseek/deepseek-chat"
		);
		assert_eq!(
			normalize_model_id("gemini-2.0-flash"),
			"google/gemini-2.0-flash-001",
		);
	}

	#[test]
	fn attempt_model_sequence_starts_with_selected_model_and_preserves_fallback_order() {
		let config = OpenRouterConfig {
			api_key: "test-key".to_string(),
			primary_model: normalize_model_id(DEFAULT_OPENROUTER_PRIMARY_MODEL),
			fallback_models: default_fallback_models(),
			app_name: None,
			site_url: None,
			base_url: DEFAULT_OPENROUTER_URL.to_string(),
			max_context_tokens: 128_000,
			cost_per_1k_tokens_usd: 0.0,
			max_request_cost_usd: 1.0,
			max_latency_ms: 60_000,
		};

		assert_eq!(
			attempt_model_sequence(&config, &config.primary_model),
			vec![
				"deepseek/deepseek-chat".to_string(),
				"google/gemini-2.0-flash-001".to_string(),
			],
		);
		assert_eq!(
			attempt_model_sequence(&config, "deepseek/deepseek-chat"),
			vec![
				"deepseek/deepseek-chat".to_string(),
				"google/gemini-2.0-flash-001".to_string(),
			],
		);
	}

	#[test]
	fn reasoning_config_disables_effort_override_for_stepfun_models_only() {
		let stepfun_config = serde_json::to_value(reasoning_config_for_model(
			"stepfun/step-3.5-flash:free",
			None,
		))
		.expect("stepfun reasoning config should serialize");
		assert_eq!(stepfun_config["exclude"], true);
		assert!(stepfun_config.get("effort").is_none());

		let deepseek_config =
			serde_json::to_value(reasoning_config_for_model("deepseek/deepseek-chat", None))
				.expect("deepseek reasoning config should serialize");
		assert_eq!(deepseek_config["exclude"], true);
		assert_eq!(deepseek_config["effort"], "none");
	}
}
