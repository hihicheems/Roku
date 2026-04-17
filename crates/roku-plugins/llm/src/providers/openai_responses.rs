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
	GenerationRequest, Message, ModelProfile, ProviderCallError, ProviderResponse, RiskTier,
	RoutingPolicy, StreamChunk, ToolCallBlock, ToolDefinition, estimate_prompt_tokens,
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
	/// Default: `https://chatgpt.com/backend-api/codex/responses`
	pub base_url: String,
	/// Optional reasoning effort (`low`, `medium`, `high`).
	pub reasoning_effort: Option<String>,
	/// Enable delta mode: include `previous_response_id` on subsequent turns
	/// to reduce upstream payload size. Controlled by `ROKU_OPENAI_WEBSOCKET_MODE`.
	/// Default: `false`.
	pub websocket_mode: bool,
	/// ChatGPT account ID from the OAuth id_token `chatgpt_account_id` claim.
	/// Present only for OAuth-authenticated sessions. When `Some`, the
	/// `ChatGPT-Account-ID` request header is emitted.
	pub chatgpt_account_id: Option<String>,
	/// Whether the authenticated account is FedRAMP-eligible. When `true` and
	/// `chatgpt_account_id` is `Some`, the `X-OpenAI-Fedramp: true` header is
	/// emitted.
	pub chatgpt_account_is_fedramp: bool,
	/// Fixed originator string recognised by the Codex backend.
	pub originator: String,
	/// Stable per-installation UUID read from `~/.roku/state/installation-id`.
	pub installation_id: String,
	/// Per-session stable identifier used for identity headers and as the basis
	/// of the `prompt_cache_key` body field.
	pub session_id: String,
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
			chatgpt_account_id: None,
			chatgpt_account_is_fedramp: false,
			originator: "codex_cli_rs".to_string(),
			installation_id: String::new(),
			session_id: String::new(),
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
// Reachability probe
// ---------------------------------------------------------------------------

/// Issue a single HEAD request against `base_url` with a 5-second timeout to
/// check whether the Responses endpoint is reachable.
///
/// - 2xx / 3xx / non-auth 4xx → silent (returns `Ok(())`).
/// - 401 / 403 → deferred to the normal auth error path (`Ok(())`); no warning.
/// - Connect failure, DNS failure, timeout → returns `Err(description)`.
///
/// Callers should print a yellow `[warn]` line to stderr on `Err`.
pub fn probe_responses_reachability(base_url: &str) -> Result<(), String> {
	let client = reqwest::blocking::Client::builder()
		.timeout(std::time::Duration::from_secs(5))
		.build()
		.map_err(|e| format!("failed to build probe client: {e}"))?;

	match client.head(base_url).send() {
		Ok(resp) => {
			let status = resp.status().as_u16();
			// 401/403 → treat as auth issue, not a reachability issue.
			if status == 401 || status == 403 {
				return Ok(());
			}
			// Any other response (2xx/3xx/4xx/5xx) means the endpoint was reachable.
			Ok(())
		}
		Err(e) => Err(e.to_string()),
	}
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
}

impl OpenAiResponsesProvider {
	pub fn new(config: OpenAiResponsesConfig) -> Result<Self, ProviderCallError> {
		let client = Client::builder()
			.connect_timeout(std::time::Duration::from_secs(30))
			.build()
			.map_err(|e| ProviderCallError::Fatal {
				message: format!("http client error: {e}"),
			})?;
		let prompt_cache_key = derive_session_prompt_cache_key(&config.session_id);
		let ws_session = std::sync::Arc::new(tokio::sync::Mutex::new(OpenAiWsSession::new(
			config.websocket_mode,
		)));
		Ok(Self {
			client,
			config,
			prompt_cache_key,
			ws_session,
		})
	}

	fn build_headers(&self) -> Result<HeaderMap, ProviderCallError> {
		use reqwest::header::HeaderName;

		let mut headers = HeaderMap::new();

		// Authorization
		headers.insert(
			reqwest::header::AUTHORIZATION,
			HeaderValue::from_str(&format!("Bearer {}", self.config.api_key)).map_err(|e| {
				ProviderCallError::Fatal {
					message: format!("invalid authorization header: {e}"),
				}
			})?,
		);

		// Content-Type
		headers.insert(
			reqwest::header::CONTENT_TYPE,
			HeaderValue::from_static("application/json"),
		);

		// Accept
		headers.insert(
			reqwest::header::ACCEPT,
			HeaderValue::from_static("text/event-stream"),
		);

		// User-Agent: codex_cli_rs/{version} ({os} {arch})
		let user_agent = format!(
			"codex_cli_rs/{} ({} {})",
			env!("CARGO_PKG_VERSION"),
			std::env::consts::OS,
			std::env::consts::ARCH,
		);
		headers.insert(
			reqwest::header::USER_AGENT,
			HeaderValue::from_str(&user_agent).map_err(|e| ProviderCallError::Fatal {
				message: format!("invalid user-agent header: {e}"),
			})?,
		);

		// originator
		headers.insert(
			HeaderName::from_static("originator"),
			HeaderValue::from_str(&self.config.originator).map_err(|e| ProviderCallError::Fatal {
				message: format!("invalid originator header: {e}"),
			})?,
		);

		// session_id
		if !self.config.session_id.is_empty() {
			headers.insert(
				HeaderName::from_static("session_id"),
				HeaderValue::from_str(&self.config.session_id).map_err(|e| {
					ProviderCallError::Fatal {
						message: format!("invalid session_id header: {e}"),
					}
				})?,
			);

			// x-client-request-id
			headers.insert(
				HeaderName::from_static("x-client-request-id"),
				HeaderValue::from_str(&self.config.session_id).map_err(|e| {
					ProviderCallError::Fatal {
						message: format!("invalid x-client-request-id header: {e}"),
					}
				})?,
			);

			// x-codex-window-id: {session_id}:0
			let window_id = format!("{}:0", self.config.session_id);
			headers.insert(
				HeaderName::from_static("x-codex-window-id"),
				HeaderValue::from_str(&window_id).map_err(|e| ProviderCallError::Fatal {
					message: format!("invalid x-codex-window-id header: {e}"),
				})?,
			);
		}

		// x-codex-installation-id
		if !self.config.installation_id.is_empty() {
			headers.insert(
				HeaderName::from_static("x-codex-installation-id"),
				HeaderValue::from_str(&self.config.installation_id).map_err(|e| {
					ProviderCallError::Fatal {
						message: format!("invalid x-codex-installation-id header: {e}"),
					}
				})?,
			);
		}

		// x-openai-internal-codex-residency
		headers.insert(
			HeaderName::from_static("x-openai-internal-codex-residency"),
			HeaderValue::from_static("us"),
		);

		// OAuth-only headers
		if let Some(account_id) = &self.config.chatgpt_account_id {
			headers.insert(
				HeaderName::from_static("chatgpt-account-id"),
				HeaderValue::from_str(account_id).map_err(|e| ProviderCallError::Fatal {
					message: format!("invalid chatgpt-account-id header: {e}"),
				})?,
			);

			if self.config.chatgpt_account_is_fedramp {
				headers.insert(
					HeaderName::from_static("x-openai-fedramp"),
					HeaderValue::from_static("true"),
				);
			}
		}

		Ok(headers)
	}
}

// ---------------------------------------------------------------------------
// Prompt cache key derivation
// ---------------------------------------------------------------------------

/// Monotonic per-process counter that disambiguates two provider instances
/// constructed within the same nanosecond tick (e.g. reconnect after init
/// failure). Used only when no explicit session_id is provided.
static PROMPT_CACHE_KEY_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Derive a session-stable opaque key for `prompt_cache_key`.
///
/// When `session_id` is non-empty, the key is the raw `session_id` so that
/// the `prompt_cache_key` body field and the `session_id` identity header
/// carry the byte-identical value, letting the backend index on a single
/// dimension.
///
/// When `session_id` is empty (legacy / test path), falls back to a
/// `pid+nanos+counter` derivation that is still stable per provider instance
/// and collision-free across restarts.
fn derive_session_prompt_cache_key(session_id: &str) -> String {
	if !session_id.is_empty() {
		return session_id.to_string();
	}
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
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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
		// When no session_id is given, back-to-back derivations must produce
		// different keys (pid+nanos+counter fallback path).
		let k1 = derive_session_prompt_cache_key("");
		let k2 = derive_session_prompt_cache_key("");
		assert_ne!(k1, k2, "back-to-back derivations must differ");
	}

	#[test]
	fn derived_prompt_cache_key_equals_session_id() {
		// When a session_id is provided, the cache key must equal the raw session_id
		// so that the prompt_cache_key body field and the session_id header are
		// byte-identical, letting the backend index on a single dimension.
		let session_id = "test-session-abc-123";
		let key = derive_session_prompt_cache_key(session_id);
		assert_eq!(key, session_id, "cache key must equal the raw session_id");
	}

	#[test]
	fn derived_prompt_cache_key_is_non_empty_and_stable_per_instance() {
		// Key derivation must return a non-empty, session-scoped identifier equal
		// to the raw session_id when one is provided.
		let key = derive_session_prompt_cache_key("stable-session");
		assert!(!key.is_empty(), "derived key must be non-empty");
		assert_eq!(
			key, "stable-session",
			"derived key must equal the raw session_id, got: {key}"
		);

		// A provider instance holds one key for its lifetime — two requests
		// built from the same instance must share the same cache partition.
		let config = OpenAiResponsesConfig {
			api_key: "test-key".to_string(),
			base_url: "https://example.invalid/v1/responses".to_string(),
			reasoning_effort: None,
			websocket_mode: false,
			chatgpt_account_id: None,
			chatgpt_account_is_fedramp: false,
			originator: "codex_cli_rs".to_string(),
			installation_id: "install-abc".to_string(),
			session_id: "session-test-123".to_string(),
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

	// --- build_headers tests ---

	fn make_test_config_api_key() -> OpenAiResponsesConfig {
		OpenAiResponsesConfig {
			api_key: "sk-test-key".to_string(),
			base_url: "https://example.invalid/v1/responses".to_string(),
			reasoning_effort: None,
			websocket_mode: false,
			chatgpt_account_id: None,
			chatgpt_account_is_fedramp: false,
			originator: "codex_cli_rs".to_string(),
			installation_id: "install-test-uuid".to_string(),
			session_id: "session-test-uuid".to_string(),
		}
	}

	fn make_test_config_oauth(account_id: Option<&str>, is_fedramp: bool) -> OpenAiResponsesConfig {
		OpenAiResponsesConfig {
			api_key: "oauth-token-abc".to_string(),
			base_url: "https://example.invalid/v1/responses".to_string(),
			reasoning_effort: None,
			websocket_mode: false,
			chatgpt_account_id: account_id.map(str::to_string),
			chatgpt_account_is_fedramp: is_fedramp,
			originator: "codex_cli_rs".to_string(),
			installation_id: "install-test-uuid".to_string(),
			session_id: "session-test-uuid".to_string(),
		}
	}

	#[test]
	fn build_headers_api_key_mode_emits_base_headers() {
		let config = make_test_config_api_key();
		let provider = OpenAiResponsesProvider::new(config).expect("construction");
		let headers = provider.build_headers().expect("headers");

		// Must have Authorization, Content-Type, Accept.
		assert!(headers.contains_key(reqwest::header::AUTHORIZATION));
		assert!(headers.contains_key(reqwest::header::CONTENT_TYPE));
		assert!(headers.contains_key(reqwest::header::ACCEPT));
		assert!(headers.contains_key(reqwest::header::USER_AGENT));

		// Must have originator and residency.
		assert!(headers.contains_key("originator"));
		assert!(headers.contains_key("x-openai-internal-codex-residency"));

		// Must have session_id and related.
		assert!(headers.contains_key("session_id"));
		assert!(headers.contains_key("x-client-request-id"));
		assert!(headers.contains_key("x-codex-installation-id"));
		assert!(headers.contains_key("x-codex-window-id"));

		// Must NOT have ChatGPT-Account-ID or Fedramp in API-key mode.
		assert!(!headers.contains_key("chatgpt-account-id"));
		assert!(!headers.contains_key("x-openai-fedramp"));
	}

	#[test]
	fn build_headers_api_key_mode_omits_chatgpt_account_headers() {
		let config = make_test_config_api_key();
		let provider = OpenAiResponsesProvider::new(config).expect("construction");
		let headers = provider.build_headers().expect("headers");
		assert!(
			!headers.contains_key("chatgpt-account-id"),
			"ChatGPT-Account-ID must be absent in API-key mode"
		);
		assert!(
			!headers.contains_key("x-openai-fedramp"),
			"X-OpenAI-Fedramp must be absent in API-key mode"
		);
	}

	#[test]
	fn build_headers_oauth_mode_with_account_id_emits_all_headers() {
		let config = make_test_config_oauth(Some("acc-123"), false);
		let provider = OpenAiResponsesProvider::new(config).expect("construction");
		let headers = provider.build_headers().expect("headers");

		assert!(headers.contains_key("chatgpt-account-id"));
		// Fedramp absent because is_fedramp = false.
		assert!(!headers.contains_key("x-openai-fedramp"));

		// Common identity headers still present.
		assert!(headers.contains_key("session_id"));
		assert!(headers.contains_key("x-codex-installation-id"));
		assert!(headers.contains_key("originator"));
		assert!(headers.contains_key("x-openai-internal-codex-residency"));
	}

	#[test]
	fn build_headers_oauth_fedramp_emits_fedramp_header() {
		let config = make_test_config_oauth(Some("acc-fed"), true);
		let provider = OpenAiResponsesProvider::new(config).expect("construction");
		let headers = provider.build_headers().expect("headers");

		assert!(headers.contains_key("chatgpt-account-id"));
		assert!(headers.contains_key("x-openai-fedramp"));
		let fedramp_val = headers.get("x-openai-fedramp").unwrap().to_str().unwrap();
		assert_eq!(fedramp_val, "true");
	}

	#[test]
	fn build_headers_oauth_missing_account_id_omits_chatgpt_headers_no_crash() {
		// account_id is None — must not include ChatGPT-Account-ID and must not panic.
		let config = make_test_config_oauth(None, false);
		let provider = OpenAiResponsesProvider::new(config).expect("construction");
		let headers = provider.build_headers().expect("headers");
		assert!(!headers.contains_key("chatgpt-account-id"));
		assert!(!headers.contains_key("x-openai-fedramp"));
	}

	#[test]
	fn build_headers_originator_reflects_config_value() {
		// Provider must emit the configured originator, not a hardcoded literal.
		let config = OpenAiResponsesConfig {
			api_key: "test".to_string(),
			base_url: "https://example.invalid".to_string(),
			reasoning_effort: None,
			websocket_mode: false,
			chatgpt_account_id: None,
			chatgpt_account_is_fedramp: false,
			originator: "something-else".to_string(),
			installation_id: "install-x".to_string(),
			session_id: "session-x".to_string(),
		};
		let provider = OpenAiResponsesProvider::new(config).expect("construction");
		let headers = provider.build_headers().expect("headers");
		let originator_val = headers.get("originator").unwrap().to_str().unwrap();
		assert_eq!(originator_val, "something-else");
	}

	#[test]
	fn build_headers_window_id_is_session_id_colon_zero() {
		let session_id = "session-abc-123";
		let config = OpenAiResponsesConfig {
			api_key: "test".to_string(),
			base_url: "https://example.invalid".to_string(),
			reasoning_effort: None,
			websocket_mode: false,
			chatgpt_account_id: None,
			chatgpt_account_is_fedramp: false,
			originator: "codex_cli_rs".to_string(),
			installation_id: "install-x".to_string(),
			session_id: session_id.to_string(),
		};
		let provider = OpenAiResponsesProvider::new(config).expect("construction");
		let headers = provider.build_headers().expect("headers");
		let window_id = headers.get("x-codex-window-id").unwrap().to_str().unwrap();
		assert_eq!(window_id, format!("{session_id}:0"));
	}

	// --- probe tests ---

	#[test]
	fn probe_connect_refused_returns_err_with_description() {
		// Port 1 is always refused on all OSes in test environments.
		let result = probe_responses_reachability("http://127.0.0.1:1");
		assert!(
			result.is_err(),
			"connect-refused probe must return Err, got: {result:?}"
		);
	}

	#[test]
	fn probe_unreachable_host_returns_err() {
		// An invalid domain that will never resolve.
		let result = probe_responses_reachability("http://this-host-does-not-exist.invalid/test");
		assert!(result.is_err(), "DNS-failure probe must return Err");
	}
}
