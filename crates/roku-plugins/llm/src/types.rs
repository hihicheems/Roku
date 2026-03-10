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
			max_retries: 2,
			initial_backoff_ms: 200,
			max_backoff_ms: 1_000,
			circuit_breaker_failure_threshold: 4,
			circuit_breaker_cooldown_ms: 30_000,
		}
	}
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GenerationRequest {
	pub system_prompt: Option<String>,
	pub prompt: String,
	pub expected_output_tokens: u64,
	pub risk_tier: RiskTier,
	pub preferred_provider: Option<String>,
	pub budget_tokens_remaining: u64,
	pub budget_cost_remaining_usd: f64,
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
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderResponse {
	pub output: String,
	#[serde(default)]
	pub finish_reason: Option<String>,
	pub prompt_tokens: u64,
	pub output_tokens: u64,
	pub latency_ms: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StructuredJsonResponse {
	pub response: LlmResponse,
	pub value: Value,
}

#[derive(Debug, Error, Clone, PartialEq)]
pub enum ProviderCallError {
	#[error("{message}")]
	Retryable { message: String },
	#[error("{message}")]
	NonRetryable { message: String },
}

impl ProviderCallError {
	pub fn retryable(message: impl Into<String>) -> Self {
		Self::Retryable {
			message: message.into(),
		}
	}

	pub fn non_retryable(message: impl Into<String>) -> Self {
		Self::NonRetryable {
			message: message.into(),
		}
	}

	pub fn is_retryable(&self) -> bool {
		matches!(self, Self::Retryable { .. })
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

pub(crate) fn estimate_prompt_tokens(prompt: &str) -> u64 {
	u64::try_from(prompt.split_whitespace().count())
		.unwrap_or(u64::MAX)
		.max(1)
}

pub(crate) fn estimate_request_input_tokens(request: &GenerationRequest) -> u64 {
	request
		.system_prompt
		.as_deref()
		.map(estimate_prompt_tokens)
		.unwrap_or(0)
		.saturating_add(estimate_prompt_tokens(&request.prompt))
}

pub(crate) fn estimate_cost_usd(tokens: u64, cost_per_1k_tokens_usd: f64) -> f64 {
	(tokens as f64 / 1000.0) * cost_per_1k_tokens_usd
}
