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

//! Direct Anthropic Messages API provider with native tool_use support.
//!
//! Implements [`LlmProvider`] for the Anthropic Messages API, supporting both
//! non-streaming (complete) and streaming (SSE) request modes. Streaming
//! responses emit `StreamChunk::ToolCallStart/Delta/Done` when the model
//! invokes tools via the native `tool_use` protocol.

use std::collections::HashMap;
use std::env;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use reqwest::Client;
use reqwest::header::{HeaderMap, HeaderValue};
use roku_common_types::{LogLevel, LogRecord, Metrics, emit_global_log};
use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;
use tokio::sync::mpsc;

use crate::router::{LlmProvider, LlmRouter};
use crate::types::{
	GenerationRequest, Message, ModelProfile, ProviderCallError, ProviderResponse, RiskTier,
	RoutingPolicy, StreamChunk, ThinkingEffort, ToolCallBlock, estimate_prompt_tokens,
};

const ANTHROPIC_PROVIDER: &str = "anthropic";
const DEFAULT_ANTHROPIC_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const DEFAULT_MAX_TOKENS: u64 = 8192;
const DEFAULT_ANTHROPIC_PRIMARY_MODEL: &str = "claude-sonnet-4-5-20250929";
const DEFAULT_ANTHROPIC_FALLBACK_MODEL: &str = "claude-3-5-haiku-20241022";
const DEFAULT_ANTHROPIC_MAX_CONTEXT_TOKENS: u64 = 200_000;
const HARD_MAX_CONTEXT_TOKENS: u64 = 1_000_000;
const HARD_MAX_REQUEST_COST_USD: f64 = 100.0;
const HARD_MAX_LATENCY_MS: u64 = 300_000;

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// Effective non-secret Anthropic runtime configuration.
///
/// Mirrors the shape of [`OpenRouterRuntimeConfig`] so the startup layer can
/// treat every provider uniformly: parse a patch from `runtime.toml`,
/// overlay environment overrides, validate and clamp. Secrets (API key)
/// stay out of this struct — they are attached separately via
/// [`AnthropicRuntimeConfig::with_api_key`].
#[derive(Debug, Clone, PartialEq)]
pub struct AnthropicRuntimeConfig {
	pub primary_model: String,
	pub fallback_models: Vec<String>,
	pub base_url: String,
	pub max_tokens: u64,
	pub max_context_tokens: u64,
	pub cost_per_1k_tokens_usd: f64,
	pub max_request_cost_usd: f64,
	pub max_latency_ms: u64,
}

/// Partial overrides for [`AnthropicRuntimeConfig`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnthropicRuntimeConfigPatch {
	pub primary_model: Option<String>,
	pub fallback_models: Option<Vec<String>>,
	pub base_url: Option<String>,
	pub max_tokens: Option<u64>,
	pub max_context_tokens: Option<u64>,
	pub cost_per_1k_tokens_usd: Option<f64>,
	pub max_request_cost_usd: Option<f64>,
	pub max_latency_ms: Option<u64>,
}

impl Default for AnthropicRuntimeConfig {
	fn default() -> Self {
		Self {
			primary_model: DEFAULT_ANTHROPIC_PRIMARY_MODEL.to_string(),
			fallback_models: vec![DEFAULT_ANTHROPIC_FALLBACK_MODEL.to_string()],
			base_url: DEFAULT_ANTHROPIC_URL.to_string(),
			max_tokens: DEFAULT_MAX_TOKENS,
			max_context_tokens: DEFAULT_ANTHROPIC_MAX_CONTEXT_TOKENS,
			cost_per_1k_tokens_usd: 0.0,
			max_request_cost_usd: 1.0,
			max_latency_ms: 60_000,
		}
	}
}

impl AnthropicRuntimeConfig {
	pub fn apply_patch(&mut self, patch: AnthropicRuntimeConfigPatch) {
		if let Some(value) = patch.primary_model {
			self.primary_model = value;
		}
		if let Some(value) = patch.fallback_models {
			self.fallback_models = value;
		}
		if let Some(value) = patch.base_url {
			self.base_url = value;
		}
		if let Some(value) = patch.max_tokens {
			self.max_tokens = value;
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

	pub fn apply_env_overrides(&mut self) -> Result<(), AnthropicBootstrapError> {
		if let Some(value) = env_override_string("ROKU_ANTHROPIC_PRIMARY_MODEL") {
			self.primary_model = value;
		}
		if let Some(value) = env_override_string("ROKU_ANTHROPIC_BASE_URL") {
			self.base_url = value;
		}
		if let Some(value) = env_var_u64("ROKU_ANTHROPIC_MAX_TOKENS")? {
			self.max_tokens = value;
		}
		Ok(())
	}

	pub fn validate_and_clamp(&mut self) -> Result<(), AnthropicBootstrapError> {
		self.primary_model = self.primary_model.trim().to_string();
		self.base_url = self.base_url.trim().to_string();
		if self.primary_model.is_empty() {
			return Err(AnthropicBootstrapError::InvalidEnv {
				key: "ROKU_ANTHROPIC_PRIMARY_MODEL",
				message: "value cannot be empty".to_string(),
			});
		}
		if self.base_url.is_empty() {
			return Err(AnthropicBootstrapError::InvalidEnv {
				key: "ROKU_ANTHROPIC_BASE_URL",
				message: "value cannot be empty".to_string(),
			});
		}
		if self.max_tokens == 0 {
			return Err(AnthropicBootstrapError::InvalidEnv {
				key: "ROKU_ANTHROPIC_MAX_TOKENS",
				message: "value must be greater than zero".to_string(),
			});
		}
		if self.max_context_tokens == 0 {
			return Err(AnthropicBootstrapError::InvalidEnv {
				key: "max_context_tokens",
				message: "value must be greater than zero".to_string(),
			});
		}
		if self.cost_per_1k_tokens_usd < 0.0 {
			return Err(AnthropicBootstrapError::InvalidEnv {
				key: "cost_per_1k_tokens_usd",
				message: "value must be non-negative".to_string(),
			});
		}
		if self.max_request_cost_usd <= 0.0 {
			return Err(AnthropicBootstrapError::InvalidEnv {
				key: "max_request_cost_usd",
				message: "value must be greater than zero".to_string(),
			});
		}
		if self.max_latency_ms == 0 {
			return Err(AnthropicBootstrapError::InvalidEnv {
				key: "max_latency_ms",
				message: "value must be greater than zero".to_string(),
			});
		}
		self.max_context_tokens = self.max_context_tokens.min(HARD_MAX_CONTEXT_TOKENS);
		self.max_request_cost_usd = self.max_request_cost_usd.min(HARD_MAX_REQUEST_COST_USD);
		self.max_latency_ms = self.max_latency_ms.min(HARD_MAX_LATENCY_MS);
		// Drop any fallback that duplicates the primary.
		self.fallback_models
			.retain(|model| model != &self.primary_model);
		Ok(())
	}

	/// Attach a resolved API key to produce the full [`AnthropicConfig`] the
	/// provider uses at request time.
	pub fn with_api_key(self, api_key: String) -> AnthropicConfig {
		AnthropicConfig {
			api_key,
			base_url: self.base_url,
			max_tokens: self.max_tokens,
		}
	}
}

/// Full per-request Anthropic configuration, including the resolved API
/// key. Constructed via [`AnthropicRuntimeConfig::with_api_key`] at
/// bootstrap time or [`AnthropicConfig::from_env`] for ad-hoc use.
#[derive(Debug, Clone, PartialEq)]
pub struct AnthropicConfig {
	pub api_key: String,
	pub base_url: String,
	pub max_tokens: u64,
}

impl AnthropicConfig {
	pub fn from_env() -> Result<Self, ProviderCallError> {
		let api_key = env::var("ROKU_ANTHROPIC_API_KEY").map_err(|_| ProviderCallError::Fatal {
			message: "ROKU_ANTHROPIC_API_KEY environment variable is not set".to_string(),
		})?;
		let base_url = env::var("ROKU_ANTHROPIC_BASE_URL")
			.unwrap_or_else(|_| DEFAULT_ANTHROPIC_URL.to_string());
		Ok(Self {
			api_key,
			base_url,
			max_tokens: DEFAULT_MAX_TOKENS,
		})
	}
}

#[derive(Debug, Error)]
pub enum AnthropicBootstrapError {
	#[error("missing required environment variable: {0}")]
	MissingEnv(&'static str),
	#[error("invalid environment variable {key}: {message}")]
	InvalidEnv { key: &'static str, message: String },
	#[error("failed to construct anthropic http client: {0}")]
	HttpClient(#[from] reqwest::Error),
	#[error("anthropic provider bootstrap failed: {0}")]
	Provider(String),
}

impl From<ProviderCallError> for AnthropicBootstrapError {
	fn from(error: ProviderCallError) -> Self {
		AnthropicBootstrapError::Provider(error.to_string())
	}
}

fn env_override_string(key: &str) -> Option<String> {
	std::env::var(key)
		.ok()
		.map(|value| value.trim().to_string())
		.filter(|value| !value.is_empty())
}

fn env_var_u64(key: &'static str) -> Result<Option<u64>, AnthropicBootstrapError> {
	let Some(raw) = env_override_string(key) else {
		return Ok(None);
	};
	raw.parse::<u64>()
		.map(Some)
		.map_err(|error| AnthropicBootstrapError::InvalidEnv {
			key,
			message: error.to_string(),
		})
}

/// Read the Anthropic API key from the environment.
///
/// Kept separate from [`AnthropicRuntimeConfig`] so future credential
/// sources (OAuth, TUI picker, keyring) can be added without reshaping the
/// runtime config surface.
pub fn anthropic_api_key_from_env() -> Result<String, AnthropicBootstrapError> {
	std::env::var("ROKU_ANTHROPIC_API_KEY")
		.ok()
		.map(|value| value.trim().to_string())
		.filter(|value| !value.is_empty())
		.ok_or(AnthropicBootstrapError::MissingEnv(
			"ROKU_ANTHROPIC_API_KEY",
		))
}

/// Construct a live [`LlmRouter`] backed by Anthropic.
///
/// Registers the provider plus a prioritized sequence of
/// [`ModelProfile`] entries built from
/// [`AnthropicRuntimeConfig::primary_model`] and its fallback chain.
pub fn build_anthropic_router_with_metrics(
	config: AnthropicConfig,
	runtime_config: AnthropicRuntimeConfig,
	metrics: Arc<Metrics>,
) -> Result<LlmRouter, AnthropicBootstrapError> {
	let mut router = LlmRouter::new(RoutingPolicy {
		max_request_cost_usd: runtime_config.max_request_cost_usd,
		max_latency_ms: runtime_config.max_latency_ms,
	})
	.with_metrics(metrics);
	router.register_provider(AnthropicProvider::new(config)?);

	let mut model_chain =
		Vec::with_capacity(runtime_config.fallback_models.len().saturating_add(1));
	model_chain.push(runtime_config.primary_model.clone());
	model_chain.extend(runtime_config.fallback_models.iter().cloned());

	for (index, model_id) in model_chain.into_iter().enumerate() {
		router.register_model(ModelProfile {
			model_id,
			provider: ANTHROPIC_PROVIDER.to_string(),
			max_context_tokens: runtime_config.max_context_tokens,
			cost_per_1k_tokens_usd: runtime_config.cost_per_1k_tokens_usd,
			max_risk_tier: RiskTier::Critical,
			route_priority: 100u8.saturating_sub(u8::try_from(index).unwrap_or(u8::MAX)),
		});
	}
	Ok(router)
}

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

pub struct AnthropicProvider {
	client: Client,
	config: AnthropicConfig,
}

impl AnthropicProvider {
	pub fn new(config: AnthropicConfig) -> Result<Self, ProviderCallError> {
		let client = Client::builder()
			.connect_timeout(std::time::Duration::from_secs(30))
			.build()
			.map_err(|e| ProviderCallError::Fatal {
				message: format!("http client error: {e}"),
			})?;
		Ok(Self { client, config })
	}

	fn build_headers(&self) -> Result<HeaderMap, ProviderCallError> {
		let mut headers = HeaderMap::new();
		headers.insert(
			"x-api-key",
			HeaderValue::from_str(&self.config.api_key).map_err(|e| ProviderCallError::Fatal {
				message: format!("invalid api key header: {e}"),
			})?,
		);
		headers.insert(
			"anthropic-version",
			HeaderValue::from_static(ANTHROPIC_VERSION),
		);
		headers.insert(
			reqwest::header::CONTENT_TYPE,
			HeaderValue::from_static("application/json"),
		);
		Ok(headers)
	}

	fn build_request_body(
		&self,
		model_id: &str,
		request: &GenerationRequest,
		stream: bool,
	) -> Value {
		let max_tokens = if request.expected_output_tokens > 0 {
			request.expected_output_tokens
		} else {
			self.config.max_tokens
		};

		let messages: Vec<Value> = if let Some(msgs) = &request.messages {
			msgs.iter()
				.map(|msg| match msg {
					Message::User { content } => {
						serde_json::json!({"role": "user", "content": content})
					}
					Message::Assistant { text, tool_calls } => {
						if tool_calls.is_empty() {
							serde_json::json!({"role": "assistant", "content": text})
						} else {
							let mut content_blocks: Vec<Value> = Vec::new();
							if !text.is_empty() {
								content_blocks
									.push(serde_json::json!({"type": "text", "text": text}));
							}
							for tc in tool_calls {
								content_blocks.push(serde_json::json!({
									"type": "tool_use",
									"id": tc.id,
									"name": tc.name,
									"input": tc.arguments,
								}));
							}
							serde_json::json!({"role": "assistant", "content": content_blocks})
						}
					}
					Message::ToolResult {
						tool_use_id,
						content,
						is_error,
					} => {
						serde_json::json!({
							"role": "user",
							"content": [{
								"type": "tool_result",
								"tool_use_id": tool_use_id,
								"content": content,
								"is_error": is_error,
							}],
						})
					}
				})
				.collect()
		} else {
			vec![serde_json::json!({"role": "user", "content": request.prompt})]
		};

		let mut body = serde_json::json!({
			"model": model_id,
			"max_tokens": max_tokens,
			"messages": messages,
		});

		if let Some(system_prompt) = &request.system_prompt {
			body["system"] = Value::String(system_prompt.clone());
		}

		if stream {
			body["stream"] = Value::Bool(true);
		}

		if let Some(tools) = &request.tools {
			let tool_defs: Vec<Value> = tools
				.iter()
				.map(|tool| {
					serde_json::json!({
						"name": tool.name,
						"description": tool.description,
						"input_schema": tool.parameters,
					})
				})
				.collect();
			if !tool_defs.is_empty() {
				body["tools"] = Value::Array(tool_defs);
			}
		}

		if let Some(effort) = request.thinking_effort {
			let budget_tokens: u64 = match effort {
				ThinkingEffort::Low => 1024,
				ThinkingEffort::Medium => 4096,
				ThinkingEffort::High => 16384,
				ThinkingEffort::None => 0,
			};
			if budget_tokens > 0 {
				// Anthropic requires max_tokens >= budget_tokens (max_tokens is
				// the total budget including thinking output). Bump it if needed.
				let current_max = body["max_tokens"].as_u64().unwrap_or(max_tokens);
				if current_max < budget_tokens + 1024 {
					body["max_tokens"] = Value::from(budget_tokens + 1024);
				}
				body["thinking"] = serde_json::json!({
					"type": "enabled",
					"budget_tokens": budget_tokens,
				});
			}
		}

		body
	}
}

#[async_trait]
impl LlmProvider for AnthropicProvider {
	fn provider_name(&self) -> &'static str {
		ANTHROPIC_PROVIDER
	}

	async fn complete(
		&self,
		model: &ModelProfile,
		request: &GenerationRequest,
	) -> Result<ProviderResponse, ProviderCallError> {
		let headers = self.build_headers()?;
		let body = self.build_request_body(&model.model_id, request, false);
		let started_at = Instant::now();

		let response = self
			.client
			.post(&self.config.base_url)
			.headers(headers)
			.json(&body)
			.send()
			.await
			.map_err(classify_request_error)?;

		let status = response.status();
		let response_body = response
			.text()
			.await
			.map_err(|e| ProviderCallError::ServerError {
				status: 0,
				message: format!("failed to read response body: {e}"),
			})?;

		if !status.is_success() {
			log_anthropic(
				LogLevel::Warn,
				"provider returned non-success status",
				[
					("model", model.model_id.clone()),
					("status", status.to_string()),
					("body", truncate_for_log(&response_body, 800)),
				],
			);
			return Err(classify_status_error(status.as_u16(), response_body));
		}

		let parsed = parse_complete_response(&response_body)?;
		let latency_ms = u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX);

		log_anthropic(
			LogLevel::Info,
			"provider request completed",
			[
				("model", model.model_id.clone()),
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

	async fn stream(
		&self,
		model: &ModelProfile,
		request: &GenerationRequest,
		tx: mpsc::Sender<StreamChunk>,
	) -> Result<ProviderResponse, ProviderCallError> {
		let headers = self.build_headers()?;
		let body = self.build_request_body(&model.model_id, request, true);
		let started_at = Instant::now();

		let http_response = self
			.client
			.post(&self.config.base_url)
			.headers(headers)
			.json(&body)
			.send()
			.await
			.map_err(classify_request_error)?;

		let status = http_response.status();
		if !status.is_success() {
			let response_body = http_response.text().await.unwrap_or_default();
			log_anthropic(
				LogLevel::Warn,
				"provider returned non-success status on streaming request",
				[
					("model", model.model_id.clone()),
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
		let mut stream_error: Option<ProviderCallError> = None;

		// Track in-progress tool_use blocks by content_block index.
		let mut pending_tools: HashMap<u32, PendingToolUse> = HashMap::new();
		let mut completed_tools: Vec<ToolCallBlock> = Vec::new();

		let event_timeout = std::time::Duration::from_secs(120);

		loop {
			let event_result = match tokio::time::timeout(event_timeout, stream.next()).await {
				Ok(Some(result)) => result,
				Ok(None) => break,
				Err(_) => {
					stream_error = Some(ProviderCallError::Timeout {
						message: "SSE stream timed out waiting for next event".to_string(),
					});
					break;
				}
			};
			let event = match event_result {
				Ok(event) => event,
				Err(error) => {
					stream_error = Some(ProviderCallError::ConnectionFailed {
						message: format!("SSE stream error: {error}"),
					});
					break;
				}
			};

			let event_type = event.event.as_str();
			let data: Value = match serde_json::from_str(&event.data) {
				Ok(value) => value,
				Err(_) => continue,
			};

			match event_type {
				"message_start" => {
					// Extract initial usage from message_start.
					if let Some(usage) = data.get("message").and_then(|m| m.get("usage")) {
						prompt_tokens = usage
							.get("input_tokens")
							.and_then(Value::as_u64)
							.unwrap_or(0);
					}
				}
				"content_block_start" => {
					let index = data.get("index").and_then(Value::as_u64).unwrap_or(0) as u32;
					let block = data.get("content_block");
					if let Some(block) = block {
						let block_type = block.get("type").and_then(Value::as_str).unwrap_or("");
						if block_type == "tool_use" {
							let id = block
								.get("id")
								.and_then(Value::as_str)
								.unwrap_or("")
								.to_string();
							let name = block
								.get("name")
								.and_then(Value::as_str)
								.unwrap_or("")
								.to_string();
							let _ = tx
								.send(StreamChunk::ToolCallStart {
									id: id.clone(),
									name: name.clone(),
								})
								.await;
							pending_tools.insert(
								index,
								PendingToolUse {
									id,
									name,
									arguments_json: String::new(),
								},
							);
						}
					}
				}
				"content_block_delta" => {
					let index = data.get("index").and_then(Value::as_u64).unwrap_or(0) as u32;
					if let Some(delta) = data.get("delta") {
						let delta_type = delta.get("type").and_then(Value::as_str).unwrap_or("");
						match delta_type {
							"text_delta" => {
								if let Some(text) = delta.get("text").and_then(Value::as_str) {
									full_text.push_str(text);
									if tx
										.send(StreamChunk::TextDelta {
											text: text.to_string(),
										})
										.await
										.is_err()
									{
										break;
									}
								}
							}
							"input_json_delta" => {
								if let Some(partial_json) =
									delta.get("partial_json").and_then(Value::as_str)
									&& let Some(pending) = pending_tools.get_mut(&index)
								{
									pending.arguments_json.push_str(partial_json);
									let _ = tx
										.send(StreamChunk::ToolCallDelta {
											id: pending.id.clone(),
											arguments_chunk: partial_json.to_string(),
										})
										.await;
								}
							}
							_ => {}
						}
					}
				}
				"content_block_stop" => {
					let index = data.get("index").and_then(Value::as_u64).unwrap_or(0) as u32;
					if let Some(pending) = pending_tools.remove(&index) {
						let _ = tx
							.send(StreamChunk::ToolCallDone {
								id: pending.id.clone(),
							})
							.await;
						let arguments = serde_json::from_str::<Value>(&pending.arguments_json)
							.unwrap_or(Value::Null);
						completed_tools.push(ToolCallBlock {
							id: pending.id,
							name: pending.name,
							arguments,
						});
					}
				}
				"message_delta" => {
					if let Some(delta) = data.get("delta")
						&& let Some(reason) = delta.get("stop_reason").and_then(Value::as_str)
					{
						finish_reason = Some(normalize_stop_reason(reason));
					}
					if let Some(usage) = data.get("usage") {
						output_tokens = usage
							.get("output_tokens")
							.and_then(Value::as_u64)
							.unwrap_or(output_tokens);
					}
				}
				"message_stop" => {
					break;
				}
				"error" => {
					let message = data
						.get("error")
						.and_then(|e| e.get("message"))
						.and_then(Value::as_str)
						.unwrap_or("unknown streaming error");
					stream_error = Some(ProviderCallError::ServerError {
						status: 0,
						message: format!("anthropic streaming error: {message}"),
					});
					break;
				}
				_ => {}
			}
		}

		let latency_ms = u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX);

		// Drain any tool_use blocks that started but never received
		// content_block_stop (e.g. stream was interrupted mid-tool).
		for (_, pending) in pending_tools.drain() {
			let _ = tx
				.send(StreamChunk::ToolCallDone {
					id: pending.id.clone(),
				})
				.await;
			let arguments =
				serde_json::from_str::<Value>(&pending.arguments_json).unwrap_or(Value::Null);
			completed_tools.push(ToolCallBlock {
				id: pending.id,
				name: pending.name,
				arguments,
			});
		}

		if prompt_tokens == 0 {
			prompt_tokens = estimate_prompt_tokens(&full_text);
		}

		let tool_calls = if completed_tools.is_empty() {
			None
		} else {
			Some(completed_tools)
		};

		let _ = tx
			.send(StreamChunk::Done {
				finish_reason: finish_reason.clone(),
				prompt_tokens,
				output_tokens,
			})
			.await;

		if let Some(error) = stream_error {
			return Err(error);
		}

		log_anthropic(
			LogLevel::Info,
			"provider streaming request completed",
			[
				("model", model.model_id.clone()),
				("status", "ok".to_string()),
				("latency_ms", latency_ms.to_string()),
				("prompt_tokens", prompt_tokens.to_string()),
				("output_tokens", output_tokens.to_string()),
			],
		);

		Ok(ProviderResponse {
			output: full_text,
			finish_reason,
			prompt_tokens,
			output_tokens,
			latency_ms,
			tool_calls,
		})
	}
}

// ---------------------------------------------------------------------------
// Streaming accumulator
// ---------------------------------------------------------------------------

struct PendingToolUse {
	id: String,
	name: String,
	arguments_json: String,
}

// ---------------------------------------------------------------------------
// Non-streaming response parsing
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct ParsedAnthropicResponse {
	output: String,
	finish_reason: Option<String>,
	prompt_tokens: u64,
	output_tokens: u64,
	tool_calls: Option<Vec<ToolCallBlock>>,
}

fn parse_complete_response(body: &str) -> Result<ParsedAnthropicResponse, ProviderCallError> {
	let response: Value =
		serde_json::from_str(body).map_err(|e| ProviderCallError::ServerError {
			status: 0,
			message: format!("invalid response json: {e}"),
		})?;

	// Check for API error.
	if let Some(error) = response.get("error") {
		let message = error
			.get("message")
			.and_then(Value::as_str)
			.unwrap_or("unknown error");
		return Err(ProviderCallError::ServerError {
			status: 0,
			message: format!("anthropic api error: {message}"),
		});
	}

	let stop_reason = response
		.get("stop_reason")
		.and_then(Value::as_str)
		.map(normalize_stop_reason);

	let usage = response.get("usage");
	let prompt_tokens = usage
		.and_then(|u| u.get("input_tokens"))
		.and_then(Value::as_u64)
		.unwrap_or(0);
	let output_tokens = usage
		.and_then(|u| u.get("output_tokens"))
		.and_then(Value::as_u64)
		.unwrap_or(0);

	let content = response
		.get("content")
		.and_then(Value::as_array)
		.cloned()
		.unwrap_or_default();

	let mut text_parts = Vec::new();
	let mut tool_calls = Vec::new();

	for block in &content {
		let block_type = block.get("type").and_then(Value::as_str).unwrap_or("");
		match block_type {
			"text" => {
				if let Some(text) = block.get("text").and_then(Value::as_str) {
					text_parts.push(text.to_string());
				}
			}
			"tool_use" => {
				let id = block
					.get("id")
					.and_then(Value::as_str)
					.unwrap_or("")
					.to_string();
				let name = block
					.get("name")
					.and_then(Value::as_str)
					.unwrap_or("")
					.to_string();
				let arguments = block.get("input").cloned().unwrap_or(Value::Null);
				tool_calls.push(ToolCallBlock {
					id,
					name,
					arguments,
				});
			}
			_ => {}
		}
	}

	Ok(ParsedAnthropicResponse {
		output: text_parts.join(""),
		finish_reason: stop_reason,
		prompt_tokens,
		output_tokens,
		tool_calls: if tool_calls.is_empty() {
			None
		} else {
			Some(tool_calls)
		},
	})
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Normalize Anthropic stop reasons to a common vocabulary.
/// Anthropic uses `end_turn` / `tool_use` / `max_tokens` / `stop_sequence`.
fn normalize_stop_reason(reason: &str) -> String {
	match reason {
		"end_turn" => "stop".to_string(),
		"tool_use" => "tool_calls".to_string(),
		other => other.to_string(),
	}
}

fn classify_request_error(error: reqwest::Error) -> ProviderCallError {
	if error.is_timeout() {
		ProviderCallError::Timeout {
			message: format!("request failed: {error}"),
		}
	} else if error.is_connect() {
		ProviderCallError::ConnectionFailed {
			message: format!("request failed: {error}"),
		}
	} else {
		ProviderCallError::Fatal {
			message: format!("request failed: {error}"),
		}
	}
}

fn classify_status_error(status_code: u16, response_body: String) -> ProviderCallError {
	let message = format!("anthropic returned status {status_code}: {response_body}");
	match status_code {
		401 | 403 => ProviderCallError::AuthenticationFailed { message },
		429 => ProviderCallError::RateLimit {
			message,
			retry_after: None,
		},
		400 => {
			let body_lower = response_body.to_lowercase();
			if body_lower.contains("context")
				&& (body_lower.contains("exceeded")
					|| body_lower.contains("too long")
					|| body_lower.contains("maximum"))
			{
				ProviderCallError::ContextWindowExceeded { detail: message }
			} else {
				ProviderCallError::InvalidRequest { message }
			}
		}
		408 | 409 => ProviderCallError::ServerError {
			status: status_code,
			message,
		},
		529 | 503 => ProviderCallError::ServerOverloaded {
			message,
			retry_after: None,
		},
		500..=599 => ProviderCallError::ServerError {
			status: status_code,
			message,
		},
		_ => ProviderCallError::Fatal { message },
	}
}

fn truncate_for_log(text: &str, max_chars: usize) -> String {
	let char_count = text.chars().count();
	if char_count <= max_chars {
		text.to_string()
	} else {
		let truncated: String = text.chars().take(max_chars).collect();
		format!("{truncated}...<truncated>")
	}
}

fn log_anthropic(
	level: LogLevel,
	message: &str,
	fields: impl IntoIterator<Item = (&'static str, String)>,
) {
	let record = fields.into_iter().fold(
		LogRecord::new("roku-plugin-llm", level, message)
			.with_field("provider", ANTHROPIC_PROVIDER),
		|record, (key, value)| record.with_field(key, value),
	);
	let _ = emit_global_log(record);
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parse_text_only_response() {
		let body = r#"{
			"id": "msg_01",
			"type": "message",
			"role": "assistant",
			"content": [{"type": "text", "text": "Hello world"}],
			"model": "claude-sonnet-4-5-20250514",
			"stop_reason": "end_turn",
			"usage": {"input_tokens": 10, "output_tokens": 5}
		}"#;
		let parsed = parse_complete_response(body).unwrap();
		assert_eq!(parsed.output, "Hello world");
		assert_eq!(parsed.finish_reason.as_deref(), Some("stop"));
		assert_eq!(parsed.prompt_tokens, 10);
		assert_eq!(parsed.output_tokens, 5);
		assert!(parsed.tool_calls.is_none());
	}

	#[test]
	fn parse_tool_use_response() {
		let body = r#"{
			"id": "msg_02",
			"type": "message",
			"role": "assistant",
			"content": [
				{"type": "text", "text": "Let me check that."},
				{
					"type": "tool_use",
					"id": "toolu_01",
					"name": "web_fetch",
					"input": {"url": "https://example.com"}
				}
			],
			"model": "claude-sonnet-4-5-20250514",
			"stop_reason": "tool_use",
			"usage": {"input_tokens": 50, "output_tokens": 30}
		}"#;
		let parsed = parse_complete_response(body).unwrap();
		assert_eq!(parsed.output, "Let me check that.");
		assert_eq!(parsed.finish_reason.as_deref(), Some("tool_calls"));
		let tool_calls = parsed.tool_calls.unwrap();
		assert_eq!(tool_calls.len(), 1);
		assert_eq!(tool_calls[0].id, "toolu_01");
		assert_eq!(tool_calls[0].name, "web_fetch");
		assert_eq!(tool_calls[0].arguments["url"], "https://example.com");
	}

	#[test]
	fn parse_multi_tool_response() {
		let body = r#"{
			"id": "msg_03",
			"type": "message",
			"role": "assistant",
			"content": [
				{
					"type": "tool_use",
					"id": "toolu_01",
					"name": "read_file",
					"input": {"path": "/tmp/a.txt"}
				},
				{
					"type": "tool_use",
					"id": "toolu_02",
					"name": "read_file",
					"input": {"path": "/tmp/b.txt"}
				}
			],
			"model": "claude-sonnet-4-5-20250514",
			"stop_reason": "tool_use",
			"usage": {"input_tokens": 100, "output_tokens": 60}
		}"#;
		let parsed = parse_complete_response(body).unwrap();
		assert!(parsed.output.is_empty());
		let tool_calls = parsed.tool_calls.unwrap();
		assert_eq!(tool_calls.len(), 2);
		assert_eq!(tool_calls[0].name, "read_file");
		assert_eq!(tool_calls[1].name, "read_file");
	}

	#[test]
	fn parse_error_response() {
		let body = r#"{
			"type": "error",
			"error": {
				"type": "invalid_request_error",
				"message": "max_tokens must be less than 8192"
			}
		}"#;
		let result = parse_complete_response(body);
		assert!(result.is_err());
		let err = result.unwrap_err().to_string();
		assert!(err.contains("max_tokens"));
	}

	#[test]
	fn normalize_stop_reasons() {
		assert_eq!(normalize_stop_reason("end_turn"), "stop");
		assert_eq!(normalize_stop_reason("tool_use"), "tool_calls");
		assert_eq!(normalize_stop_reason("max_tokens"), "max_tokens");
		assert_eq!(normalize_stop_reason("stop_sequence"), "stop_sequence");
	}

	#[test]
	fn build_request_body_with_tools() {
		let config = AnthropicConfig {
			api_key: "test-key".to_string(),
			base_url: DEFAULT_ANTHROPIC_URL.to_string(),
			max_tokens: DEFAULT_MAX_TOKENS,
		};
		let provider = AnthropicProvider::new(config).unwrap();
		let request = GenerationRequest {
			system_prompt: Some("You are helpful.".to_string()),
			prompt: "Hello".to_string(),
			messages: None,
			expected_output_tokens: 1024,
			risk_tier: crate::types::RiskTier::Low,
			preferred_provider: None,
			budget_tokens_remaining: 100_000,
			budget_cost_remaining_usd: 10.0,
			tools: Some(vec![crate::types::ToolDefinition {
				name: "web_fetch".to_string(),
				description: "Fetch a URL".to_string(),
				parameters: serde_json::json!({"type": "object", "properties": {"url": {"type": "string"}}}),
			}]),
			model_override: None,
			thinking_effort: None,
		};
		let body = provider.build_request_body("claude-sonnet-4-5-20250514", &request, false);
		assert_eq!(body["model"], "claude-sonnet-4-5-20250514");
		assert_eq!(body["max_tokens"], 1024);
		assert_eq!(body["system"], "You are helpful.");
		assert!(body.get("stream").is_none());
		let tools = body["tools"].as_array().unwrap();
		assert_eq!(tools.len(), 1);
		assert_eq!(tools[0]["name"], "web_fetch");
		assert!(tools[0].get("input_schema").is_some());
	}

	#[test]
	fn build_request_body_stream_no_tools() {
		let config = AnthropicConfig {
			api_key: "test-key".to_string(),
			base_url: DEFAULT_ANTHROPIC_URL.to_string(),
			max_tokens: DEFAULT_MAX_TOKENS,
		};
		let provider = AnthropicProvider::new(config).unwrap();
		let request = GenerationRequest {
			system_prompt: None,
			prompt: "Hi".to_string(),
			messages: None,
			expected_output_tokens: 512,
			risk_tier: crate::types::RiskTier::Low,
			preferred_provider: None,
			budget_tokens_remaining: 50_000,
			budget_cost_remaining_usd: 5.0,
			tools: None,
			model_override: None,
			thinking_effort: None,
		};
		let body = provider.build_request_body("claude-sonnet-4-5-20250514", &request, true);
		assert_eq!(body["stream"], true);
		assert!(body.get("system").is_none());
		assert!(body.get("tools").is_none());
	}
}
