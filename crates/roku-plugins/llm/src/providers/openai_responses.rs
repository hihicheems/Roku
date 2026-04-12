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
use std::time::Instant;

use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use reqwest::Client;
use reqwest::header::{HeaderMap, HeaderValue};
use roku_common_types::{LogLevel, LogRecord, Metrics, emit_global_log};
use serde_json::{Value, json};
use thiserror::Error;
use tokio::sync::mpsc;

use crate::router::{LlmProvider, LlmRouter};
use crate::types::{
	GenerationRequest, Message, ModelProfile, ProviderCallError, ProviderResponse, RiskTier,
	RoutingPolicy, StreamChunk, ToolCallBlock, ToolDefinition, estimate_prompt_tokens,
};

const OPENAI_RESPONSES_PROVIDER: &str = "openai_responses";
const DEFAULT_RESPONSES_URL: &str = "https://api.openai.com/v1/responses";

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
}

impl OpenAiResponsesConfig {
	pub fn new(api_key: String) -> Self {
		Self {
			api_key,
			base_url: DEFAULT_RESPONSES_URL.to_string(),
			reasoning_effort: None,
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
}

impl OpenAiResponsesProvider {
	pub fn new(config: OpenAiResponsesConfig) -> Result<Self, ProviderCallError> {
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

/// Convert a [`GenerationRequest`] into the Responses API `input[]` format.
///
/// The Responses API does not accept system messages in `input[]`; they must
/// appear as a top-level `instructions` string. Tool results become
/// `function_call_output` items. Assistant messages with tool calls emit
/// separate `function_call` items for each call.
fn build_responses_request(
	model_id: &str,
	request: &GenerationRequest,
	stream: bool,
	reasoning_effort: Option<&str>,
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
	});

	if let Some(tools) = tools_value.filter(|t| !t.is_empty()) {
		body["tools"] = json!(tools);
	}

	// Reasoning config — only attach when requested.
	if let Some(effort) = reasoning_effort {
		body["reasoning"] = json!({
			"effort": effort,
			"summary": "auto",
		});
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

/// Parsed result from a non-streaming Responses API response body.
#[derive(Debug)]
struct ParsedResponsesCompletion {
	output: String,
	finish_reason: Option<String>,
	prompt_tokens: u64,
	output_tokens: u64,
	tool_calls: Option<Vec<ToolCallBlock>>,
}

/// Parse the full JSON body returned when `stream: false`.
///
/// The Responses API returns:
/// ```json
/// {
///   "output": [
///     {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "..."}]},
///     {"type": "function_call", "name": "...", "arguments": "...", "call_id": "..."}
///   ],
///   "usage": {"input_tokens": N, "output_tokens": M}
/// }
/// ```
fn parse_responses_completion(body: &str) -> Result<ParsedResponsesCompletion, ProviderCallError> {
	let response: Value = serde_json::from_str(body)
		.map_err(|e| ProviderCallError::retryable(format!("invalid response json: {e}")))?;

	// Surface any API-level error object.
	if let Some(error) = response.get("error") {
		let message = error
			.get("message")
			.and_then(Value::as_str)
			.unwrap_or("unknown error");
		return Err(ProviderCallError::retryable(format!(
			"openai responses api error: {message}"
		)));
	}

	let output_items = response
		.get("output")
		.and_then(Value::as_array)
		.cloned()
		.unwrap_or_default();

	let mut text_parts: Vec<String> = Vec::new();
	let mut tool_calls: Vec<ToolCallBlock> = Vec::new();

	for item in &output_items {
		let item_type = item.get("type").and_then(Value::as_str).unwrap_or("");
		match item_type {
			"message" => {
				if let Some(content_array) = item.get("content").and_then(Value::as_array) {
					for part in content_array {
						if part
							.get("type")
							.and_then(Value::as_str)
							.map(|t| t == "output_text")
							.unwrap_or(false) && let Some(text) =
							part.get("text").and_then(Value::as_str)
						{
							text_parts.push(text.to_string());
						}
					}
				}
			}
			"function_call" => {
				let id = item
					.get("call_id")
					.and_then(Value::as_str)
					.unwrap_or("")
					.to_string();
				let name = item
					.get("name")
					.and_then(Value::as_str)
					.unwrap_or("")
					.to_string();
				let arguments = item
					.get("arguments")
					.and_then(Value::as_str)
					.and_then(|s| serde_json::from_str::<Value>(s).ok())
					.unwrap_or(Value::Null);
				tool_calls.push(ToolCallBlock {
					id,
					name,
					arguments,
				});
			}
			_ => {}
		}
	}

	let usage = response.get("usage");
	let prompt_tokens = usage
		.and_then(|u| u.get("input_tokens"))
		.and_then(Value::as_u64)
		.unwrap_or(0);
	let output_tokens = usage
		.and_then(|u| u.get("output_tokens"))
		.and_then(Value::as_u64)
		.unwrap_or(0);

	// Derive a finish reason from the response status field if present.
	let finish_reason = response
		.get("status")
		.and_then(Value::as_str)
		.map(str::to_string);

	Ok(ParsedResponsesCompletion {
		output: text_parts.join(""),
		finish_reason,
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
	has_function_call: bool,
	/// Set when an `error` or `response.failed` SSE event is received.
	stream_error: Option<String>,
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
			has_function_call: false,
			stream_error: None,
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
			// Log for observability; no action needed.
			if let Some(id) = parsed
				.get("response")
				.and_then(|r| r.get("id"))
				.and_then(Value::as_str)
			{
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
		let headers = self.build_headers()?;
		let body = build_responses_request(
			&model.model_id,
			request,
			false, // non-streaming
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
			log_responses(
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

		let parsed = parse_responses_completion(&response_body)?;
		let latency_ms = u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX);

		log_responses(
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
		let body = build_responses_request(
			&model.model_id,
			request,
			true, // streaming
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

		loop {
			let event_result = event_stream.next().await;
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
					stream_error = Some(ProviderCallError::retryable(format!(
						"SSE stream error: {e}"
					)));
					break;
				}
				None => break,
			}
		}

		// Propagate API-level errors from SSE events.
		if stream_error.is_none()
			&& let Some(msg) = state.stream_error.take()
		{
			stream_error =
				Some(ProviderCallError::non_retryable(format!("responses api: {msg}")));
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
			return Err(error);
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
			],
		);

		Ok(ProviderResponse {
			output: state.full_text,
			finish_reason: state.finish_reason,
			prompt_tokens: state.prompt_tokens,
			output_tokens: state.output_tokens,
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
	let message = format!("openai responses returned status {status_code}: {response_body}");
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
		};

		let body = build_responses_request("gpt-4.1", &request, false, None);
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
		};

		let body = build_responses_request("gpt-4.1", &request, false, None);
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
		};

		let body = build_responses_request("gpt-4.1", &request, false, None);
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
		};

		let body = build_responses_request("gpt-4.1", &request, false, None);
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
		};

		let body = build_responses_request("gpt-4.1", &request, false, None);
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
		};

		let body = build_responses_request("gpt-4.1", &request, false, Some("high"));
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
		};

		let body = build_responses_request("gpt-4.1", &request, false, None);
		assert!(body.get("reasoning").is_none());
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
		};

		let body = build_responses_request("gpt-4.1", &request, false, None);
		let tools = body["tools"].as_array().expect("tools array");
		assert_eq!(tools.len(), 1);
		assert_eq!(tools[0]["type"], "function");
		assert_eq!(tools[0]["name"], "search");
	}

	// --- Non-streaming response parsing ---

	#[test]
	fn parse_text_only_response() {
		let body = r#"{
            "output": [
                {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "Hello!"}]}
            ],
            "usage": {"input_tokens": 10, "output_tokens": 5},
            "status": "completed"
        }"#;

		let parsed = parse_responses_completion(body).unwrap();
		assert_eq!(parsed.output, "Hello!");
		assert_eq!(parsed.prompt_tokens, 10);
		assert_eq!(parsed.output_tokens, 5);
		assert!(parsed.tool_calls.is_none());
		assert_eq!(parsed.finish_reason.as_deref(), Some("completed"));
	}

	#[test]
	fn parse_function_call_response() {
		let body = r#"{
            "output": [
                {"type": "function_call", "name": "read_file", "arguments": "{\"path\":\"/foo.rs\"}", "call_id": "call_1"}
            ],
            "usage": {"input_tokens": 20, "output_tokens": 15}
        }"#;

		let parsed = parse_responses_completion(body).unwrap();
		assert!(parsed.output.is_empty());
		let tool_calls = parsed.tool_calls.unwrap();
		assert_eq!(tool_calls.len(), 1);
		assert_eq!(tool_calls[0].id, "call_1");
		assert_eq!(tool_calls[0].name, "read_file");
		assert_eq!(tool_calls[0].arguments["path"], "/foo.rs");
	}

	#[test]
	fn parse_mixed_text_and_tool_calls() {
		let body = r#"{
            "output": [
                {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "Reading file..."}]},
                {"type": "function_call", "name": "read_file", "arguments": "{\"path\":\"/a.txt\"}", "call_id": "call_a"}
            ],
            "usage": {"input_tokens": 30, "output_tokens": 20}
        }"#;

		let parsed = parse_responses_completion(body).unwrap();
		assert_eq!(parsed.output, "Reading file...");
		let tool_calls = parsed.tool_calls.unwrap();
		assert_eq!(tool_calls.len(), 1);
		assert_eq!(tool_calls[0].name, "read_file");
	}

	#[test]
	fn parse_api_error_response() {
		let body = r#"{"error": {"message": "invalid_api_key", "type": "auth_error"}}"#;
		let result = parse_responses_completion(body);
		assert!(result.is_err());
		assert!(result.unwrap_err().to_string().contains("invalid_api_key"));
	}
}
