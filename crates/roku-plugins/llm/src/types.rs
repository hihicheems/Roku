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

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum RiskTier {
	Low,
	Medium,
	High,
	Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingEffort {
	None,
	Low,
	Medium,
	High,
}

/// Per-model, per-tier pricing in USD per million tokens.
/// Anthropic uses 4 tiers; OpenAI uses 3 tiers (cache_write is free).
#[derive(Debug, Clone, PartialEq)]
pub struct ModelCostProfile {
	pub model_id_prefix: &'static str,
	pub provider: &'static str,
	pub input_per_mtok: f64,
	pub cache_write_per_mtok: f64,
	pub cache_read_per_mtok: f64,
	pub output_per_mtok: f64,
	/// Maximum output tokens for this model family.
	pub max_output_tokens: u64,
	/// Pricing table timestamp for staleness detection.
	pub as_of: &'static str,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelProfile {
	pub model_id: String,
	pub provider: String,
	pub max_context_tokens: u64,
	pub cost_per_1k_tokens_usd: f64,
	pub max_risk_tier: RiskTier,
	pub route_priority: u8,
}

impl ModelProfile {
	pub(crate) fn supports(&self, request: &GenerationRequest) -> bool {
		if let Some(preferred_provider) = &request.preferred_provider
			&& preferred_provider != &self.provider
		{
			return false;
		}

		let estimated_prompt_tokens = estimate_request_input_tokens(request);
		let estimated_total_tokens =
			estimated_prompt_tokens.saturating_add(request.expected_output_tokens);
		if estimated_total_tokens > self.max_context_tokens {
			return false;
		}
		if estimated_total_tokens > request.budget_tokens_remaining {
			return false;
		}
		if self.max_risk_tier < request.risk_tier {
			return false;
		}

		let estimated_cost = estimate_cost_usd(estimated_total_tokens, self.cost_per_1k_tokens_usd);
		estimated_cost <= request.budget_cost_remaining_usd
	}
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutingPolicy {
	pub max_request_cost_usd: f64,
	pub max_latency_ms: u64,
}

impl Default for RoutingPolicy {
	fn default() -> Self {
		Self {
			max_request_cost_usd: 2.0,
			max_latency_ms: 30_000,
		}
	}
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderResiliencePolicy {
	pub max_retries: u8,
	pub initial_backoff_ms: u64,
	pub max_backoff_ms: u64,
	pub circuit_breaker_failure_threshold: u32,
	pub circuit_breaker_cooldown_ms: u64,
}

impl Default for ProviderResiliencePolicy {
	fn default() -> Self {
		Self {
			max_retries: 5,
			initial_backoff_ms: 200,
			max_backoff_ms: 30_000,
			circuit_breaker_failure_threshold: 4,
			circuit_breaker_cooldown_ms: 30_000,
		}
	}
}

/// A single named block of system prompt content.
///
/// The `id` is a stable identifier that downstream cache adapters use to
/// decide which blocks to mark with `cache_control`. Ids are conventionally
/// short lowercase snake_case strings (`"identity"`, `"tool_guidance"`,
/// `"environment"`, `"project_instruction"`, `"memory"`, `"plan_mode"`) and
/// must be unique within a `SystemPromptSections` value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SystemPromptBlock {
	/// Stable identifier for this block.
	pub id: String,
	/// The rendered content for this block.
	pub content: String,
}

/// System prompt decomposed into static and dynamic groups.
///
/// **Static blocks** have byte-identical content across `cd` / timestamp /
/// runtime-variable changes within the same session. Prompt caching requires
/// a stable byte-identical prefix; blocks in this group are candidates for
/// `cache_control` markers.
///
/// **Dynamic blocks** contain content that legitimately varies per turn (e.g.
/// `working_directory`, per-turn environment probe output). They must never
/// be marked cacheable.
///
/// Call [`SystemPromptSections::flatten`] to obtain a single `String` that
/// is byte-for-byte identical to the legacy `build_system_prompt` output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SystemPromptSections {
	/// Blocks whose content is stable within a session.
	pub static_blocks: Vec<SystemPromptBlock>,
	/// Blocks whose content may change between turns.
	pub dynamic_blocks: Vec<SystemPromptBlock>,
}

impl SystemPromptSections {
	/// Flatten the sections into a single string in the same order that
	/// [`build_system_prompt`] produced: static blocks first, then dynamic
	/// blocks, joined by `"\n\n"`.
	///
	/// This is the backward-compatible rendering path used by provider
	/// adapters that do not yet implement block-aware serialization.
	///
	/// [`build_system_prompt`]: crate::roku_agent_runtime::system_prompt::build_system_prompt
	pub fn flatten(&self) -> String {
		self.static_blocks
			.iter()
			.chain(self.dynamic_blocks.iter())
			.map(|b| b.content.as_str())
			.collect::<Vec<_>>()
			.join("\n\n")
	}

	/// Returns `true` when there are no blocks at all.
	pub fn is_empty(&self) -> bool {
		self.static_blocks.is_empty() && self.dynamic_blocks.is_empty()
	}
}

/// A tool definition sent to the LLM for native tool_use / function calling.
///
/// Follows the OpenAI-compatible format used by OpenRouter:
/// `{"type": "function", "function": {"name": "...", "description": "...", "parameters": {...}}}`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDefinition {
	pub name: String,
	pub description: String,
	/// JSON Schema for the tool's input parameters.
	pub parameters: Value,
}

/// A tool call block returned by the LLM via native tool_use.
///
/// Maps from OpenAI format:
/// `{"id": "call_xxx", "type": "function", "function": {"name": "...", "arguments": "{...}"}}`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallBlock {
	pub id: String,
	pub name: String,
	pub arguments: Value,
}

/// Request body for the `/responses/compact` endpoint.
///
/// Fields follow the OpenAI Responses API compact contract. Fields that are
/// not applicable to compaction (`stream`, `store`, `include`,
/// `prompt_cache_key`, `max_output_tokens`) are intentionally omitted.
///
/// `input` uses the provider-neutral [`Message`] type; each provider
/// implementation is responsible for converting to its own wire format.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompactRequest {
	/// The model to use for compaction.
	pub model: String,
	/// Top-level system instructions (same role as `instructions` in normal requests).
	pub instructions: String,
	/// The conversation history to compact, in provider-neutral form.
	pub input: Vec<Message>,
	/// Tool definitions available in the conversation.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub tools: Vec<ToolDefinition>,
	/// Whether the model may call multiple tools in parallel.
	#[serde(default)]
	pub parallel_tool_calls: bool,
	/// Optional reasoning configuration (`effort`, `summary`).
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub reasoning: Option<Value>,
}

/// Token usage reported by the `/responses/compact` endpoint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct CompactUsageSummary {
	#[serde(default)]
	pub prompt_tokens: u64,
	#[serde(default)]
	pub output_tokens: u64,
	#[serde(default)]
	pub cached_input_tokens: u64,
}

/// Response from the `/responses/compact` endpoint.
///
/// `output` contains the compacted conversation in provider-neutral form;
/// each provider implementation is responsible for converting from its own
/// wire format back to [`Message`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompactResponse {
	/// Compacted conversation messages in provider-neutral form.
	pub output: Vec<Message>,
	/// Token usage for the compaction call.
	#[serde(default)]
	pub usage: CompactUsageSummary,
}

/// A provider-neutral conversation message for the turn loop.
///
/// System prompt is NOT a variant here — it stays in `GenerationRequest::system_prompt`
/// because providers handle it differently (Anthropic: top-level field, OpenAI: system message).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Message {
	User {
		content: String,
	},
	Assistant {
		text: String,
		#[serde(default, skip_serializing_if = "Vec::is_empty")]
		tool_calls: Vec<ToolCallBlock>,
	},
	ToolResult {
		tool_use_id: String,
		content: String,
		#[serde(default)]
		is_error: bool,
	},
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GenerationRequest {
	pub system_prompt: Option<String>,
	pub prompt: String,
	/// Conversation messages for multi-turn interactions. When present, providers
	/// use these instead of wrapping `prompt` as a single user message.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub messages: Option<Vec<Message>>,
	pub expected_output_tokens: u64,
	pub risk_tier: RiskTier,
	pub preferred_provider: Option<String>,
	pub budget_tokens_remaining: u64,
	pub budget_cost_remaining_usd: f64,
	/// Tool definitions for native tool_use. When Some, the provider should
	/// send these as the `tools` parameter and expect `tool_calls` in response.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub tools: Option<Vec<ToolDefinition>>,
	/// Override the model selected by the router. When Some, the router will
	/// prefer the specified model if it is registered.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub model_override: Option<String>,
	/// Controls how much extended thinking budget the provider should allocate.
	/// Defaults to no thinking when None or ThinkingEffort::None.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub thinking_effort: Option<ThinkingEffort>,
	/// Structured form of the system prompt, decomposed into static and dynamic
	/// blocks. When `Some`, this is the authoritative source; `system_prompt`
	/// holds the same content flattened for adapters that do not yet implement
	/// block-aware serialization. Later cache adapter units will replace the
	/// flatten call with block-level `cache_control` markers.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub system_prompt_sections: Option<SystemPromptSections>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmResponse {
	pub provider: String,
	pub model_id: String,
	pub output: String,
	#[serde(default)]
	pub finish_reason: Option<String>,
	pub prompt_tokens: u64,
	pub output_tokens: u64,
	pub total_tokens: u64,
	pub estimated_cost_usd: f64,
	pub latency_ms: u64,
	/// Tool call blocks from native tool_use response. Present when the model
	/// chose to call tools via the native protocol instead of generating text.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub tool_calls: Option<Vec<ToolCallBlock>>,
	/// Input tokens that were newly written to the provider's prompt cache
	/// on this turn. Anthropic reports this as `cache_creation_input_tokens`
	/// in `usage`. OpenAI does not expose a write-side counter, so this
	/// field stays `0` for every OpenAI turn.
	#[serde(default)]
	pub cache_creation_input_tokens: u64,
	/// Input tokens that were served from the provider's prompt cache on
	/// this turn. Anthropic reports this as `cache_read_input_tokens`;
	/// OpenAI reports the equivalent as `input_tokens_details.cached_tokens`
	/// (Responses API) or `prompt_tokens_details.cached_tokens` (Chat
	/// Completions). `0` when the provider did not report any cache hit or
	/// when the field is absent from the raw response.
	#[serde(default)]
	pub cache_read_input_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderResponse {
	pub output: String,
	#[serde(default)]
	pub finish_reason: Option<String>,
	pub prompt_tokens: u64,
	pub output_tokens: u64,
	pub latency_ms: u64,
	/// Tool call blocks from native tool_use. None when the model responded
	/// with text only.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub tool_calls: Option<Vec<ToolCallBlock>>,
	/// See [`LlmResponse::cache_creation_input_tokens`].
	#[serde(default)]
	pub cache_creation_input_tokens: u64,
	/// See [`LlmResponse::cache_read_input_tokens`].
	#[serde(default)]
	pub cache_read_input_tokens: u64,
	/// OpenAI Responses API response ID for delta mode (`previous_response_id`
	/// on the next turn). `None` for providers that do not return a response ID.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub response_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StructuredJsonResponse {
	pub response: LlmResponse,
	pub value: Value,
}

#[derive(Debug, Error, Clone, PartialEq)]
pub enum ProviderCallError {
	/// 429 Too Many Requests — rate limited by the provider.
	#[error("rate limited: {message}")]
	RateLimit {
		message: String,
		retry_after: Option<Duration>,
	},

	/// 529/503 — provider is overloaded.
	#[error("server overloaded: {message}")]
	ServerOverloaded {
		message: String,
		retry_after: Option<Duration>,
	},

	/// Context window exceeded (provider-specific 400 with context signal).
	#[error("context window exceeded: {detail}")]
	ContextWindowExceeded { detail: String },

	/// 500/502/504 and other server errors.
	#[error("server error ({status}): {message}")]
	ServerError { status: u16, message: String },

	/// Request timed out.
	#[error("request timed out: {message}")]
	Timeout { message: String },

	/// Network connection failed.
	#[error("connection failed: {message}")]
	ConnectionFailed { message: String },

	/// 401/403 — authentication or authorization failure.
	#[error("authentication failed: {message}")]
	AuthenticationFailed { message: String },

	/// 400 — invalid request (not context-related).
	#[error("invalid request: {message}")]
	InvalidRequest { message: String },

	/// Quota exhausted (different from rate limit — persistent, not temporary).
	#[error("quota exceeded: {message}")]
	QuotaExceeded { message: String },

	/// Unrecoverable error.
	#[error("fatal: {message}")]
	Fatal { message: String },
}

impl ProviderCallError {
	/// Whether this error class is eligible for retry.
	pub fn is_retryable(&self) -> bool {
		matches!(
			self,
			Self::RateLimit { .. }
				| Self::ServerOverloaded { .. }
				| Self::ServerError { .. }
				| Self::Timeout { .. }
				| Self::ConnectionFailed { .. }
		)
	}

	/// Server-suggested delay before retrying, if any.
	pub fn suggested_delay(&self) -> Option<Duration> {
		match self {
			Self::RateLimit { retry_after, .. } | Self::ServerOverloaded { retry_after, .. } => {
				*retry_after
			}
			_ => None,
		}
	}
}

#[derive(Debug, Error, Clone, PartialEq)]
pub enum LlmAdapterError {
	#[error("no model eligible for request")]
	NoEligibleModel,
	#[error("provider is not registered: {0}")]
	ProviderNotRegistered(String),
	#[error("request budget exceeded: {0}")]
	BudgetExceeded(String),
	#[error("request latency exceeded policy: latency={latency_ms}ms max={max_latency_ms}ms")]
	LatencyExceeded {
		latency_ms: u64,
		max_latency_ms: u64,
	},
	#[error("provider circuit is open for {provider}; retry after {retry_after_ms}ms")]
	CircuitOpen {
		provider: String,
		retry_after_ms: u64,
	},
	#[error("provider call failed for {provider}/{model_id}: {message}")]
	ProviderCallFailed {
		provider: String,
		model_id: String,
		message: String,
	},
	/// The provider rejected the request because the prompt exceeded the
	/// model's context window. Surfaced as its own variant (rather than
	/// stringified into [`Self::ProviderCallFailed`]) so callers in the
	/// runtime can react with compaction + retry.
	#[error("context window exceeded for {provider}/{model_id}: {detail}")]
	ContextWindowExceeded {
		provider: String,
		model_id: String,
		detail: String,
	},
}

#[derive(Debug, Error, Clone, PartialEq)]
pub enum StructuredOutputError {
	#[error("provider returned unreadable content")]
	UnreadableProviderContent,
	#[error("provider returned content = null")]
	NullContent,
	#[error("provider returned finish_reason = length")]
	FinishReasonLength,
	#[error("invalid json: {0}")]
	InvalidJson(String),
}

#[derive(Debug, Error, Clone, PartialEq)]
pub enum StructuredGenerationError {
	#[error(transparent)]
	Llm(#[from] LlmAdapterError),
	#[error(transparent)]
	ParseGuard(#[from] StructuredOutputError),
}

/// A single chunk emitted during a streaming LLM completion.
///
/// Callers receive a sequence of these via an `mpsc::Receiver<StreamChunk>`
/// and reassemble the full response from `TextDelta` events, then confirm
/// completion via `Done`.
///
/// Tool call variants (`ToolCallStart`, `ToolCallDelta`, `ToolCallDone`) are
/// emitted when the model responds with native tool_use during streaming.
/// Providers that support streaming tool_use emit these alongside or instead
/// of `TextDelta`. Downstream consumers can use these to begin tool
/// execution before the stream completes.
#[derive(Debug, Clone)]
pub enum StreamChunk {
	/// Incremental text content from the assistant.
	TextDelta { text: String },
	/// A new tool call block has started. Contains the tool call `id` and
	/// the tool `name`. Arguments will follow via `ToolCallDelta`.
	ToolCallStart { id: String, name: String },
	/// Incremental JSON arguments for an in-progress tool call.
	ToolCallDelta { id: String, arguments_chunk: String },
	/// A tool call block has finished. All argument deltas for this `id`
	/// have been sent.
	ToolCallDone { id: String },
	/// The stream has finished. `prompt_tokens` and `output_tokens` come
	/// from the final usage block in the SSE stream; they are zero when the
	/// provider does not include a usage event.
	Done {
		finish_reason: Option<String>,
		prompt_tokens: u64,
		output_tokens: u64,
	},
}

pub(crate) fn estimate_prompt_tokens(prompt: &str) -> u64 {
	u64::try_from(prompt.split_whitespace().count())
		.unwrap_or(u64::MAX)
		.max(1)
}

pub(crate) fn estimate_request_input_tokens(request: &GenerationRequest) -> u64 {
	let system_tokens = request
		.system_prompt
		.as_deref()
		.map(estimate_prompt_tokens)
		.unwrap_or(0);
	if let Some(messages) = &request.messages {
		let msg_tokens: u64 = messages
			.iter()
			.map(|m| match m {
				Message::User { content } => estimate_prompt_tokens(content),
				Message::Assistant { text, tool_calls } => {
					let text_tokens = estimate_prompt_tokens(text);
					let tool_tokens: u64 = tool_calls
						.iter()
						.map(|tc| {
							// Estimate: name + serialized arguments
							let arg_str = tc.arguments.to_string();
							estimate_prompt_tokens(&tc.name) + estimate_prompt_tokens(&arg_str)
						})
						.sum();
					text_tokens + tool_tokens
				}
				Message::ToolResult { content, .. } => estimate_prompt_tokens(content),
			})
			.sum();
		system_tokens.saturating_add(msg_tokens)
	} else {
		system_tokens.saturating_add(estimate_prompt_tokens(&request.prompt))
	}
}

pub(crate) fn estimate_cost_usd(tokens: u64, cost_per_1k_tokens_usd: f64) -> f64 {
	(tokens as f64 / 1000.0) * cost_per_1k_tokens_usd
}

#[cfg(test)]
mod tests {
	use super::*;
	use serde_json::json;

	fn make_request(messages: Vec<Message>) -> GenerationRequest {
		GenerationRequest {
			system_prompt: None,
			prompt: String::new(),
			messages: Some(messages),
			expected_output_tokens: 100,
			risk_tier: RiskTier::Low,
			preferred_provider: None,
			budget_tokens_remaining: 100_000,
			budget_cost_remaining_usd: 10.0,
			tools: None,
			model_override: None,
			thinking_effort: None,
			system_prompt_sections: None,
		}
	}

	#[test]
	fn assistant_with_tool_calls_has_higher_token_estimate_than_without() {
		let without = make_request(vec![Message::Assistant {
			text: "I will call the tool".to_string(),
			tool_calls: Vec::new(),
		}]);
		let with_calls = make_request(vec![Message::Assistant {
			text: "I will call the tool".to_string(),
			tool_calls: vec![ToolCallBlock {
				id: "call_1".to_string(),
				name: "Read".to_string(),
				arguments: json!({"path": "/some/file.txt"}),
			}],
		}]);
		assert!(
			estimate_request_input_tokens(&with_calls) > estimate_request_input_tokens(&without),
			"assistant message with tool_calls should have a higher token estimate"
		);
	}
}
