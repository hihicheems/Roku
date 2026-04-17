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

//! OpenAI Responses API provider — distinct from the Chat Completions provider.
//!
//! This provider targets `POST /v1/responses` (the OpenAI Responses API), which
//! uses a fundamentally different wire format from `/v1/chat/completions`:
//!
//! - **Request**: `input[]` items with explicit `type` tags (`message`,
//!   `function_call`, `function_call_output`), plus a top-level `instructions`
//!   string instead of a system message in the array.
//! - **Streaming**: SSE events named `response.*` (e.g.
//!   `response.output_text.delta`, `response.output_item.added`).
//! - **Non-streaming**: a JSON object with an `output[]` array instead of
//!   `choices[]`.
//!
//! Adapted from the Rara Codex driver (`draft/rara/crates/kernel/src/llm/codex.rs`).
//! NO imports from `super::openai` — this is a fully independent provider.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use reqwest::Client;
use reqwest::header::{HeaderMap, HeaderValue};
use roku_common_types::{LogLevel, LogRecord, Metrics, emit_global_log};
use serde_json::{Value, json};
use thiserror::Error;
use tokio::sync::mpsc;

use crate::providers::openai_ws::{OpenAiWsSession, SharedWsSession};
use crate::router::{LlmProvider, LlmRouter};
use crate::types::{
	CompactRequest, CompactResponse, CompactUsageSummary, GenerationRequest, Message, ModelProfile,
	ProviderCallError, ProviderResponse, RiskTier, RoutingPolicy, StreamChunk, ToolCallBlock,
	ToolDefinition, estimate_prompt_tokens,
};

const OPENAI_RESPONSES_PROVIDER: &str = "openai_responses";
/// ChatGPT backend Responses API — OAuth tokens only work here, not on the
/// public `api.openai.com/v1/responses` endpoint (which requires
/// `api.responses.write` scope that the PKCE flow does not grant).
const DEFAULT_RESPONSES_URL: &str = "https://chatgpt.com/backend-api/codex/responses";

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// Configuration for the OpenAI Responses API provider.
#[derive(Debug, Clone)]
pub struct OpenAiResponsesConfig {
	pub api_key: String,
	/// Default: `https://api.openai.com/v1/responses`
	pub base_url: String,
	/// Optional reasoning effort (`low`, `medium`, `high`).
	pub reasoning_effort: Option<String>,
	/// Enable delta mode: include `previous_response_id` on subsequent turns
	/// to reduce upstream payload size. Controlled by `ROKU_OPENAI_WEBSOCKET_MODE`.
	/// Default: `false`.
	pub websocket_mode: bool,
}

impl OpenAiResponsesConfig {
	pub fn new(api_key: String) -> Self {
		let websocket_mode = std::env::var("ROKU_OPENAI_WEBSOCKET_MODE")
			.map(|v| v.eq_ignore_ascii_case("true") || v == "1")
			.unwrap_or(false);
		Self {
			api_key,
			base_url: DEFAULT_RESPONSES_URL.to_string(),
			reasoning_effort: None,
			websocket_mode,
		}
	}
}

// ---------------------------------------------------------------------------
// Bootstrap error
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum OpenAiResponsesBootstrapError {
	#[error("failed to construct http client: {0}")]
	HttpClient(#[from] reqwest::Error),
	#[error("provider bootstrap failed: {0}")]
	Provider(String),
}

impl From<ProviderCallError> for OpenAiResponsesBootstrapError {
	fn from(error: ProviderCallError) -> Self {
		OpenAiResponsesBootstrapError::Provider(error.to_string())
	}
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

/// Construct a live [`LlmRouter`] backed by the OpenAI Responses API.
///
/// Reuses [`super::openai::OpenAiRuntimeConfig`] for model names and routing
/// limits — that struct is a plain data container and does not carry any
/// Chat Completions wire-format logic.
pub fn build_openai_responses_router_with_metrics(
	config: OpenAiResponsesConfig,
	runtime_config: super::openai::OpenAiRuntimeConfig,
	metrics: Arc<Metrics>,
) -> Result<LlmRouter, OpenAiResponsesBootstrapError> {
	let mut router = LlmRouter::new(RoutingPolicy {
		max_request_cost_usd: runtime_config.max_request_cost_usd,
		max_latency_ms: runtime_config.max_latency_ms,
	})
	.with_metrics(metrics);

	router.register_provider(OpenAiResponsesProvider::new(config)?);

	let mut model_chain =
		Vec::with_capacity(runtime_config.fallback_models.len().saturating_add(1));
	model_chain.push(runtime_config.primary_model.clone());
	model_chain.extend(runtime_config.fallback_models.iter().cloned());

	for (index, model_id) in model_chain.into_iter().enumerate() {
		router.register_model(ModelProfile {
			model_id,
			provider: OPENAI_RESPONSES_PROVIDER.to_string(),
			max_context_tokens: runtime_config.max_context_tokens,
			cost_per_1k_tokens_usd: runtime_config.cost_per_1k_tokens_usd,
			max_risk_tier: RiskTier::Critical,
			route_priority: 100u8.saturating_sub(u8::try_from(index).unwrap_or(u8::MAX)),
		});
	}

	Ok(router)
}

// ---------------------------------------------------------------------------
// Provider struct
// ---------------------------------------------------------------------------

/// LLM provider that calls the OpenAI Responses API (`POST /v1/responses`).
pub struct OpenAiResponsesProvider {
	client: Client,
	config: OpenAiResponsesConfig,
	/// Session-stable key sent as `prompt_cache_key`. Computed once at
	/// provider construction time so every request in the same session
	/// carries the same key (prerequisite for cache hits on turn 2+).
	prompt_cache_key: String,
	/// Per-session delta state. Tracks `previous_response_id` across turns
	/// when `config.websocket_mode` is enabled.
	ws_session: SharedWsSession,
	/// Timeout applied to the compact endpoint request. Defaults to
	/// [`COMPACT_REQUEST_TIMEOUT`] (90s). Overridable in tests via
	/// [`OpenAiResponsesProvider::with_compact_timeout`].
	compact_timeout: std::time::Duration,
}

impl OpenAiResponsesProvider {
	pub fn new(config: OpenAiResponsesConfig) -> Result<Self, ProviderCallError> {
		let client = Client::builder()
			.connect_timeout(std::time::Duration::from_secs(30))
			.build()
			.map_err(|e| ProviderCallError::Fatal {
				message: format!("http client error: {e}"),
			})?;
		let prompt_cache_key = derive_session_prompt_cache_key();
		let ws_session = std::sync::Arc::new(tokio::sync::Mutex::new(OpenAiWsSession::new(
			config.websocket_mode,
		)));
		Ok(Self {
			client,
			config,
			prompt_cache_key,
			ws_session,
			compact_timeout: COMPACT_REQUEST_TIMEOUT,
		})
	}

	/// Construct a provider with a custom compact-endpoint timeout.
	///
	/// Intended for unit tests only: allows tests to inject a very short
	/// timeout so the compact timeout path fires quickly without waiting
	/// for the production 90s constant.
	#[cfg(test)]
	pub(crate) fn with_compact_timeout(
		client: Client,
		config: OpenAiResponsesConfig,
		compact_timeout: std::time::Duration,
	) -> Self {
		let prompt_cache_key = derive_session_prompt_cache_key();
		let ws_session = std::sync::Arc::new(tokio::sync::Mutex::new(OpenAiWsSession::new(
			config.websocket_mode,
		)));
		Self {
			client,
			config,
			prompt_cache_key,
			ws_session,
			compact_timeout,
		}
	}

	fn build_headers(&self) -> Result<HeaderMap, ProviderCallError> {
		let mut headers = HeaderMap::new();
		headers.insert(
			reqwest::header::AUTHORIZATION,
			HeaderValue::from_str(&format!("Bearer {}", self.config.api_key)).map_err(|e| {
				ProviderCallError::Fatal {
					message: format!("invalid authorization header: {e}"),
				}
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
// Prompt cache key derivation
// ---------------------------------------------------------------------------

/// Monotonic per-process counter that disambiguates two provider instances
/// constructed within the same nanosecond tick (e.g. reconnect after init
/// failure). Combined with `pid + nanos` it makes `prompt_cache_key`
/// collision-free across restarts even on OSes that recycle pids quickly.
static PROMPT_CACHE_KEY_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Derive a session-stable opaque key for `prompt_cache_key`.
///
/// OpenAI's automatic prefix caching partitions cache entries by
/// `prompt_cache_key`, so reusing the same key across turns within a session
/// is what produces cache hits on turn 2+. The key's value is opaque to the
/// server — only its stability matters.
///
/// We combine `pid`, construction time, and a monotonic counter so two
/// provider instances constructed back-to-back (or after pid reuse) cannot
/// share a key and accidentally read each other's cached prefix.
fn derive_session_prompt_cache_key() -> String {
	let pid = std::process::id();
	let nanos = SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.map(|d| d.as_nanos())
		.unwrap_or(0);
	let counter = PROMPT_CACHE_KEY_COUNTER.fetch_add(1, Ordering::Relaxed);
	format!("roku-{pid:x}-{nanos:x}-{counter:x}")
}

// ---------------------------------------------------------------------------
// Request building
// ---------------------------------------------------------------------------

/// Convert a [`GenerationRequest`] into the Responses API `input[]` format.
///
/// The Responses API does not accept system messages in `input[]`; they must
/// appear as a top-level `instructions` string. Tool results become
/// `function_call_output` items. Assistant messages with tool calls emit
/// separate `function_call` items for each call.
///
/// When `previous_response_id` is `Some`, it is added to the request body to
/// enable delta mode: the server can skip reprocessing already-seen context.
fn build_responses_request(
	model_id: &str,
	request: &GenerationRequest,
	stream: bool,
	reasoning_effort: Option<&str>,
	prompt_cache_key: &str,
	previous_response_id: Option<&str>,
) -> Value {
	let mut input: Vec<Value> = Vec::new();

	// System prompt → top-level `instructions`, never goes into input[].
	let instructions = request.system_prompt.as_deref().unwrap_or("").to_string();

	// Convert messages to Responses API input items.
	if let Some(messages) = &request.messages {
		for msg in messages {
			match msg {
				Message::User { content } => {
					input.push(json!({
						"type": "message",
						"role": "user",
						"content": [{"type": "input_text", "text": content}],
					}));
				}
				Message::Assistant { text, tool_calls } => {
					// If there is text content, emit a message item first.
					if !text.is_empty() {
						input.push(json!({
							"type": "message",
							"role": "assistant",
							"content": [{"type": "output_text", "text": text}],
						}));
					}
					// Each tool call becomes a separate function_call item.
					for tc in tool_calls {
						input.push(json!({
							"type": "function_call",
							"name": tc.name,
							"arguments": tc.arguments.to_string(),
							"call_id": tc.id,
						}));
					}
				}
				Message::ToolResult {
					tool_use_id,
					content,
					..
				} => {
					input.push(json!({
						"type": "function_call_output",
						"call_id": tool_use_id,
						"output": content,
					}));
				}
			}
		}
	} else {
		// Fallback: wrap the single `prompt` field as a user message.
		input.push(json!({
			"type": "message",
			"role": "user",
			"content": [{"type": "input_text", "text": &request.prompt}],
		}));
	}

	// Tools array — same shape as Chat Completions.
	let tools_value: Option<Vec<Value>> = request.tools.as_ref().map(|tool_defs| {
		tool_defs
			.iter()
			.map(build_tool_definition)
			.collect::<Vec<_>>()
	});

	let mut body = json!({
		"model": model_id,
		"instructions": instructions,
		"input": input,
		"stream": stream,
		"store": false,
		"prompt_cache_key": prompt_cache_key,
	});

	if let Some(tools) = tools_value.filter(|t| !t.is_empty()) {
		body["tools"] = json!(tools);
	}

	// Delta mode: include the prior response ID so the server can skip
	// reprocessing already-processed context from the previous turn.
	if let Some(prev_id) = previous_response_id {
		body["previous_response_id"] = serde_json::json!(prev_id);
	}

	// Reasoning config — only attach when requested.
	if let Some(effort) = reasoning_effort {
		body["reasoning"] = json!({
			"effort": effort,
			"summary": "auto",
		});
	}

	// For reasoning-capable models, request that encrypted reasoning content
	// is included in the response so it can be passed back in subsequent turns,
	// enabling the provider to reuse prior reasoning traces.
	if crate::model_cost::is_reasoning_model(model_id) {
		body["include"] = json!(["reasoning.encrypted_content"]);
	}

	body
}

fn build_tool_definition(tool: &ToolDefinition) -> Value {
	json!({
		"type": "function",
		"name": tool.name,
		"description": tool.description,
		"parameters": tool.parameters,
	})
}

// ---------------------------------------------------------------------------
// Non-streaming response parsing
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// SSE state accumulator
// ---------------------------------------------------------------------------

struct SseStreamState {
	/// Maps `output_index` (from SSE events) to the `call_id` of the tool call
	/// at that position. Populated on `response.output_item.added` so that
	/// `response.function_call_arguments.delta` can correlate by output_index.
	output_index_to_call_id: HashMap<u64, String>,
	/// Pending tool calls accumulated during streaming, keyed by call_id.
	pending_tools: HashMap<String, PendingResponsesToolCall>,
	full_text: String,
	finish_reason: Option<String>,
	prompt_tokens: u64,
	output_tokens: u64,
	cache_read_input_tokens: u64,
	has_function_call: bool,
	/// Set when an `error` or `response.failed` SSE event is received.
	stream_error: Option<String>,
	/// The response `id` from the server, captured from `response.created` or
	/// `response.completed` for delta mode (`previous_response_id` next turn).
	response_id: Option<String>,
}

struct PendingResponsesToolCall {
	name: String,
	arguments: String,
	/// Whether `ToolCallStart` has been emitted.
	started: bool,
}

impl SseStreamState {
	fn new() -> Self {
		Self {
			output_index_to_call_id: HashMap::new(),
			pending_tools: HashMap::new(),
			full_text: String::new(),
			finish_reason: None,
			prompt_tokens: 0,
			output_tokens: 0,
			cache_read_input_tokens: 0,
			has_function_call: false,
			stream_error: None,
			response_id: None,
		}
	}
}

// ---------------------------------------------------------------------------
// SSE event dispatch
// ---------------------------------------------------------------------------

/// Process a single Responses API SSE event.
///
/// Returns `true` when the stream should terminate (the response has completed
/// or failed), `false` otherwise.
async fn handle_sse_event(
	event_type: &str,
	data: &str,
	tx: &mpsc::Sender<StreamChunk>,
	state: &mut SseStreamState,
) -> bool {
	let parsed: Value = match serde_json::from_str(data) {
		Ok(v) => v,
		Err(_) => return false,
	};

	match event_type {
		// --- Text output ---
		"response.output_text.delta" => {
			if let Some(delta) = parsed.get("delta").and_then(Value::as_str) {
				state.full_text.push_str(delta);
				let _ = tx
					.send(StreamChunk::TextDelta {
						text: delta.to_string(),
					})
					.await;
			}
		}

		// --- Tool call lifecycle ---
		"response.output_item.added" => {
			if let Some(item) = parsed.get("item") {
				let item_type = item.get("type").and_then(Value::as_str).unwrap_or("");
				if item_type == "function_call" {
					let output_index = parsed
						.get("output_index")
						.and_then(Value::as_u64)
						.unwrap_or(0);
					let call_id = item
						.get("call_id")
						.and_then(Value::as_str)
						.unwrap_or("")
						.to_string();
					let name = item
						.get("name")
						.and_then(Value::as_str)
						.unwrap_or("")
						.to_string();

					if !call_id.is_empty() {
						// Register the output_index → call_id mapping so that
						// argument delta events can look up the call_id.
						state
							.output_index_to_call_id
							.insert(output_index, call_id.clone());

						let started = !name.is_empty();
						if started {
							let _ = tx
								.send(StreamChunk::ToolCallStart {
									id: call_id.clone(),
									name: name.clone(),
								})
								.await;
						}
						state.pending_tools.insert(
							call_id,
							PendingResponsesToolCall {
								name,
								arguments: String::new(),
								started,
							},
						);
					}
				}
			}
		}

		"response.function_call_arguments.delta" => {
			if let Some(delta) = parsed.get("delta").and_then(Value::as_str) {
				let output_index = parsed
					.get("output_index")
					.and_then(Value::as_u64)
					.unwrap_or(0);
				// Look up the call_id registered for this output_index.
				if let Some(cid) = state.output_index_to_call_id.get(&output_index).cloned()
					&& let Some(pending) = state.pending_tools.get_mut(&cid)
				{
					pending.arguments.push_str(delta);
					if pending.started {
						let _ = tx
							.send(StreamChunk::ToolCallDelta {
								id: cid.clone(),
								arguments_chunk: delta.to_string(),
							})
							.await;
					}
				}
			}
		}

		"response.output_item.done" => {
			if let Some(item) = parsed.get("item") {
				let item_type = item.get("type").and_then(Value::as_str).unwrap_or("");
				if item_type == "function_call" {
					state.has_function_call = true;
					let call_id = item
						.get("call_id")
						.and_then(Value::as_str)
						.unwrap_or("")
						.to_string();
					// Only emit ToolCallDone if ToolCallStart was sent.
					let was_started = state
						.pending_tools
						.get(&call_id)
						.map(|p| p.started)
						.unwrap_or(false);
					if !call_id.is_empty() && was_started {
						let _ = tx
							.send(StreamChunk::ToolCallDone {
								id: call_id.clone(),
							})
							.await;
					}
				}
			}
		}

		// --- Response lifecycle ---
		"response.completed" => {
			// Capture response ID as fallback (in case response.created was missed).
			if state.response_id.is_none()
				&& let Some(id) = parsed
					.get("response")
					.and_then(|r| r.get("id"))
					.and_then(Value::as_str)
			{
				state.response_id = Some(id.to_string());
			}

			// Extract usage.
			if let Some(usage) = parsed.get("response").and_then(|r| r.get("usage")) {
				state.prompt_tokens = usage
					.get("input_tokens")
					.and_then(Value::as_u64)
					.unwrap_or(state.prompt_tokens);
				state.output_tokens = usage
					.get("output_tokens")
					.and_then(Value::as_u64)
					.unwrap_or(state.output_tokens);
				state.cache_read_input_tokens = usage
					.get("input_tokens_details")
					.and_then(|d| d.get("cached_tokens"))
					.and_then(Value::as_u64)
					.unwrap_or(state.cache_read_input_tokens);
			}

			// Fallback: if no text was received via `response.output_text.delta`
			// events during streaming, extract it from the completed response's
			// `output[]` array. Some API responses deliver text only here.
			if state.full_text.is_empty()
				&& let Some(outputs) = parsed
					.get("response")
					.and_then(|r| r.get("output"))
					.and_then(Value::as_array)
			{
				for item in outputs {
					if item.get("type").and_then(Value::as_str) == Some("message")
						&& let Some(content) = item.get("content").and_then(Value::as_array)
					{
						for part in content {
							if part.get("type").and_then(Value::as_str) == Some("output_text")
								&& let Some(text) = part.get("text").and_then(Value::as_str)
							{
								state.full_text.push_str(text);
								let _ = tx
									.send(StreamChunk::TextDelta {
										text: text.to_string(),
									})
									.await;
							}
						}
					}
				}
			}

			state.finish_reason = Some(if state.has_function_call {
				"tool_calls".to_string()
			} else {
				"stop".to_string()
			});
			return true;
		}

		"response.incomplete" => {
			if let Some(usage) = parsed.get("response").and_then(|r| r.get("usage")) {
				state.prompt_tokens = usage
					.get("input_tokens")
					.and_then(Value::as_u64)
					.unwrap_or(state.prompt_tokens);
				state.output_tokens = usage
					.get("output_tokens")
					.and_then(Value::as_u64)
					.unwrap_or(state.output_tokens);
				state.cache_read_input_tokens = usage
					.get("input_tokens_details")
					.and_then(|d| d.get("cached_tokens"))
					.and_then(Value::as_u64)
					.unwrap_or(state.cache_read_input_tokens);
			}
			let reason = parsed
				.get("response")
				.and_then(|r| r.get("incomplete_details"))
				.and_then(|d| d.get("reason"))
				.and_then(Value::as_str)
				.unwrap_or("incomplete");
			state.finish_reason = Some(reason.to_string());
			return true;
		}

		"response.created" => {
			// Capture the response ID for delta mode and log for observability.
			if let Some(id) = parsed
				.get("response")
				.and_then(|r| r.get("id"))
				.and_then(Value::as_str)
			{
				state.response_id = Some(id.to_string());
				log_responses(
					LogLevel::Debug,
					"response created",
					[("response_id", id.to_string())],
				);
			}
		}

		"error" => {
			let message = parsed
				.get("message")
				.and_then(Value::as_str)
				.unwrap_or("unknown error");
			log_responses(
				LogLevel::Warn,
				"responses api error event",
				[("message", message.to_string())],
			);
			state.stream_error = Some(message.to_string());
			return true;
		}

		"response.failed" => {
			let reason = parsed
				.get("response")
				.and_then(|r| r.get("error"))
				.and_then(|e| e.get("message"))
				.and_then(Value::as_str)
				.unwrap_or("response failed");
			log_responses(
				LogLevel::Warn,
				"responses api failed",
				[("reason", reason.to_string())],
			);
			state.stream_error = Some(reason.to_string());
			return true;
		}

		_ => {
			// Unknown or unhandled event — safe to ignore.
		}
	}

	false
}

// ---------------------------------------------------------------------------
// Compact endpoint helpers
// ---------------------------------------------------------------------------

/// Derive the compact endpoint URL from the base responses URL.
///
/// The base URL is e.g. `https://chatgpt.com/backend-api/codex/responses`.
/// The compact endpoint is at the same base path with `/compact` appended,
/// i.e. `https://chatgpt.com/backend-api/codex/responses/compact`.
fn compact_url(base_url: &str) -> String {
	// Strip trailing slash then append /compact.
	let trimmed = base_url.trim_end_matches('/');
	format!("{trimmed}/compact")
}

/// Timeout for compact endpoint calls. Shorter than the normal 300s adapter
/// default so that a hung compact call falls back to mechanical summarization
/// quickly.
pub(crate) const COMPACT_REQUEST_TIMEOUT: std::time::Duration =
	std::time::Duration::from_secs(90);

// ---------------------------------------------------------------------------
// LlmProvider implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl LlmProvider for OpenAiResponsesProvider {
	fn provider_name(&self) -> &'static str {
		OPENAI_RESPONSES_PROVIDER
	}

	async fn complete(
		&self,
		model: &ModelProfile,
		request: &GenerationRequest,
	) -> Result<ProviderResponse, ProviderCallError> {
		// The ChatGPT backend requires `stream: true` — it returns 400 for
		// non-streaming requests. Delegate to stream() with a local channel
		// and discard the stream chunks (the ProviderResponse carries the
		// accumulated output).
		let (tx, mut rx) = mpsc::channel::<StreamChunk>(64);
		let result = self.stream(model, request, tx).await;
		// Drain channel to avoid blocking the sender if it hasn't finished.
		while rx.recv().await.is_some() {}
		result
	}

	async fn stream(
		&self,
		model: &ModelProfile,
		request: &GenerationRequest,
		tx: mpsc::Sender<StreamChunk>,
	) -> Result<ProviderResponse, ProviderCallError> {
		let headers = self.build_headers()?;

		// Read delta session state before building the request body.
		let (previous_response_id, delta_reuse_count) = {
			let session = self.ws_session.lock().await;
			if session.is_enabled() {
				(
					session.previous_response_id().map(str::to_owned),
					session.reuse_count(),
				)
			} else {
				(None, 0)
			}
		};

		// Always stream=true — ChatGPT backend requires it.
		let body = build_responses_request(
			&model.model_id,
			request,
			true,
			self.config.reasoning_effort.as_deref(),
			&self.prompt_cache_key,
			previous_response_id.as_deref(),
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
			log_responses(
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
		let mut state = SseStreamState::new();
		let mut stream_error: Option<ProviderCallError> = None;

		// Per-event timeout: if no SSE event arrives within this duration,
		// treat the stream as stalled and abort (prevents indefinite hangs
		// after context compaction or server-side failures).
		let event_timeout = std::time::Duration::from_secs(120);

		loop {
			let event_result = match tokio::time::timeout(event_timeout, event_stream.next()).await
			{
				Ok(result) => result,
				Err(_) => {
					stream_error = Some(ProviderCallError::Timeout {
						message: "SSE stream timed out waiting for next event".to_string(),
					});
					break;
				}
			};
			match event_result {
				Some(Ok(event)) => {
					if event.data == "[DONE]" {
						break;
					}
					let terminal =
						handle_sse_event(&event.event, &event.data, &tx, &mut state).await;
					if terminal {
						break;
					}
				}
				Some(Err(e)) => {
					stream_error = Some(ProviderCallError::ConnectionFailed {
						message: format!("SSE stream error: {e}"),
					});
					break;
				}
				None => break,
			}
		}

		// Propagate API-level errors from SSE events.
		if stream_error.is_none()
			&& let Some(msg) = state.stream_error.take()
		{
			stream_error = Some(ProviderCallError::Fatal {
				message: format!("responses api: {msg}"),
			});
		}

		let latency_ms = u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX);

		// Collect completed tool calls from pending state.
		let mut completed_tools: Vec<ToolCallBlock> = Vec::new();
		for (call_id, pending) in state.pending_tools.drain() {
			let arguments =
				serde_json::from_str::<Value>(&pending.arguments).unwrap_or(Value::Null);
			completed_tools.push(ToolCallBlock {
				id: call_id,
				name: pending.name,
				arguments,
			});
		}

		if state.prompt_tokens == 0 {
			state.prompt_tokens = estimate_prompt_tokens(&state.full_text);
		}

		let tool_calls = if completed_tools.is_empty() {
			None
		} else {
			Some(completed_tools)
		};

		let _ = tx
			.send(StreamChunk::Done {
				finish_reason: state.finish_reason.clone(),
				prompt_tokens: state.prompt_tokens,
				output_tokens: state.output_tokens,
			})
			.await;

		if let Some(error) = stream_error {
			// On failure, reset delta session state so the next turn starts
			// fresh rather than sending a stale previous_response_id.
			if self.config.websocket_mode {
				let mut session = self.ws_session.lock().await;
				session.reset();
			}
			return Err(error);
		}

		// Update delta session state on success.
		let response_id = state.response_id.clone();
		if self.config.websocket_mode
			&& let Some(ref rid) = response_id
		{
			let mut session = self.ws_session.lock().await;
			session.record_response(rid.clone());
		}

		log_responses(
			LogLevel::Info,
			"provider streaming request completed",
			[
				("model", model.model_id.clone()),
				("status", "ok".to_string()),
				("latency_ms", latency_ms.to_string()),
				("prompt_tokens", state.prompt_tokens.to_string()),
				("output_tokens", state.output_tokens.to_string()),
				(
					"delta_reuse_count",
					if self.config.websocket_mode {
						delta_reuse_count.to_string()
					} else {
						"disabled".to_string()
					},
				),
			],
		);

		Ok(ProviderResponse {
			output: state.full_text,
			finish_reason: state.finish_reason,
			prompt_tokens: state.prompt_tokens,
			output_tokens: state.output_tokens,
			cache_creation_input_tokens: 0,
			cache_read_input_tokens: state.cache_read_input_tokens,
			latency_ms,
			tool_calls,
			response_id,
		})
	}

	fn supports_compact_history(&self) -> bool {
		true
	}

	fn supports_output_slot_cap(&self) -> bool {
		false
	}

	async fn compact_history(
		&self,
		request: &CompactRequest,
	) -> Option<Result<CompactResponse, ProviderCallError>> {
		let url = compact_url(&self.config.base_url);
		let headers = match self.build_headers() {
			Ok(h) => h,
			Err(e) => return Some(Err(e)),
		};

		// Convert internal Message format to Responses API input items.
		let input_items: Vec<Value> = messages_to_responses_input(&request.input);

		// Build the compact request body. Tool choice is always "auto" for compaction.
		let tools_value: Vec<Value> = request.tools.iter().map(build_tool_definition).collect();

		let mut body = json!({
			"model": request.model,
			"instructions": request.instructions,
			"input": input_items,
			"tool_choice": "auto",
			"parallel_tool_calls": request.parallel_tool_calls,
		});
		if !tools_value.is_empty() {
			body["tools"] = json!(tools_value);
		}
		if let Some(reasoning) = &request.reasoning {
			body["reasoning"] = reasoning.clone();
		}

		let http_result = tokio::time::timeout(
			self.compact_timeout,
			self.client
				.post(&url)
				.headers(headers)
				.timeout(self.compact_timeout)
				.json(&body)
				.send(),
		)
		.await;

		let http_response = match http_result {
			Ok(Ok(resp)) => resp,
			Ok(Err(e)) => return Some(Err(classify_request_error(e))),
			Err(_) => {
				return Some(Err(ProviderCallError::Timeout {
					message: "compact request timed out after 90s".to_string(),
				}));
			}
		};

		let status = http_response.status();
		if !status.is_success() {
			let body_text = http_response.text().await.unwrap_or_default();
			log_responses(
				LogLevel::Warn,
				"compact endpoint returned non-success status",
				[
					("url", url),
					("status", status.to_string()),
					("body", truncate_for_log(&body_text, 400)),
				],
			);
			return Some(Err(classify_status_error(status.as_u16(), body_text)));
		}

		let body_text = match http_response.text().await {
			Ok(t) => t,
			Err(e) => {
				return Some(Err(ProviderCallError::Fatal {
					message: format!("compact response body read error: {e}"),
				}));
			}
		};

		let parsed: Value = match serde_json::from_str(&body_text) {
			Ok(v) => v,
			Err(e) => {
				return Some(Err(ProviderCallError::Fatal {
					message: format!("compact response JSON parse error: {e}"),
				}));
			}
		};

		let raw_output = match parsed.get("output").and_then(|v| v.as_array()) {
			Some(arr) => arr.clone(),
			None => {
				return Some(Err(ProviderCallError::Fatal {
					message: "compact response missing 'output' field".to_string(),
				}));
			}
		};

		// Convert Responses API output items back to internal Message format.
		let output = responses_output_to_messages(&raw_output);

		let usage = parsed
			.get("usage")
			.map(|u| CompactUsageSummary {
				prompt_tokens: u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
				output_tokens: u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
				cached_input_tokens: u
					.get("input_tokens_details")
					.and_then(|d| d.get("cached_tokens"))
					.and_then(|v| v.as_u64())
					.unwrap_or(0),
			})
			.unwrap_or_default();

		Some(Ok(CompactResponse { output, usage }))
	}
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Convert internal [`Message`] slice to Responses API `input[]` wire format.
///
/// This mirrors the conversion in `build_responses_request` but operates on
/// a raw slice of messages rather than a full `GenerationRequest`.
fn messages_to_responses_input(messages: &[Message]) -> Vec<Value> {
	let mut input: Vec<Value> = Vec::new();
	for msg in messages {
		match msg {
			Message::User { content } => {
				input.push(json!({
					"type": "message",
					"role": "user",
					"content": [{"type": "input_text", "text": content}],
				}));
			}
			Message::Assistant { text, tool_calls } => {
				if !text.is_empty() {
					input.push(json!({
						"type": "message",
						"role": "assistant",
						"content": [{"type": "output_text", "text": text}],
					}));
				}
				for tc in tool_calls {
					input.push(json!({
						"type": "function_call",
						"name": tc.name,
						"arguments": tc.arguments.to_string(),
						"call_id": tc.id,
					}));
				}
			}
			Message::ToolResult {
				tool_use_id,
				content,
				..
			} => {
				input.push(json!({
					"type": "function_call_output",
					"call_id": tool_use_id,
					"output": content,
				}));
			}
		}
	}
	input
}

/// Convert Responses API `output[]` wire items back to internal [`Message`] format.
///
/// Items not recognised as a known type are silently skipped. Tool calls that
/// share a `call_id` are grouped into a single `Message::Assistant` entry.
fn responses_output_to_messages(items: &[Value]) -> Vec<Message> {
	let mut messages: Vec<Message> = Vec::new();

	for item in items {
		let item_type = item.get("type").and_then(Value::as_str).unwrap_or("");
		match item_type {
			"message" => {
				let role = item.get("role").and_then(Value::as_str).unwrap_or("user");
				let mut text = String::new();
				if let Some(content) = item.get("content").and_then(Value::as_array) {
					for part in content {
						let part_type = part.get("type").and_then(Value::as_str).unwrap_or("");
						if matches!(part_type, "output_text" | "input_text" | "text")
							&& let Some(t) = part.get("text").and_then(Value::as_str)
						{
							text.push_str(t);
						}
					}
				}
				if role == "assistant" {
					messages.push(Message::Assistant {
						text,
						tool_calls: vec![],
					});
				} else {
					messages.push(Message::User { content: text });
				}
			}
			"function_call" => {
				let call_id = item
					.get("call_id")
					.and_then(Value::as_str)
					.unwrap_or("")
					.to_string();
				let name = item
					.get("name")
					.and_then(Value::as_str)
					.unwrap_or("")
					.to_string();
				let arguments_str = item
					.get("arguments")
					.and_then(Value::as_str)
					.unwrap_or("{}");
				let arguments = serde_json::from_str::<Value>(arguments_str).unwrap_or(Value::Null);
				// Append to the last assistant message if there is one, otherwise
				// create a new empty assistant message to hold this tool call.
				let tc = crate::types::ToolCallBlock {
					id: call_id,
					name,
					arguments,
				};
				match messages.last_mut() {
					Some(Message::Assistant { tool_calls, .. }) => {
						tool_calls.push(tc);
					}
					_ => {
						messages.push(Message::Assistant {
							text: String::new(),
							tool_calls: vec![tc],
						});
					}
				}
			}
			"function_call_output" => {
				let call_id = item
					.get("call_id")
					.and_then(Value::as_str)
					.unwrap_or("")
					.to_string();
				let content = item
					.get("output")
					.and_then(Value::as_str)
					.unwrap_or("")
					.to_string();
				messages.push(Message::ToolResult {
					tool_use_id: call_id,
					content,
					is_error: false,
				});
			}
			_ => {
				// Unknown item type — skip.
			}
		}
	}

	messages
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
	let message = format!("openai responses returned status {status_code}: {response_body}");
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

fn log_responses(
	level: LogLevel,
	message: &str,
	fields: impl IntoIterator<Item = (&'static str, String)>,
) {
	let record = fields.into_iter().fold(
		LogRecord::new("roku-plugin-llm", level, message)
			.with_field("provider", OPENAI_RESPONSES_PROVIDER),
		|record, (key, value)| record.with_field(key, value),
	);
	let _ = emit_global_log(record);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
	use super::*;
	use serde_json::json;

	// --- Request building ---

	#[test]
	fn user_message_becomes_input_text_item() {
		let request = GenerationRequest {
			system_prompt: None,
			prompt: String::new(),
			messages: Some(vec![Message::User {
				content: "hello world".to_string(),
			}]),
			expected_output_tokens: 100,
			risk_tier: RiskTier::Low,
			preferred_provider: None,
			budget_tokens_remaining: 100_000,
			budget_cost_remaining_usd: 10.0,
			tools: None,
			model_override: None,
			thinking_effort: None,
			system_prompt_sections: None,
		};

		let body =
			build_responses_request("gpt-4.1", &request, false, None, "test-cache-key", None);
		let input = body["input"].as_array().expect("input array");
		assert_eq!(input.len(), 1);
		assert_eq!(input[0]["type"], "message");
		assert_eq!(input[0]["role"], "user");
		let content = input[0]["content"].as_array().expect("content array");
		assert_eq!(content[0]["type"], "input_text");
		assert_eq!(content[0]["text"], "hello world");
	}

	#[test]
	fn assistant_message_becomes_output_text_item() {
		let request = GenerationRequest {
			system_prompt: None,
			prompt: String::new(),
			messages: Some(vec![Message::Assistant {
				text: "I can help".to_string(),
				tool_calls: vec![],
			}]),
			expected_output_tokens: 100,
			risk_tier: RiskTier::Low,
			preferred_provider: None,
			budget_tokens_remaining: 100_000,
			budget_cost_remaining_usd: 10.0,
			tools: None,
			model_override: None,
			thinking_effort: None,
			system_prompt_sections: None,
		};

		let body =
			build_responses_request("gpt-4.1", &request, false, None, "test-cache-key", None);
		let input = body["input"].as_array().expect("input array");
		assert_eq!(input.len(), 1);
		assert_eq!(input[0]["type"], "message");
		let content = input[0]["content"].as_array().expect("content array");
		assert_eq!(content[0]["type"], "output_text");
		assert_eq!(content[0]["text"], "I can help");
	}

	#[test]
	fn assistant_with_tool_calls_emits_function_call_items() {
		let request = GenerationRequest {
			system_prompt: None,
			prompt: String::new(),
			messages: Some(vec![Message::Assistant {
				text: String::new(),
				tool_calls: vec![ToolCallBlock {
					id: "call_abc".to_string(),
					name: "read_file".to_string(),
					arguments: json!({"path": "/foo.rs"}),
				}],
			}]),
			expected_output_tokens: 100,
			risk_tier: RiskTier::Low,
			preferred_provider: None,
			budget_tokens_remaining: 100_000,
			budget_cost_remaining_usd: 10.0,
			tools: None,
			model_override: None,
			thinking_effort: None,
			system_prompt_sections: None,
		};

		let body =
			build_responses_request("gpt-4.1", &request, false, None, "test-cache-key", None);
		let input = body["input"].as_array().expect("input array");
		// text was empty so no message item, only function_call
		assert_eq!(input.len(), 1);
		assert_eq!(input[0]["type"], "function_call");
		assert_eq!(input[0]["name"], "read_file");
		assert_eq!(input[0]["call_id"], "call_abc");
	}

	#[test]
	fn tool_result_becomes_function_call_output() {
		let request = GenerationRequest {
			system_prompt: None,
			prompt: String::new(),
			messages: Some(vec![Message::ToolResult {
				tool_use_id: "call_xyz".to_string(),
				content: "file contents".to_string(),
				is_error: false,
			}]),
			expected_output_tokens: 100,
			risk_tier: RiskTier::Low,
			preferred_provider: None,
			budget_tokens_remaining: 100_000,
			budget_cost_remaining_usd: 10.0,
			tools: None,
			model_override: None,
			thinking_effort: None,
			system_prompt_sections: None,
		};

		let body =
			build_responses_request("gpt-4.1", &request, false, None, "test-cache-key", None);
		let input = body["input"].as_array().expect("input array");
		assert_eq!(input.len(), 1);
		assert_eq!(input[0]["type"], "function_call_output");
		assert_eq!(input[0]["call_id"], "call_xyz");
		assert_eq!(input[0]["output"], "file contents");
	}

	#[test]
	fn system_prompt_goes_to_instructions_not_input() {
		let request = GenerationRequest {
			system_prompt: Some("You are a helpful assistant.".to_string()),
			prompt: String::new(),
			messages: Some(vec![Message::User {
				content: "hi".to_string(),
			}]),
			expected_output_tokens: 100,
			risk_tier: RiskTier::Low,
			preferred_provider: None,
			budget_tokens_remaining: 100_000,
			budget_cost_remaining_usd: 10.0,
			tools: None,
			model_override: None,
			thinking_effort: None,
			system_prompt_sections: None,
		};

		let body =
			build_responses_request("gpt-4.1", &request, false, None, "test-cache-key", None);
		assert_eq!(body["instructions"], "You are a helpful assistant.");
		// input should only contain the user message, not the system prompt
		let input = body["input"].as_array().expect("input array");
		assert_eq!(input.len(), 1);
		assert_eq!(input[0]["role"], "user");
	}

	#[test]
	fn reasoning_effort_attached_when_provided() {
		let request = GenerationRequest {
			system_prompt: None,
			prompt: "test".to_string(),
			messages: None,
			expected_output_tokens: 100,
			risk_tier: RiskTier::Low,
			preferred_provider: None,
			budget_tokens_remaining: 100_000,
			budget_cost_remaining_usd: 10.0,
			tools: None,
			model_override: None,
			thinking_effort: None,
			system_prompt_sections: None,
		};

		let body = build_responses_request(
			"gpt-4.1",
			&request,
			false,
			Some("high"),
			"test-cache-key",
			None,
		);
		assert_eq!(body["reasoning"]["effort"], "high");
		assert_eq!(body["reasoning"]["summary"], "auto");
	}

	#[test]
	fn no_reasoning_when_effort_is_none() {
		let request = GenerationRequest {
			system_prompt: None,
			prompt: "test".to_string(),
			messages: None,
			expected_output_tokens: 100,
			risk_tier: RiskTier::Low,
			preferred_provider: None,
			budget_tokens_remaining: 100_000,
			budget_cost_remaining_usd: 10.0,
			tools: None,
			model_override: None,
			thinking_effort: None,
			system_prompt_sections: None,
		};

		let body =
			build_responses_request("gpt-4.1", &request, false, None, "test-cache-key", None);
		assert!(body.get("reasoning").is_none());
	}

	#[test]
	fn reasoning_model_includes_encrypted_content_field() {
		let request = GenerationRequest {
			system_prompt: None,
			prompt: "test".to_string(),
			messages: None,
			expected_output_tokens: 100,
			risk_tier: RiskTier::Low,
			preferred_provider: None,
			budget_tokens_remaining: 100_000,
			budget_cost_remaining_usd: 10.0,
			tools: None,
			model_override: None,
			thinking_effort: None,
			system_prompt_sections: None,
		};

		// o3-mini is a reasoning model — should include encrypted_content
		let body = build_responses_request(
			"o3-mini",
			&request,
			false,
			Some("medium"),
			"test-cache-key",
			None,
		);
		let include = body["include"].as_array().expect("include array");
		assert!(
			include
				.iter()
				.any(|v| v.as_str() == Some("reasoning.encrypted_content")),
			"reasoning model must request encrypted_content"
		);
	}

	#[test]
	fn non_reasoning_model_does_not_include_encrypted_content_field() {
		let request = GenerationRequest {
			system_prompt: None,
			prompt: "test".to_string(),
			messages: None,
			expected_output_tokens: 100,
			risk_tier: RiskTier::Low,
			preferred_provider: None,
			budget_tokens_remaining: 100_000,
			budget_cost_remaining_usd: 10.0,
			tools: None,
			model_override: None,
			thinking_effort: None,
			system_prompt_sections: None,
		};

		// gpt-4.1 is not a reasoning model — no include field
		let body =
			build_responses_request("gpt-4.1", &request, false, None, "test-cache-key", None);
		assert!(
			body.get("include").is_none(),
			"non-reasoning model must not have include field"
		);
	}

	#[test]
	fn tools_array_included_when_tools_present() {
		use crate::types::ToolDefinition;

		let request = GenerationRequest {
			system_prompt: None,
			prompt: String::new(),
			messages: None,
			expected_output_tokens: 100,
			risk_tier: RiskTier::Low,
			preferred_provider: None,
			budget_tokens_remaining: 100_000,
			budget_cost_remaining_usd: 10.0,
			tools: Some(vec![ToolDefinition {
				name: "search".to_string(),
				description: "Search the web".to_string(),
				parameters: json!({"type": "object"}),
			}]),
			model_override: None,
			thinking_effort: None,
			system_prompt_sections: None,
		};

		let body =
			build_responses_request("gpt-4.1", &request, false, None, "test-cache-key", None);
		let tools = body["tools"].as_array().expect("tools array");
		assert_eq!(tools.len(), 1);
		assert_eq!(tools[0]["type"], "function");
		assert_eq!(tools[0]["name"], "search");
	}

	// Non-streaming response parsing tests removed — complete() now delegates
	// to stream() because the ChatGPT backend requires stream=true.

	#[tokio::test]
	async fn response_completed_populates_cached_prompt_tokens() {
		let (tx, mut rx) = mpsc::channel::<StreamChunk>(8);
		let mut state = SseStreamState::new();
		let data = r#"{
			"response": {
				"output": [],
				"usage": {
					"input_tokens": 321,
					"output_tokens": 7,
					"input_tokens_details": {"cached_tokens": 200}
				}
			}
		}"#;
		let done = handle_sse_event("response.completed", data, &tx, &mut state).await;
		assert!(done);
		assert_eq!(state.prompt_tokens, 321);
		assert_eq!(state.cache_read_input_tokens, 200);
		// Drain any chunks the handler produced so the channel closes cleanly.
		drop(tx);
		while rx.recv().await.is_some() {}
	}

	#[test]
	fn build_request_attaches_prompt_cache_key() {
		let request = GenerationRequest {
			system_prompt: None,
			prompt: "hi".to_string(),
			messages: None,
			expected_output_tokens: 100,
			risk_tier: RiskTier::Low,
			preferred_provider: None,
			budget_tokens_remaining: 100_000,
			budget_cost_remaining_usd: 10.0,
			tools: None,
			model_override: None,
			thinking_effort: None,
			system_prompt_sections: None,
		};
		let body = build_responses_request("gpt-4.1", &request, false, None, "session-abc", None);
		assert_eq!(body["prompt_cache_key"], "session-abc");
	}

	#[test]
	fn derived_prompt_cache_key_differs_across_back_to_back_constructions() {
		// Back-to-back construction within the same nanosecond must produce
		// different keys — otherwise two sessions started in the same tick
		// would share a cache partition and cross-session prefix reads could
		// leak between them.
		let k1 = derive_session_prompt_cache_key();
		let k2 = derive_session_prompt_cache_key();
		assert_ne!(k1, k2, "back-to-back derivations must differ");
	}

	#[test]
	fn derived_prompt_cache_key_is_non_empty_and_stable_per_instance() {
		// Key derivation must return a non-empty, session-scoped identifier.
		let key = derive_session_prompt_cache_key();
		assert!(!key.is_empty(), "derived key must be non-empty");
		assert!(
			key.starts_with("roku-"),
			"derived key should carry the roku prefix, got: {key}"
		);

		// A provider instance holds one key for its lifetime — two requests
		// built from the same instance must share the same cache partition.
		let config = OpenAiResponsesConfig {
			api_key: "test-key".to_string(),
			base_url: "https://example.invalid/v1/responses".to_string(),
			reasoning_effort: None,
			websocket_mode: false,
		};
		let provider =
			OpenAiResponsesProvider::new(config).expect("provider construction succeeds");
		let request = GenerationRequest {
			system_prompt: None,
			prompt: "hi".to_string(),
			messages: None,
			expected_output_tokens: 100,
			risk_tier: RiskTier::Low,
			preferred_provider: None,
			budget_tokens_remaining: 100_000,
			budget_cost_remaining_usd: 10.0,
			tools: None,
			model_override: None,
			thinking_effort: None,
			system_prompt_sections: None,
		};
		let first = build_responses_request(
			"gpt-4.1",
			&request,
			true,
			None,
			&provider.prompt_cache_key,
			None,
		);
		let second = build_responses_request(
			"gpt-4.1",
			&request,
			true,
			None,
			&provider.prompt_cache_key,
			None,
		);
		assert_eq!(first["prompt_cache_key"], second["prompt_cache_key"]);
		let k = first["prompt_cache_key"].as_str().expect("string key");
		assert!(!k.is_empty());
	}

	#[test]
	fn test_delta_request_includes_previous_response_id() {
		// When previous_response_id is Some, the request body must carry it.
		let request = GenerationRequest {
			system_prompt: None,
			prompt: "hi".to_string(),
			messages: None,
			expected_output_tokens: 100,
			risk_tier: RiskTier::Low,
			preferred_provider: None,
			budget_tokens_remaining: 100_000,
			budget_cost_remaining_usd: 10.0,
			tools: None,
			model_override: None,
			thinking_effort: None,
			system_prompt_sections: None,
		};

		// With previous_response_id — delta request.
		let body = build_responses_request(
			"gpt-4.1",
			&request,
			true,
			None,
			"cache-key",
			Some("resp_abc"),
		);
		assert_eq!(
			body.get("previous_response_id").and_then(|v| v.as_str()),
			Some("resp_abc"),
			"previous_response_id must be present in delta request"
		);

		// Without previous_response_id — first turn, no delta field.
		let body_no_delta =
			build_responses_request("gpt-4.1", &request, true, None, "cache-key", None);
		assert!(
			body_no_delta.get("previous_response_id").is_none(),
			"previous_response_id must be absent when no prior response"
		);
	}

	// ---------------------------------------------------------------------------
	// compact_url helper
	// ---------------------------------------------------------------------------

	#[test]
	fn compact_url_appends_compact_segment() {
		assert_eq!(
			compact_url("https://chatgpt.com/backend-api/codex/responses"),
			"https://chatgpt.com/backend-api/codex/responses/compact"
		);
	}

	#[test]
	fn compact_url_strips_trailing_slash_before_appending() {
		assert_eq!(
			compact_url("https://chatgpt.com/backend-api/codex/responses/"),
			"https://chatgpt.com/backend-api/codex/responses/compact"
		);
	}

	// ---------------------------------------------------------------------------
	// Trait default capability predicates
	// ---------------------------------------------------------------------------

	#[test]
	fn openai_responses_provider_supports_compact_history() {
		let config = OpenAiResponsesConfig {
			api_key: "test-key".to_string(),
			base_url: "https://example.invalid/v1/responses".to_string(),
			reasoning_effort: None,
			websocket_mode: false,
		};
		let provider =
			OpenAiResponsesProvider::new(config).expect("provider construction succeeds");
		assert!(
			provider.supports_compact_history(),
			"OpenAiResponsesProvider must report compact_history support"
		);
	}

	#[test]
	fn openai_responses_provider_does_not_support_output_slot_cap() {
		let config = OpenAiResponsesConfig {
			api_key: "test-key".to_string(),
			base_url: "https://example.invalid/v1/responses".to_string(),
			reasoning_effort: None,
			websocket_mode: false,
		};
		let provider =
			OpenAiResponsesProvider::new(config).expect("provider construction succeeds");
		assert!(
			!provider.supports_output_slot_cap(),
			"OpenAiResponsesProvider must NOT support output-slot escalation"
		);
	}

	// ---------------------------------------------------------------------------
	// messages_to_responses_input and responses_output_to_messages round-trips
	// ---------------------------------------------------------------------------

	#[test]
	fn messages_to_input_converts_user_message() {
		let messages = vec![Message::User {
			content: "hello".to_string(),
		}];
		let input = messages_to_responses_input(&messages);
		assert_eq!(input.len(), 1);
		assert_eq!(input[0]["type"], "message");
		assert_eq!(input[0]["role"], "user");
		assert_eq!(input[0]["content"][0]["text"], "hello");
	}

	#[test]
	fn messages_to_input_converts_assistant_with_text() {
		let messages = vec![Message::Assistant {
			text: "I can help".to_string(),
			tool_calls: vec![],
		}];
		let input = messages_to_responses_input(&messages);
		assert_eq!(input.len(), 1);
		assert_eq!(input[0]["type"], "message");
		assert_eq!(input[0]["role"], "assistant");
		assert_eq!(input[0]["content"][0]["type"], "output_text");
	}

	#[test]
	fn messages_to_input_converts_tool_result() {
		let messages = vec![Message::ToolResult {
			tool_use_id: "call_abc".to_string(),
			content: "result".to_string(),
			is_error: false,
		}];
		let input = messages_to_responses_input(&messages);
		assert_eq!(input.len(), 1);
		assert_eq!(input[0]["type"], "function_call_output");
		assert_eq!(input[0]["call_id"], "call_abc");
	}

	#[test]
	fn responses_output_to_messages_converts_assistant_message() {
		let items = vec![json!({
			"type": "message",
			"role": "assistant",
			"content": [{"type": "output_text", "text": "summary here"}]
		})];
		let messages = responses_output_to_messages(&items);
		assert_eq!(messages.len(), 1);
		assert!(matches!(&messages[0], Message::Assistant { text, .. } if text == "summary here"));
	}

	#[test]
	fn responses_output_to_messages_converts_function_call() {
		let items = vec![json!({
			"type": "function_call",
			"call_id": "call_xyz",
			"name": "read_file",
			"arguments": "{\"path\": \"/foo\"}"
		})];
		let messages = responses_output_to_messages(&items);
		assert_eq!(messages.len(), 1);
		match &messages[0] {
			Message::Assistant { tool_calls, .. } => {
				assert_eq!(tool_calls.len(), 1);
				assert_eq!(tool_calls[0].id, "call_xyz");
				assert_eq!(tool_calls[0].name, "read_file");
			}
			other => panic!("expected Assistant, got {other:?}"),
		}
	}

	#[test]
	fn responses_output_to_messages_skips_unknown_items() {
		let items = vec![
			json!({ "type": "thinking", "content": "..." }),
			json!({ "type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "hi"}] }),
		];
		let messages = responses_output_to_messages(&items);
		// thinking item is skipped
		assert_eq!(messages.len(), 1);
	}

	// ---------------------------------------------------------------------------
	// compact_history HTTP integration tests (mock TCP server)
	// ---------------------------------------------------------------------------

	/// Bind a local TCP listener and return its address.
	/// The returned JoinHandle serves one HTTP exchange then exits.
	async fn spawn_mock_compact_server(
		status: u16,
		body: &'static str,
	) -> (String, tokio::task::JoinHandle<()>) {
		let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
			.await
			.expect("mock server: bind must succeed");
		let addr = listener.local_addr().expect("must have address");
		let handle = tokio::spawn(async move {
			if let Ok((mut stream, _)) = listener.accept().await {
				use tokio::io::{AsyncReadExt, AsyncWriteExt};
				let mut buf = vec![0u8; 4096];
				// Read the request (we don't validate it in most tests).
				let _ = stream.read(&mut buf).await;
				let response = format!(
					"HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
					body.len(),
					body
				);
				let _ = stream.write_all(response.as_bytes()).await;
			}
		});
		(format!("http://{addr}/responses"), handle)
	}

	fn compact_provider_for(base_url: String) -> OpenAiResponsesProvider {
		let config = OpenAiResponsesConfig {
			api_key: "test-key".to_string(),
			base_url,
			reasoning_effort: None,
			websocket_mode: false,
		};
		OpenAiResponsesProvider::new(config).expect("provider construction succeeds")
	}

	fn minimal_compact_request(base_url: &str) -> CompactRequest {
		let _ = base_url;
		CompactRequest {
			model: "gpt-4.1".to_string(),
			instructions: "summarize".to_string(),
			input: vec![Message::User {
				content: "hello".to_string(),
			}],
			tools: vec![],
			parallel_tool_calls: false,
			reasoning: None,
		}
	}

	#[tokio::test]
	async fn compact_history_success_returns_parsed_response() {
		let success_body = r#"{
			"output": [
				{"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "compact summary"}]}
			],
			"usage": {"input_tokens": 100, "output_tokens": 50, "input_tokens_details": {"cached_tokens": 20}}
		}"#;
		let (base_url, _srv) = spawn_mock_compact_server(200, success_body).await;
		let provider = compact_provider_for(base_url.clone());
		let req = minimal_compact_request(&base_url);

		let result = provider.compact_history(&req).await;
		assert!(
			result.is_some(),
			"must return Some for a supporting provider"
		);
		let response = result.unwrap().expect("must be Ok on 200");

		assert_eq!(response.output.len(), 1, "one output message expected");
		assert!(
			matches!(&response.output[0], Message::Assistant { text, .. } if text == "compact summary")
		);
		assert_eq!(response.usage.prompt_tokens, 100);
		assert_eq!(response.usage.output_tokens, 50);
		assert_eq!(response.usage.cached_input_tokens, 20);
	}

	#[tokio::test]
	async fn compact_history_404_returns_err_without_panic() {
		let (base_url, _srv) = spawn_mock_compact_server(404, r#"{"error":"not found"}"#).await;
		let provider = compact_provider_for(base_url.clone());
		let req = minimal_compact_request(&base_url);

		let result = provider.compact_history(&req).await;
		assert!(result.is_some());
		let err = result.unwrap().expect_err("404 must be an error");
		assert!(
			matches!(err, ProviderCallError::Fatal { .. }),
			"404 should map to Fatal: {err}"
		);
	}

	#[tokio::test]
	async fn compact_history_500_returns_err() {
		let (base_url, _srv) = spawn_mock_compact_server(500, r#"{"error":"server error"}"#).await;
		let provider = compact_provider_for(base_url.clone());
		let req = minimal_compact_request(&base_url);

		let result = provider.compact_history(&req).await;
		assert!(result.is_some());
		let err = result.unwrap().expect_err("500 must be an error");
		assert!(
			matches!(err, ProviderCallError::ServerError { status: 500, .. }),
			"500 should map to ServerError: {err}"
		);
	}

	#[tokio::test]
	async fn compact_history_malformed_body_missing_output_returns_err() {
		let (base_url, _srv) = spawn_mock_compact_server(200, r#"{"usage":{}}"#).await;
		let provider = compact_provider_for(base_url.clone());
		let req = minimal_compact_request(&base_url);

		let result = provider.compact_history(&req).await;
		assert!(result.is_some());
		let err = result
			.unwrap()
			.expect_err("missing output field must be an error");
		match err {
			ProviderCallError::Fatal { message } => {
				assert!(
					message.contains("output"),
					"error message should mention output field, got: {message}"
				);
			}
			other => panic!("expected Fatal, got {other:?}"),
		}
	}

	#[tokio::test]
	async fn compact_history_timeout_returns_err() {
		// Bind a listener that never responds within the test timeout.
		// We use a very short timeout to keep CI fast, configured via the
		// COMPACT_REQUEST_TIMEOUT constant (90s). To avoid a 90s wait in CI,
		// we connect to an address that drops the connection immediately.
		// The provider's timeout is 90s; this test just verifies the error
		// path when the server hangs by connecting to a port that accepts
		// but never writes a response.
		let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
			.await
			.expect("bind");
		let addr = listener.local_addr().expect("addr");
		// Accept but never respond.
		tokio::spawn(async move {
			if let Ok((_stream, _)) = listener.accept().await {
				// Hold the connection open briefly then drop it.
				tokio::time::sleep(std::time::Duration::from_millis(50)).await;
				// Stream dropped here — server closes the connection.
			}
		});

		// Use a very short client timeout for the test by constructing a custom client.
		let client = reqwest::Client::builder()
			.connect_timeout(std::time::Duration::from_millis(200))
			.build()
			.expect("client");
		let config = OpenAiResponsesConfig {
			api_key: "test-key".to_string(),
			base_url: format!("http://{addr}/responses"),
			reasoning_effort: None,
			websocket_mode: false,
		};
		let provider = OpenAiResponsesProvider {
			client,
			config,
			prompt_cache_key: "test".to_string(),
			ws_session: std::sync::Arc::new(tokio::sync::Mutex::new(
				crate::providers::openai_ws::OpenAiWsSession::new(false),
			)),
			compact_timeout: COMPACT_REQUEST_TIMEOUT,
		};
		let req = CompactRequest {
			model: "gpt-4.1".to_string(),
			instructions: "summarize".to_string(),
			input: vec![],
			tools: vec![],
			parallel_tool_calls: false,
			reasoning: None,
		};

		let result = provider.compact_history(&req).await;
		assert!(result.is_some(), "must return Some");
		let err = result
			.unwrap()
			.expect_err("connection close must be an error");
		// Either ConnectionFailed or Fatal is acceptable here.
		assert!(
			matches!(
				err,
				ProviderCallError::ConnectionFailed { .. }
					| ProviderCallError::Fatal { .. }
					| ProviderCallError::Timeout { .. }
			),
			"expected a failure variant, got: {err:?}"
		);
	}

	#[tokio::test]
	async fn compact_history_timeout_returns_timeout_variant() {
		// This test verifies that the COMPACT_REQUEST_TIMEOUT constant is
		// actually wired into the compact_history() path. A mock server that
		// accepts the TCP connection but never writes a response is used.
		// The provider is constructed with a very short timeout via the
		// test-only with_compact_timeout() constructor so the test completes
		// quickly rather than waiting for the full 90s production constant.
		//
		// The production constant (90s) is left unchanged — this test only
		// exercises the code path, not the specific duration.
		assert_eq!(
			COMPACT_REQUEST_TIMEOUT,
			std::time::Duration::from_secs(90),
			"production COMPACT_REQUEST_TIMEOUT must remain 90s"
		);

		// Bind a server that accepts the connection but never writes a response.
		let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
			.await
			.expect("bind");
		let addr = listener.local_addr().expect("addr");
		tokio::spawn(async move {
			// Accept connection and hold it open without responding.
			if let Ok((_stream, _)) = listener.accept().await {
				// Stream held open until this task completes; the client
				// timeout must fire before this is dropped.
				tokio::time::sleep(std::time::Duration::from_secs(10)).await;
			}
		});

		let client = reqwest::Client::builder()
			.build()
			.expect("client");
		let config = OpenAiResponsesConfig {
			api_key: "test-key".to_string(),
			base_url: format!("http://{addr}/responses"),
			reasoning_effort: None,
			websocket_mode: false,
		};
		// Inject a 300ms timeout so the test completes quickly.
		let provider = OpenAiResponsesProvider::with_compact_timeout(
			client,
			config,
			std::time::Duration::from_millis(300),
		);

		let req = CompactRequest {
			model: "gpt-4.1".to_string(),
			instructions: "summarize".to_string(),
			input: vec![],
			tools: vec![],
			parallel_tool_calls: false,
			reasoning: None,
		};

		let started = std::time::Instant::now();
		let result = provider.compact_history(&req).await;
		let elapsed = started.elapsed();

		// Must complete well under 1s (we used a 300ms timeout).
		assert!(
			elapsed < std::time::Duration::from_secs(1),
			"compact_history must respect the timeout: elapsed={elapsed:?}"
		);

		assert!(result.is_some(), "must return Some");
		let err = result
			.unwrap()
			.expect_err("hung server must be an error");
		assert!(
			matches!(err, ProviderCallError::Timeout { .. }),
			"expected Timeout variant when server never responds, got: {err:?}"
		);
	}
}
