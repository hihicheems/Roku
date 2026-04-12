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

//! Direct OpenAI Chat Completions API provider with native function calling.
//!
//! Implements [`LlmProvider`] for the OpenAI Chat Completions API, supporting
//! both non-streaming and streaming request modes. Streaming responses
//! accumulate tool call deltas using an index-keyed tracker, following the
//! critical invariant that `ToolCallStart` must precede `ToolCallDelta` for
//! each tool call index.
//!
//! Adapted from the patterns in OpenAI's official Codex CLI (`codex-rs`) and
//! rara's production OpenAI provider (`kernel/src/llm/openai.rs`).

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
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio::sync::mpsc;

use crate::router::{LlmProvider, LlmRouter};
use crate::types::{
	GenerationRequest, Message, ModelProfile, ProviderCallError, ProviderResponse, RiskTier,
	RoutingPolicy, StreamChunk, ToolCallBlock, estimate_prompt_tokens,
};

const OPENAI_PROVIDER: &str = "openai";
const DEFAULT_OPENAI_URL: &str = "https://api.openai.com/v1/chat/completions";
const DEFAULT_MAX_TOKENS: u64 = 8192;
const DEFAULT_OPENAI_PRIMARY_MODEL: &str = "gpt-5.4";
const DEFAULT_OPENAI_FALLBACK_MODEL: &str = "gpt-5.1-codex-mini";
const DEFAULT_OPENAI_MAX_CONTEXT_TOKENS: u64 = 128_000;
const HARD_MAX_CONTEXT_TOKENS: u64 = 1_000_000;
const HARD_MAX_REQUEST_COST_USD: f64 = 100.0;
const HARD_MAX_LATENCY_MS: u64 = 300_000;

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// Effective non-secret OpenAI runtime configuration.
///
/// Mirrors the shape of [`AnthropicRuntimeConfig`] and
/// [`OpenRouterRuntimeConfig`] so the startup layer can treat every
/// provider uniformly. Secrets (API key) stay out of this struct — they
/// are attached via [`OpenAiRuntimeConfig::with_api_key`].
#[derive(Debug, Clone, PartialEq)]
pub struct OpenAiRuntimeConfig {
	pub primary_model: String,
	pub fallback_models: Vec<String>,
	pub base_url: String,
	pub max_tokens: u64,
	pub max_context_tokens: u64,
	pub cost_per_1k_tokens_usd: f64,
	pub max_request_cost_usd: f64,
	pub max_latency_ms: u64,
	/// Optional reasoning effort level sent to the API (`low`, `medium`, `high`).
	/// Only effective for models that support reasoning (gpt-5.x, o-series).
	pub reasoning_effort: Option<String>,
}

/// Partial overrides for [`OpenAiRuntimeConfig`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenAiRuntimeConfigPatch {
	pub primary_model: Option<String>,
	pub fallback_models: Option<Vec<String>>,
	pub base_url: Option<String>,
	pub max_tokens: Option<u64>,
	pub max_context_tokens: Option<u64>,
	pub cost_per_1k_tokens_usd: Option<f64>,
	pub max_request_cost_usd: Option<f64>,
	pub max_latency_ms: Option<u64>,
	pub reasoning_effort: Option<String>,
}

impl Default for OpenAiRuntimeConfig {
	fn default() -> Self {
		Self {
			primary_model: DEFAULT_OPENAI_PRIMARY_MODEL.to_string(),
			fallback_models: vec![DEFAULT_OPENAI_FALLBACK_MODEL.to_string()],
			base_url: DEFAULT_OPENAI_URL.to_string(),
			max_tokens: DEFAULT_MAX_TOKENS,
			max_context_tokens: DEFAULT_OPENAI_MAX_CONTEXT_TOKENS,
			cost_per_1k_tokens_usd: 0.0,
			max_request_cost_usd: 1.0,
			max_latency_ms: 60_000,
			reasoning_effort: None,
		}
	}
}

impl OpenAiRuntimeConfig {
	pub fn apply_patch(&mut self, patch: OpenAiRuntimeConfigPatch) {
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
		if let Some(value) = patch.reasoning_effort {
			self.reasoning_effort = Some(value);
		}
	}

	pub fn apply_env_overrides(&mut self) -> Result<(), OpenAiBootstrapError> {
		if let Some(value) = env_override_string("ROKU_OPENAI_PRIMARY_MODEL") {
			self.primary_model = value;
		}
		if let Some(value) = env_override_string("ROKU_OPENAI_BASE_URL") {
			self.base_url = value;
		}
		if let Some(value) = env_var_u64("ROKU_OPENAI_MAX_TOKENS")? {
			self.max_tokens = value;
		}
		if let Some(value) = env_override_string("ROKU_OPENAI_REASONING_EFFORT") {
			self.reasoning_effort = Some(value);
		}
		Ok(())
	}

	pub fn validate_and_clamp(&mut self) -> Result<(), OpenAiBootstrapError> {
		self.primary_model = self.primary_model.trim().to_string();
		self.base_url = self.base_url.trim().to_string();
		if self.primary_model.is_empty() {
			return Err(OpenAiBootstrapError::InvalidEnv {
				key: "ROKU_OPENAI_PRIMARY_MODEL",
				message: "value cannot be empty".to_string(),
			});
		}
		if self.base_url.is_empty() {
			return Err(OpenAiBootstrapError::InvalidEnv {
				key: "ROKU_OPENAI_BASE_URL",
				message: "value cannot be empty".to_string(),
			});
		}
		if self.max_tokens == 0 {
			return Err(OpenAiBootstrapError::InvalidEnv {
				key: "ROKU_OPENAI_MAX_TOKENS",
				message: "value must be greater than zero".to_string(),
			});
		}
		if self.max_context_tokens == 0 {
			return Err(OpenAiBootstrapError::InvalidEnv {
				key: "max_context_tokens",
				message: "value must be greater than zero".to_string(),
			});
		}
		if self.cost_per_1k_tokens_usd < 0.0 {
			return Err(OpenAiBootstrapError::InvalidEnv {
				key: "cost_per_1k_tokens_usd",
				message: "value must be non-negative".to_string(),
			});
		}
		if self.max_request_cost_usd <= 0.0 {
			return Err(OpenAiBootstrapError::InvalidEnv {
				key: "max_request_cost_usd",
				message: "value must be greater than zero".to_string(),
			});
		}
		if self.max_latency_ms == 0 {
			return Err(OpenAiBootstrapError::InvalidEnv {
				key: "max_latency_ms",
				message: "value must be greater than zero".to_string(),
			});
		}
		self.max_context_tokens = self.max_context_tokens.min(HARD_MAX_CONTEXT_TOKENS);
		self.max_request_cost_usd = self.max_request_cost_usd.min(HARD_MAX_REQUEST_COST_USD);
		self.max_latency_ms = self.max_latency_ms.min(HARD_MAX_LATENCY_MS);
		self.fallback_models
			.retain(|model| model != &self.primary_model);
		Ok(())
	}

	/// Attach a resolved API key to produce the full [`OpenAiConfig`] the
	/// provider uses at request time.
	pub fn with_api_key(self, api_key: String) -> OpenAiConfig {
		OpenAiConfig {
			api_key,
			base_url: self.base_url,
			max_tokens: self.max_tokens,
			reasoning_effort: self.reasoning_effort,
		}
	}
}

/// Full per-request OpenAI configuration, including the resolved API key.
/// Constructed via [`OpenAiRuntimeConfig::with_api_key`] at bootstrap time
/// or [`OpenAiConfig::from_env`] for ad-hoc use.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenAiConfig {
	pub api_key: String,
	pub base_url: String,
	pub max_tokens: u64,
	pub reasoning_effort: Option<String>,
}

impl OpenAiConfig {
	pub fn from_env() -> Result<Self, ProviderCallError> {
		let api_key = env::var("ROKU_OPENAI_API_KEY").map_err(|_| {
			ProviderCallError::non_retryable("ROKU_OPENAI_API_KEY environment variable is not set")
		})?;
		let base_url =
			env::var("ROKU_OPENAI_BASE_URL").unwrap_or_else(|_| DEFAULT_OPENAI_URL.to_string());
		let reasoning_effort = env::var("ROKU_OPENAI_REASONING_EFFORT")
			.ok()
			.filter(|v| !v.is_empty());
		Ok(Self {
			api_key,
			base_url,
			max_tokens: DEFAULT_MAX_TOKENS,
			reasoning_effort,
		})
	}
}

#[derive(Debug, Error)]
pub enum OpenAiBootstrapError {
	#[error("missing required environment variable: {0}")]
	MissingEnv(&'static str),
	#[error("invalid environment variable {key}: {message}")]
	InvalidEnv { key: &'static str, message: String },
	#[error("failed to construct openai http client: {0}")]
	HttpClient(#[from] reqwest::Error),
	#[error("openai provider bootstrap failed: {0}")]
	Provider(String),
}

impl From<ProviderCallError> for OpenAiBootstrapError {
	fn from(error: ProviderCallError) -> Self {
		OpenAiBootstrapError::Provider(error.to_string())
	}
}

fn env_override_string(key: &str) -> Option<String> {
	std::env::var(key)
		.ok()
		.map(|value| value.trim().to_string())
		.filter(|value| !value.is_empty())
}

fn env_var_u64(key: &'static str) -> Result<Option<u64>, OpenAiBootstrapError> {
	let Some(raw) = env_override_string(key) else {
		return Ok(None);
	};
	raw.parse::<u64>()
		.map(Some)
		.map_err(|error| OpenAiBootstrapError::InvalidEnv {
			key,
			message: error.to_string(),
		})
}

/// Read the OpenAI API key from the environment.
///
/// Kept separate from [`OpenAiRuntimeConfig`] so future credential sources
/// (OAuth via ChatGPT login, TUI picker, keyring) can be added without
/// reshaping the runtime config surface.
pub fn openai_api_key_from_env() -> Result<String, OpenAiBootstrapError> {
	std::env::var("ROKU_OPENAI_API_KEY")
		.ok()
		.map(|value| value.trim().to_string())
		.filter(|value| !value.is_empty())
		.ok_or(OpenAiBootstrapError::MissingEnv("ROKU_OPENAI_API_KEY"))
}

/// Construct a live [`LlmRouter`] backed by OpenAI.
///
/// Registers the provider plus a prioritized sequence of
/// [`ModelProfile`] entries built from
/// [`OpenAiRuntimeConfig::primary_model`] and its fallback chain.
pub fn build_openai_router_with_metrics(
	config: OpenAiConfig,
	runtime_config: OpenAiRuntimeConfig,
	metrics: Arc<Metrics>,
) -> Result<LlmRouter, OpenAiBootstrapError> {
	let mut router = LlmRouter::new(RoutingPolicy {
		max_request_cost_usd: runtime_config.max_request_cost_usd,
		max_latency_ms: runtime_config.max_latency_ms,
	})
	.with_metrics(metrics);
	router.register_provider(OpenAiProvider::new(config)?);

	let mut model_chain =
		Vec::with_capacity(runtime_config.fallback_models.len().saturating_add(1));
	model_chain.push(runtime_config.primary_model.clone());
	model_chain.extend(runtime_config.fallback_models.iter().cloned());

	for (index, model_id) in model_chain.into_iter().enumerate() {
		router.register_model(ModelProfile {
			model_id,
			provider: OPENAI_PROVIDER.to_string(),
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

pub struct OpenAiProvider {
	client: Client,
	config: OpenAiConfig,
}

impl OpenAiProvider {
	pub fn new(config: OpenAiConfig) -> Result<Self, ProviderCallError> {
		let client = Client::builder()
			.build()
			.map_err(|e| ProviderCallError::non_retryable(format!("http client error: {e}")))?;
		Ok(Self { client, config })
	}

	fn build_headers(&self) -> Result<HeaderMap, ProviderCallError> {
		let mut headers = HeaderMap::new();
		headers.insert(
			reqwest::header::AUTHORIZATION,
			HeaderValue::from_str(&format!("Bearer {}", self.config.api_key)).map_err(|e| {
				ProviderCallError::non_retryable(format!("invalid authorization header: {e}"))
			})?,
		);
		headers.insert(
			reqwest::header::CONTENT_TYPE,
			HeaderValue::from_static("application/json"),
		);
		Ok(headers)
	}
}

// ---------------------------------------------------------------------------
// Request building
// ---------------------------------------------------------------------------

/// OpenAI Chat Completions request body.
///
/// Follows the format from codex-rs and rara's WireTool/ChatRequest patterns.
#[derive(Debug, Serialize)]
struct ChatCompletionRequest<'a> {
	model: &'a str,
	messages: Vec<Value>,
	#[serde(skip_serializing_if = "Option::is_none")]
	max_tokens: Option<u64>,
	#[serde(skip_serializing_if = "std::ops::Not::not")]
	stream: bool,
	#[serde(skip_serializing_if = "Option::is_none")]
	tools: Option<Vec<OpenAiToolDefinition>>,
	/// Request usage in streaming chunks (required to get token counts).
	#[serde(skip_serializing_if = "Option::is_none")]
	stream_options: Option<StreamOptions>,
	/// Reasoning effort level for models that support it (gpt-5.x, o-series).
	#[serde(skip_serializing_if = "Option::is_none")]
	reasoning_effort: Option<&'a str>,
}

fn message_to_openai_value(msg: &Message) -> Value {
	match msg {
		Message::User { content } => serde_json::json!({"role": "user", "content": content}),
		Message::Assistant { text, tool_calls } if tool_calls.is_empty() => {
			serde_json::json!({"role": "assistant", "content": text})
		}
		Message::Assistant { text, tool_calls } => {
			let tc_array: Vec<Value> = tool_calls
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
			serde_json::json!({"role": "assistant", "content": text, "tool_calls": tc_array})
		}
		Message::ToolResult {
			tool_use_id,
			content,
			..
		} => serde_json::json!({"role": "tool", "tool_call_id": tool_use_id, "content": content}),
	}
}

/// OpenAI tool definition: `{type: "function", function: {name, description, parameters}}`
#[derive(Debug, Clone, Serialize)]
struct OpenAiToolDefinition {
	r#type: &'static str,
	function: OpenAiFunctionDef,
}

#[derive(Debug, Clone, Serialize)]
struct OpenAiFunctionDef {
	name: String,
	description: String,
	parameters: Value,
}

#[derive(Debug, Serialize)]
struct StreamOptions {
	include_usage: bool,
}

fn build_request<'a>(
	model_id: &'a str,
	request: &'a GenerationRequest,
	stream: bool,
	max_tokens: u64,
	reasoning_effort: Option<&'a str>,
) -> ChatCompletionRequest<'a> {
	let mut messages: Vec<Value> = Vec::with_capacity(4);
	if let Some(system_prompt) = &request.system_prompt {
		messages.push(serde_json::json!({"role": "system", "content": system_prompt}));
	}
	if let Some(msgs) = &request.messages {
		for msg in msgs {
			messages.push(message_to_openai_value(msg));
		}
	} else {
		messages.push(serde_json::json!({"role": "user", "content": &request.prompt}));
	}

	let tools = request.tools.as_ref().map(|tool_defs| {
		tool_defs
			.iter()
			.map(|tool| OpenAiToolDefinition {
				r#type: "function",
				function: OpenAiFunctionDef {
					name: tool.name.clone(),
					description: tool.description.clone(),
					parameters: tool.parameters.clone(),
				},
			})
			.collect::<Vec<_>>()
	});

	let max_tokens_value = if request.expected_output_tokens > 0 {
		request.expected_output_tokens
	} else {
		max_tokens
	};

	ChatCompletionRequest {
		model: model_id,
		messages,
		max_tokens: Some(max_tokens_value),
		stream,
		tools: tools.filter(|t| !t.is_empty()),
		stream_options: if stream {
			Some(StreamOptions {
				include_usage: true,
			})
		} else {
			None
		},
		reasoning_effort,
	}
}

// ---------------------------------------------------------------------------
// Non-streaming response parsing
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct ParsedChatCompletion {
	output: String,
	finish_reason: Option<String>,
	prompt_tokens: u64,
	output_tokens: u64,
	tool_calls: Option<Vec<ToolCallBlock>>,
}

fn parse_complete_response(body: &str) -> Result<ParsedChatCompletion, ProviderCallError> {
	let response: Value = serde_json::from_str(body)
		.map_err(|e| ProviderCallError::retryable(format!("invalid response json: {e}")))?;

	// Check for API error.
	if let Some(error) = response.get("error") {
		let message = error
			.get("message")
			.and_then(Value::as_str)
			.unwrap_or("unknown error");
		return Err(ProviderCallError::retryable(format!(
			"openai api error: {message}"
		)));
	}

	let choice = response
		.get("choices")
		.and_then(Value::as_array)
		.and_then(|c| c.first())
		.ok_or_else(|| {
			ProviderCallError::retryable("openai response contained no choices".to_string())
		})?;

	let finish_reason = choice
		.get("finish_reason")
		.and_then(Value::as_str)
		.map(str::to_string);

	let message = choice.get("message");

	// Parse tool_calls from message.
	let tool_calls = parse_tool_calls_from_message(message);

	// Extract text content. May be null when tool_calls are present.
	let output = message
		.and_then(|m| m.get("content"))
		.and_then(Value::as_str)
		.unwrap_or("")
		.to_string();

	let usage = response.get("usage");
	let prompt_tokens = usage
		.and_then(|u| u.get("prompt_tokens"))
		.and_then(Value::as_u64)
		.unwrap_or(0);
	let output_tokens = usage
		.and_then(|u| u.get("completion_tokens"))
		.and_then(Value::as_u64)
		.unwrap_or(0);

	Ok(ParsedChatCompletion {
		output,
		finish_reason,
		prompt_tokens,
		output_tokens,
		tool_calls,
	})
}

/// Parse `tool_calls` from an OpenAI message object.
///
/// Format: `[{id, type: "function", function: {name, arguments: "json-string"}}]`
///
/// Adapted from openrouter.rs `parse_tool_calls_from_message` and rara's wire
/// format parsing.
fn parse_tool_calls_from_message(message: Option<&Value>) -> Option<Vec<ToolCallBlock>> {
	let tool_calls_json = message?.get("tool_calls").and_then(Value::as_array)?;

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

// ---------------------------------------------------------------------------
// Streaming tool call accumulator
// ---------------------------------------------------------------------------

/// Tracks in-progress tool calls during streaming, keyed by the `index` field
/// in SSE `delta.tool_calls[*]` chunks.
///
/// Critical invariant (from rara `kernel/AGENT.md`):
/// `ToolCallStart` MUST be emitted before any `ToolCallDelta` for the same
/// index. Some providers deliver `id` + `name` + `arguments` in a single
/// chunk; this accumulator handles that by checking the `started` flag.
struct PendingToolCall {
	id: String,
	name: String,
	arguments: String,
	/// Whether `ToolCallStart` has been emitted for this entry.
	started: bool,
}

/// Process a single streaming `delta.tool_calls` array.
///
/// For each entry: accumulate id/name/arguments, then emit `ToolCallStart`
/// (once, when both id and name are known) and `ToolCallDelta` (for each
/// arguments chunk).
async fn process_streaming_tool_calls(
	tool_calls_delta: &[Value],
	pending: &mut HashMap<u32, PendingToolCall>,
	tx: &mpsc::Sender<StreamChunk>,
) {
	for tc in tool_calls_delta {
		let index = tc.get("index").and_then(Value::as_u64).unwrap_or(0) as u32;

		let entry = pending.entry(index).or_insert_with(|| PendingToolCall {
			id: String::new(),
			name: String::new(),
			arguments: String::new(),
			started: false,
		});

		// Accumulate id (emitted once by the provider).
		if let Some(id) = tc.get("id").and_then(Value::as_str)
			&& !id.is_empty()
		{
			entry.id = id.to_string();
		}

		// Accumulate function name and arguments.
		let new_args = if let Some(function) = tc.get("function") {
			if let Some(name) = function.get("name").and_then(Value::as_str)
				&& !name.is_empty()
			{
				entry.name.push_str(name);
			}
			function
				.get("arguments")
				.and_then(Value::as_str)
				.filter(|s| !s.is_empty())
				.map(str::to_string)
		} else {
			None
		};

		// Emit ToolCallStart once both id and name are available.
		// This MUST happen before any ToolCallDelta for this index.
		if !entry.started && !entry.id.is_empty() && !entry.name.is_empty() {
			entry.started = true;
			let _ = tx
				.send(StreamChunk::ToolCallStart {
					id: entry.id.clone(),
					name: entry.name.clone(),
				})
				.await;
		}

		// Emit ToolCallDelta for new arguments (only after start).
		if let Some(args_chunk) = new_args {
			entry.arguments.push_str(&args_chunk);
			if entry.started {
				let _ = tx
					.send(StreamChunk::ToolCallDelta {
						id: entry.id.clone(),
						arguments_chunk: args_chunk,
					})
					.await;
			}
		}
	}
}

// ---------------------------------------------------------------------------
// LlmProvider implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl LlmProvider for OpenAiProvider {
	fn provider_name(&self) -> &'static str {
		OPENAI_PROVIDER
	}

	async fn complete(
		&self,
		model: &ModelProfile,
		request: &GenerationRequest,
	) -> Result<ProviderResponse, ProviderCallError> {
		let headers = self.build_headers()?;
		let body = build_request(
			&model.model_id,
			request,
			false,
			self.config.max_tokens,
			self.config.reasoning_effort.as_deref(),
		);
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
		let response_body = response.text().await.map_err(|e| {
			ProviderCallError::retryable(format!("failed to read response body: {e}"))
		})?;

		if !status.is_success() {
			log_openai(
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

		log_openai(
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
		let body = build_request(
			&model.model_id,
			request,
			true,
			self.config.max_tokens,
			self.config.reasoning_effort.as_deref(),
		);
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
			log_openai(
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

		let mut event_stream = http_response.bytes_stream().eventsource();
		let mut full_text = String::new();
		let mut finish_reason: Option<String> = None;
		let mut prompt_tokens: u64 = 0;
		let mut output_tokens: u64 = 0;
		let mut stream_error: Option<ProviderCallError> = None;
		let mut pending_tools: HashMap<u32, PendingToolCall> = HashMap::new();

		while let Some(event_result) = event_stream.next().await {
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

			// Extract usage from chunks that carry it (stream_options.include_usage).
			if let Some(usage) = chunk.get("usage").filter(|u| !u.is_null()) {
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

			let Some(choice) = choice else {
				continue;
			};

			// Extract finish_reason.
			if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str)
				&& !reason.is_empty()
			{
				finish_reason = Some(reason.to_string());
			}

			let delta = match choice.get("delta") {
				Some(d) => d,
				None => continue,
			};

			// Text content delta.
			if let Some(text) = delta.get("content").and_then(Value::as_str)
				&& !text.is_empty()
			{
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

			// Tool call deltas.
			if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
				process_streaming_tool_calls(tool_calls, &mut pending_tools, &tx).await;
			}
		}

		let latency_ms = u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX);

		// Drain any pending tool calls — emit ToolCallDone and collect into
		// completed list. Handles both normal completion and stream interruption.
		let mut completed_tools: Vec<ToolCallBlock> = Vec::new();
		for (_, pending) in pending_tools.drain() {
			// If start was never emitted (shouldn't happen in normal flow, but
			// guard against partial streams), emit it now.
			if !pending.started && !pending.id.is_empty() && !pending.name.is_empty() {
				let _ = tx
					.send(StreamChunk::ToolCallStart {
						id: pending.id.clone(),
						name: pending.name.clone(),
					})
					.await;
			}
			let _ = tx
				.send(StreamChunk::ToolCallDone {
					id: pending.id.clone(),
				})
				.await;
			let arguments =
				serde_json::from_str::<Value>(&pending.arguments).unwrap_or(Value::Null);
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

		log_openai(
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
// Helpers
// ---------------------------------------------------------------------------

fn classify_request_error(error: reqwest::Error) -> ProviderCallError {
	if error.is_timeout() || error.is_connect() {
		ProviderCallError::retryable(format!("request failed: {error}"))
	} else {
		ProviderCallError::non_retryable(format!("request failed: {error}"))
	}
}

fn classify_status_error(status_code: u16, response_body: String) -> ProviderCallError {
	let message = format!("openai returned status {status_code}: {response_body}");
	match status_code {
		408 | 409 | 429 | 500..=599 => ProviderCallError::retryable(message),
		_ => ProviderCallError::non_retryable(message),
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

fn log_openai(
	level: LogLevel,
	message: &str,
	fields: impl IntoIterator<Item = (&'static str, String)>,
) {
	let record = fields.into_iter().fold(
		LogRecord::new("roku-plugin-llm", level, message).with_field("provider", OPENAI_PROVIDER),
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
			"id": "chatcmpl-01",
			"object": "chat.completion",
			"choices": [{
				"index": 0,
				"message": {"role": "assistant", "content": "Hello world"},
				"finish_reason": "stop"
			}],
			"usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
		}"#;
		let parsed = parse_complete_response(body).unwrap();
		assert_eq!(parsed.output, "Hello world");
		assert_eq!(parsed.finish_reason.as_deref(), Some("stop"));
		assert_eq!(parsed.prompt_tokens, 10);
		assert_eq!(parsed.output_tokens, 5);
		assert!(parsed.tool_calls.is_none());
	}

	#[test]
	fn parse_tool_call_response() {
		let body = r#"{
			"id": "chatcmpl-02",
			"choices": [{
				"index": 0,
				"message": {
					"role": "assistant",
					"content": null,
					"tool_calls": [{
						"id": "call_abc",
						"type": "function",
						"function": {
							"name": "web_fetch",
							"arguments": "{\"url\":\"https://example.com\"}"
						}
					}]
				},
				"finish_reason": "tool_calls"
			}],
			"usage": {"prompt_tokens": 50, "completion_tokens": 30, "total_tokens": 80}
		}"#;
		let parsed = parse_complete_response(body).unwrap();
		assert!(parsed.output.is_empty());
		assert_eq!(parsed.finish_reason.as_deref(), Some("tool_calls"));
		let tool_calls = parsed.tool_calls.unwrap();
		assert_eq!(tool_calls.len(), 1);
		assert_eq!(tool_calls[0].id, "call_abc");
		assert_eq!(tool_calls[0].name, "web_fetch");
		assert_eq!(tool_calls[0].arguments["url"], "https://example.com");
	}

	#[test]
	fn parse_multi_tool_response() {
		let body = r#"{
			"id": "chatcmpl-03",
			"choices": [{
				"index": 0,
				"message": {
					"role": "assistant",
					"content": "Let me read both files.",
					"tool_calls": [
						{"id": "call_1", "type": "function", "function": {"name": "read_file", "arguments": "{\"path\":\"/a.txt\"}"}},
						{"id": "call_2", "type": "function", "function": {"name": "read_file", "arguments": "{\"path\":\"/b.txt\"}"}}
					]
				},
				"finish_reason": "tool_calls"
			}],
			"usage": {"prompt_tokens": 100, "completion_tokens": 60, "total_tokens": 160}
		}"#;
		let parsed = parse_complete_response(body).unwrap();
		assert_eq!(parsed.output, "Let me read both files.");
		let tool_calls = parsed.tool_calls.unwrap();
		assert_eq!(tool_calls.len(), 2);
		assert_eq!(tool_calls[0].id, "call_1");
		assert_eq!(tool_calls[1].id, "call_2");
	}

	#[test]
	fn parse_error_response() {
		let body = r#"{
			"error": {
				"message": "Rate limit exceeded",
				"type": "rate_limit_error",
				"code": "rate_limit_exceeded"
			}
		}"#;
		let result = parse_complete_response(body);
		assert!(result.is_err());
		let err = result.unwrap_err().to_string();
		assert!(err.contains("Rate limit"));
	}

	#[test]
	fn build_request_with_tools() {
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
		};
		let body = build_request("gpt-4o", &request, false, DEFAULT_MAX_TOKENS, None);
		let json = serde_json::to_value(&body).unwrap();
		assert_eq!(json["model"], "gpt-4o");
		assert_eq!(json["max_tokens"], 1024);
		// stream=false is omitted by serde skip_serializing_if, so the key is absent.
		assert!(json.get("stream").is_none());
		let tools = json["tools"].as_array().unwrap();
		assert_eq!(tools.len(), 1);
		assert_eq!(tools[0]["type"], "function");
		assert_eq!(tools[0]["function"]["name"], "web_fetch");
		assert!(json.get("stream_options").is_none());
	}

	#[test]
	fn build_streaming_request_includes_usage_option() {
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
		};
		let body = build_request("gpt-4o", &request, true, DEFAULT_MAX_TOKENS, None);
		let json = serde_json::to_value(&body).unwrap();
		assert!(json["stream"].as_bool().unwrap());
		assert_eq!(json["stream_options"]["include_usage"], true);
		assert!(json.get("tools").is_none());
	}

	#[tokio::test]
	async fn streaming_tool_call_accumulation() {
		let (tx, mut rx) = mpsc::channel::<StreamChunk>(32);
		let mut pending = HashMap::new();

		// Chunk 1: id + name arrive.
		let chunk1 = vec![serde_json::json!({
			"index": 0,
			"id": "call_abc",
			"function": {"name": "web_fetch", "arguments": ""}
		})];
		process_streaming_tool_calls(&chunk1, &mut pending, &tx).await;

		// Chunk 2: arguments arrive.
		let chunk2 = vec![serde_json::json!({
			"index": 0,
			"function": {"arguments": "{\"url\":\"https://"}
		})];
		process_streaming_tool_calls(&chunk2, &mut pending, &tx).await;

		// Chunk 3: more arguments.
		let chunk3 = vec![serde_json::json!({
			"index": 0,
			"function": {"arguments": "example.com\"}"}
		})];
		process_streaming_tool_calls(&chunk3, &mut pending, &tx).await;

		drop(tx);

		// Verify: ToolCallStart, then two ToolCallDeltas.
		let msg1 = rx.recv().await.unwrap();
		assert!(
			matches!(msg1, StreamChunk::ToolCallStart { ref id, ref name } if id == "call_abc" && name == "web_fetch")
		);

		let msg2 = rx.recv().await.unwrap();
		assert!(
			matches!(msg2, StreamChunk::ToolCallDelta { ref arguments_chunk, .. } if arguments_chunk == "{\"url\":\"https://")
		);

		let msg3 = rx.recv().await.unwrap();
		assert!(
			matches!(msg3, StreamChunk::ToolCallDelta { ref arguments_chunk, .. } if arguments_chunk == "example.com\"}")
		);

		// Verify accumulated arguments.
		assert_eq!(pending.len(), 1);
		let entry = pending.get(&0).unwrap();
		assert_eq!(entry.arguments, "{\"url\":\"https://example.com\"}");
		assert!(entry.started);
	}

	#[tokio::test]
	async fn streaming_single_chunk_tool_call() {
		let (tx, mut rx) = mpsc::channel::<StreamChunk>(32);
		let mut pending = HashMap::new();

		// Some providers send id + name + arguments in a single chunk.
		let chunk = vec![serde_json::json!({
			"index": 0,
			"id": "call_xyz",
			"function": {"name": "read_file", "arguments": "{\"path\":\"/tmp/test\"}"}
		})];
		process_streaming_tool_calls(&chunk, &mut pending, &tx).await;

		drop(tx);

		// Should get ToolCallStart then ToolCallDelta in order.
		let msg1 = rx.recv().await.unwrap();
		assert!(matches!(msg1, StreamChunk::ToolCallStart { ref name, .. } if name == "read_file"));

		let msg2 = rx.recv().await.unwrap();
		assert!(
			matches!(msg2, StreamChunk::ToolCallDelta { ref arguments_chunk, .. } if arguments_chunk.contains("/tmp/test"))
		);
	}
}
