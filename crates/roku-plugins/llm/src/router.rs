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

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use roku_common_types::{LlmInvocationOutcome, Metrics};
use serde_json::Value;

use crate::types::{
	GenerationRequest, LlmAdapterError, LlmResponse, ModelProfile, ProviderCallError,
	ProviderResiliencePolicy, ProviderResponse, RiskTier, RoutingPolicy, StreamChunk,
	StructuredGenerationError, StructuredJsonResponse, StructuredOutputError, estimate_cost_usd,
};

#[async_trait]
pub trait LlmProvider: Send + Sync {
	fn provider_name(&self) -> &'static str;
	async fn complete(
		&self,
		model: &ModelProfile,
		request: &GenerationRequest,
	) -> Result<ProviderResponse, ProviderCallError>;

	/// Stream a generation request, sending [`StreamChunk`] events through `tx`.
	///
	/// Default implementation falls back to [`complete()`] and sends the full
	/// output as a single [`StreamChunk::TextDelta`] followed by [`StreamChunk::Done`].
	async fn stream(
		&self,
		model: &ModelProfile,
		request: &GenerationRequest,
		tx: tokio::sync::mpsc::Sender<StreamChunk>,
	) -> Result<ProviderResponse, ProviderCallError> {
		let response = self.complete(model, request).await?;
		let _ = tx
			.send(StreamChunk::TextDelta {
				text: response.output.clone(),
			})
			.await;
		let _ = tx
			.send(StreamChunk::Done {
				finish_reason: response.finish_reason.clone(),
				prompt_tokens: response.prompt_tokens,
				output_tokens: response.output_tokens,
			})
			.await;
		Ok(response)
	}
}

pub struct LlmRouter {
	policy: RoutingPolicy,
	models: Vec<ModelProfile>,
	providers: HashMap<String, RegisteredProvider>,
	metrics: Option<Arc<Metrics>>,
	resilience_policy: ProviderResiliencePolicy,
	/// Single-threaded tokio runtime used by the `_blocking()` bridge methods.
	/// Kept behind an `Arc` so `LlmRouter` can be cheaply cloned if needed.
	blocking_runtime: Arc<tokio::runtime::Runtime>,
}

impl Default for LlmRouter {
	fn default() -> Self {
		Self::new(RoutingPolicy::default())
	}
}

impl LlmRouter {
	pub fn new(policy: RoutingPolicy) -> Self {
		let blocking_runtime = tokio::runtime::Builder::new_current_thread()
			.enable_all()
			.build()
			.expect("LlmRouter blocking runtime must be constructible");
		Self {
			policy,
			models: Vec::new(),
			providers: HashMap::new(),
			metrics: None,
			resilience_policy: ProviderResiliencePolicy::default(),
			blocking_runtime: Arc::new(blocking_runtime),
		}
	}

	pub fn with_metrics(mut self, metrics: Arc<Metrics>) -> Self {
		self.metrics = Some(metrics);
		self
	}

	pub fn with_provider_resilience_policy(
		mut self,
		resilience_policy: ProviderResiliencePolicy,
	) -> Self {
		self.resilience_policy = resilience_policy;
		self
	}

	pub fn register_model(&mut self, model: ModelProfile) {
		self.models.push(model);
	}

	pub fn register_provider<P>(&mut self, provider: P)
	where
		P: LlmProvider + 'static,
	{
		self.providers.insert(
			provider.provider_name().to_string(),
			RegisteredProvider::new(provider),
		);
	}

	/// Async entrypoint: route a request and return the full response.
	pub async fn generate(
		&self,
		request: &GenerationRequest,
	) -> Result<LlmResponse, LlmAdapterError> {
		let selected_model = match self.select_model(request) {
			Ok(selected_model) => selected_model,
			Err(error) => {
				if let Some(metrics) = &self.metrics {
					metrics.record_llm_routing_failure();
				}
				return Err(error);
			}
		};
		let provider = self
			.providers
			.get(&selected_model.provider)
			.ok_or_else(|| LlmAdapterError::ProviderNotRegistered(selected_model.provider.clone()));
		let provider = match provider {
			Ok(provider) => provider,
			Err(error) => {
				self.record_failure(selected_model, 0, 0, 0, 0.0);
				return Err(error);
			}
		};

		let provider_response = self
			.complete_with_resilience(provider, selected_model, request)
			.await;
		let provider_response = match provider_response {
			Ok(provider_response) => provider_response,
			Err(error) => {
				self.record_failure(selected_model, 0, 0, 0, 0.0);
				return Err(error);
			}
		};

		if provider_response.latency_ms > self.policy.max_latency_ms {
			self.record_failure(
				selected_model,
				provider_response.prompt_tokens,
				provider_response.output_tokens,
				provider_response.latency_ms,
				estimate_cost_usd(
					provider_response
						.prompt_tokens
						.saturating_add(provider_response.output_tokens),
					selected_model.cost_per_1k_tokens_usd,
				),
			);
			return Err(LlmAdapterError::LatencyExceeded {
				latency_ms: provider_response.latency_ms,
				max_latency_ms: self.policy.max_latency_ms,
			});
		}

		let total_tokens = provider_response
			.prompt_tokens
			.saturating_add(provider_response.output_tokens);
		if total_tokens > request.budget_tokens_remaining {
			self.record_failure(
				selected_model,
				provider_response.prompt_tokens,
				provider_response.output_tokens,
				provider_response.latency_ms,
				estimate_cost_usd(total_tokens, selected_model.cost_per_1k_tokens_usd),
			);
			return Err(LlmAdapterError::BudgetExceeded(format!(
				"token budget exceeded: used={} budget={}",
				total_tokens, request.budget_tokens_remaining
			)));
		}

		let estimated_cost_usd =
			estimate_cost_usd(total_tokens, selected_model.cost_per_1k_tokens_usd);
		if estimated_cost_usd > request.budget_cost_remaining_usd
			|| estimated_cost_usd > self.policy.max_request_cost_usd
		{
			self.record_failure(
				selected_model,
				provider_response.prompt_tokens,
				provider_response.output_tokens,
				provider_response.latency_ms,
				estimated_cost_usd,
			);
			return Err(LlmAdapterError::BudgetExceeded(format!(
				"cost budget exceeded: cost={estimated_cost_usd:.4} budget={:.4} policy_max={:.4}",
				request.budget_cost_remaining_usd, self.policy.max_request_cost_usd
			)));
		}

		self.record_success(
			selected_model,
			provider_response.prompt_tokens,
			provider_response.output_tokens,
			provider_response.latency_ms,
			estimated_cost_usd,
		);
		Ok(LlmResponse {
			provider: selected_model.provider.clone(),
			model_id: selected_model.model_id.clone(),
			output: provider_response.output,
			finish_reason: provider_response.finish_reason,
			prompt_tokens: provider_response.prompt_tokens,
			output_tokens: provider_response.output_tokens,
			total_tokens,
			estimated_cost_usd,
			latency_ms: provider_response.latency_ms,
			tool_calls: provider_response.tool_calls.clone(),
		})
	}

	/// Async entrypoint: stream a request, sending [`StreamChunk`] events through `tx`.
	///
	/// Returns the final [`LlmResponse`] after the stream completes.
	pub async fn generate_streaming(
		&self,
		request: &GenerationRequest,
		tx: tokio::sync::mpsc::Sender<StreamChunk>,
	) -> Result<LlmResponse, LlmAdapterError> {
		const STREAMING_OVERALL_TIMEOUT: Duration = Duration::from_secs(300);

		let selected_model = self.select_model(request)?;
		let provider = self
			.providers
			.get(&selected_model.provider)
			.ok_or_else(|| {
				LlmAdapterError::ProviderNotRegistered(selected_model.provider.clone())
			})?;

		provider
			.allow_call(&self.resilience_policy)
			.map_err(|retry_after_ms| LlmAdapterError::CircuitOpen {
				provider: selected_model.provider.clone(),
				retry_after_ms,
			})?;

		let total_attempts = usize::from(self.resilience_policy.max_retries).saturating_add(1);
		let chunks_forwarded = Arc::new(AtomicU64::new(0));
		let mut last_error: Option<LlmAdapterError> = None;

		for attempt_index in 0..total_attempts {
			// Per-attempt channel pair; a forwarder task copies chunks to the
			// real `tx` while tracking how many have been sent.
			let (attempt_tx, mut attempt_rx) = tokio::sync::mpsc::channel::<StreamChunk>(32);
			let real_tx = tx.clone();
			let counter = Arc::clone(&chunks_forwarded);
			let forwarder = tokio::spawn(async move {
				while let Some(chunk) = attempt_rx.recv().await {
					counter.fetch_add(1, Ordering::Relaxed);
					if real_tx.send(chunk).await.is_err() {
						break;
					}
				}
			});

			let stream_result = tokio::time::timeout(
				STREAMING_OVERALL_TIMEOUT,
				provider
					.provider
					.stream(selected_model, request, attempt_tx),
			)
			.await;

			// Let the forwarder drain naturally — attempt_tx was moved into stream()
			// and is dropped when it returns, closing the channel. Aborting would
			// lose buffered chunks and undercount chunks_forwarded.
			let _ = forwarder.await;

			match stream_result {
				Ok(Ok(response)) => {
					provider.record_success();
					let total_tokens = response
						.prompt_tokens
						.saturating_add(response.output_tokens);
					let estimated_cost_usd =
						estimate_cost_usd(total_tokens, selected_model.cost_per_1k_tokens_usd);
					return Ok(LlmResponse {
						provider: selected_model.provider.clone(),
						model_id: selected_model.model_id.clone(),
						output: response.output,
						finish_reason: response.finish_reason,
						prompt_tokens: response.prompt_tokens,
						output_tokens: response.output_tokens,
						total_tokens,
						estimated_cost_usd,
						latency_ms: response.latency_ms,
						tool_calls: response.tool_calls.clone(),
					});
				}
				Ok(Err(error)) => {
					let attempts_used = attempt_index.saturating_add(1);
					let opened_circuit = provider.record_failure(&self.resilience_policy);
					let any_chunks = chunks_forwarded.load(Ordering::Relaxed) > 0;

					// Never retry once chunks have been forwarded (would duplicate content).
					if any_chunks
						|| !error.is_retryable()
						|| opened_circuit || attempts_used >= total_attempts
					{
						return Err(LlmAdapterError::ProviderCallFailed {
							provider: selected_model.provider.clone(),
							model_id: selected_model.model_id.clone(),
							message: format!("{error} after {attempts_used} attempts"),
						});
					}

					let backoff_ms = backoff_for_attempt(attempt_index, &self.resilience_policy);
					if backoff_ms > 0 {
						tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
					}
					last_error = Some(LlmAdapterError::ProviderCallFailed {
						provider: selected_model.provider.clone(),
						model_id: selected_model.model_id.clone(),
						message: format!("{error} after {attempts_used} attempts"),
					});
				}
				Err(_elapsed) => {
					let attempts_used = attempt_index.saturating_add(1);
					let opened_circuit = provider.record_failure(&self.resilience_policy);
					let any_chunks = chunks_forwarded.load(Ordering::Relaxed) > 0;

					// Treat timeout like a retryable error — retry if no chunks sent.
					if any_chunks || opened_circuit || attempts_used >= total_attempts {
						return Err(LlmAdapterError::ProviderCallFailed {
							provider: selected_model.provider.clone(),
							model_id: selected_model.model_id.clone(),
							message: format!(
								"streaming overall timeout exceeded (300s) after {attempts_used} attempts"
							),
						});
					}

					let backoff_ms =
						backoff_for_attempt(attempt_index, &self.resilience_policy);
					if backoff_ms > 0 {
						tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
					}
					last_error = Some(LlmAdapterError::ProviderCallFailed {
						provider: selected_model.provider.clone(),
						model_id: selected_model.model_id.clone(),
						message: format!(
							"streaming overall timeout exceeded (300s) after {attempts_used} attempts"
						),
					});
				}
			}
		}

		Err(
			last_error.unwrap_or_else(|| LlmAdapterError::ProviderCallFailed {
				provider: selected_model.provider.clone(),
				model_id: selected_model.model_id.clone(),
				message: "streaming call exhausted attempts".to_string(),
			}),
		)
	}

	/// Async entrypoint: generate and parse a structured JSON response.
	pub async fn generate_json_value(
		&self,
		request: &GenerationRequest,
	) -> Result<StructuredJsonResponse, StructuredGenerationError> {
		let response = match self.generate(request).await {
			Ok(response) => response,
			Err(error) => return Err(map_structured_generation_error(error)),
		};
		if response.finish_reason.as_deref() == Some("length") {
			return Err(StructuredOutputError::FinishReasonLength.into());
		}
		let payload = extract_json_payload(&response.output);
		let value = serde_json::from_str::<Value>(payload)
			.map_err(|error| StructuredOutputError::InvalidJson(error.to_string()))?;
		Ok(StructuredJsonResponse { response, value })
	}

	/// Synchronous bridge: blocks until `generate` completes.
	///
	/// Used by callers that are not yet async (Unit 1 bridge; removed in Unit
	/// 3 when the full call chain is async).
	pub fn generate_blocking(
		&self,
		request: &GenerationRequest,
	) -> Result<LlmResponse, LlmAdapterError> {
		// When called from inside an existing tokio runtime (e.g. actix-web
		// handlers), block_on panics with "Cannot start a runtime from within a
		// runtime". Spawn a scoped helper thread to avoid this.
		if tokio::runtime::Handle::try_current().is_ok() {
			let rt = self.blocking_runtime.clone();
			std::thread::scope(|s| {
				s.spawn(|| rt.block_on(self.generate(request)))
					.join()
					.unwrap()
			})
		} else {
			self.blocking_runtime.block_on(self.generate(request))
		}
	}

	/// Synchronous bridge: blocks until `generate_json_value` completes.
	pub fn generate_json_value_blocking(
		&self,
		request: &GenerationRequest,
	) -> Result<StructuredJsonResponse, StructuredGenerationError> {
		if tokio::runtime::Handle::try_current().is_ok() {
			let rt = self.blocking_runtime.clone();
			std::thread::scope(|s| {
				s.spawn(|| rt.block_on(self.generate_json_value(request)))
					.join()
					.unwrap()
			})
		} else {
			self.blocking_runtime
				.block_on(self.generate_json_value(request))
		}
	}

	async fn complete_with_resilience(
		&self,
		provider: &RegisteredProvider,
		model: &ModelProfile,
		request: &GenerationRequest,
	) -> Result<ProviderResponse, LlmAdapterError> {
		provider
			.allow_call(&self.resilience_policy)
			.map_err(|retry_after_ms| LlmAdapterError::CircuitOpen {
				provider: model.provider.clone(),
				retry_after_ms,
			})?;

		let total_attempts = usize::from(self.resilience_policy.max_retries).saturating_add(1);
		for attempt_index in 0..total_attempts {
			match provider.provider.complete(model, request).await {
				Ok(response) => {
					provider.record_success();
					return Ok(response);
				}
				Err(error) => {
					let attempts_used = attempt_index.saturating_add(1);
					let opened_circuit = provider.record_failure(&self.resilience_policy);
					if !error.is_retryable() {
						return Err(LlmAdapterError::ProviderCallFailed {
							provider: model.provider.clone(),
							model_id: model.model_id.clone(),
							message: format!("{error} after {attempts_used} attempts"),
						});
					}
					if opened_circuit || attempts_used >= total_attempts {
						let failure_reason = if opened_circuit {
							format!(
								"{error} after {attempts_used} attempts; provider circuit opened"
							)
						} else {
							format!("{error} after {attempts_used} attempts")
						};
						return Err(LlmAdapterError::ProviderCallFailed {
							provider: model.provider.clone(),
							model_id: model.model_id.clone(),
							message: failure_reason,
						});
					}

					let backoff_ms = backoff_for_attempt(attempt_index, &self.resilience_policy);
					if backoff_ms > 0 {
						tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
					}
				}
			}
		}

		Err(LlmAdapterError::ProviderCallFailed {
			provider: model.provider.clone(),
			model_id: model.model_id.clone(),
			message: "provider call exhausted attempts".to_string(),
		})
	}

	fn record_success(
		&self,
		model: &ModelProfile,
		prompt_tokens: u64,
		output_tokens: u64,
		latency_ms: u64,
		estimated_cost_usd: f64,
	) {
		if let Some(metrics) = &self.metrics {
			metrics.record_llm_invocation(
				&model.provider,
				&model.model_id,
				LlmInvocationOutcome::Success,
				prompt_tokens,
				output_tokens,
				latency_ms,
				estimated_cost_usd,
			);
		}
	}

	fn record_failure(
		&self,
		model: &ModelProfile,
		prompt_tokens: u64,
		output_tokens: u64,
		latency_ms: u64,
		estimated_cost_usd: f64,
	) {
		if let Some(metrics) = &self.metrics {
			metrics.record_llm_invocation(
				&model.provider,
				&model.model_id,
				LlmInvocationOutcome::Failure,
				prompt_tokens,
				output_tokens,
				latency_ms,
				estimated_cost_usd,
			);
		}
	}

	fn select_model(&self, request: &GenerationRequest) -> Result<&ModelProfile, LlmAdapterError> {
		// If model_override is set and the model passes eligibility checks
		// (risk tier, token budget, cost budget), prefer it over normal routing.
		if let Some(override_id) = &request.model_override
			&& let Some(model) = self.models.iter().find(|m| &m.model_id == override_id)
			&& model.supports(request)
		{
			return Ok(model);
		}

		let mut eligible = self
			.models
			.iter()
			.filter(|model| model.supports(request))
			.collect::<Vec<_>>();
		if eligible.is_empty() {
			return Err(LlmAdapterError::NoEligibleModel);
		}

		eligible.sort_by(|left, right| {
			let risk_order = right.max_risk_tier.cmp(&left.max_risk_tier);
			if matches!(request.risk_tier, RiskTier::High | RiskTier::Critical) {
				risk_order
					.then_with(|| right.route_priority.cmp(&left.route_priority))
					.then_with(|| {
						left.cost_per_1k_tokens_usd
							.partial_cmp(&right.cost_per_1k_tokens_usd)
							.unwrap_or(std::cmp::Ordering::Equal)
					})
			} else {
				left.cost_per_1k_tokens_usd
					.partial_cmp(&right.cost_per_1k_tokens_usd)
					.unwrap_or(std::cmp::Ordering::Equal)
					.then_with(|| right.route_priority.cmp(&left.route_priority))
					.then_with(|| risk_order)
			}
		});

		eligible
			.into_iter()
			.next()
			.ok_or(LlmAdapterError::NoEligibleModel)
	}

	/// Returns a list of all registered model IDs.
	pub fn available_models(&self) -> Vec<String> {
		self.models.iter().map(|m| m.model_id.clone()).collect()
	}
}

fn map_structured_generation_error(error: LlmAdapterError) -> StructuredGenerationError {
	match &error {
		LlmAdapterError::ProviderCallFailed { message, .. } => {
			if message.contains("provider_unreadable_content:") {
				return StructuredOutputError::UnreadableProviderContent.into();
			}
			if message.contains("provider_content_null:") {
				return StructuredOutputError::NullContent.into();
			}
			if message.contains("provider_finish_reason_length:") {
				return StructuredOutputError::FinishReasonLength.into();
			}
			StructuredGenerationError::Llm(error)
		}
		_ => StructuredGenerationError::Llm(error),
	}
}

fn extract_json_payload(output: &str) -> &str {
	let trimmed = output.trim();
	if let Some(stripped) = trimmed.strip_prefix("```") {
		return stripped
			.strip_prefix("json")
			.map(str::trim_start)
			.unwrap_or(stripped)
			.strip_suffix("```")
			.map(str::trim)
			.unwrap_or(stripped);
	}
	trimmed
}

struct RegisteredProvider {
	provider: Arc<dyn LlmProvider>,
	state: Mutex<CircuitBreakerState>,
}

impl RegisteredProvider {
	fn new<P>(provider: P) -> Self
	where
		P: LlmProvider + 'static,
	{
		Self {
			provider: Arc::new(provider),
			state: Mutex::new(CircuitBreakerState::Closed {
				consecutive_failures: 0,
			}),
		}
	}

	fn allow_call(&self, policy: &ProviderResiliencePolicy) -> Result<(), u64> {
		let mut state = self
			.state
			.lock()
			.expect("provider circuit breaker state lock must not be poisoned");
		let now = Instant::now();
		match *state {
			CircuitBreakerState::Closed { .. } => Ok(()),
			CircuitBreakerState::HalfOpen => Err(0),
			CircuitBreakerState::Open { retry_at } => {
				if now >= retry_at {
					*state = CircuitBreakerState::HalfOpen;
					Ok(())
				} else {
					let retry_after_ms = retry_at
						.saturating_duration_since(now)
						.as_millis()
						.try_into()
						.unwrap_or(u64::MAX);
					if policy.circuit_breaker_cooldown_ms == 0 {
						*state = CircuitBreakerState::HalfOpen;
						Ok(())
					} else {
						Err(retry_after_ms)
					}
				}
			}
		}
	}

	fn record_success(&self) {
		let mut state = self
			.state
			.lock()
			.expect("provider circuit breaker state lock must not be poisoned");
		*state = CircuitBreakerState::Closed {
			consecutive_failures: 0,
		};
	}

	fn record_failure(&self, policy: &ProviderResiliencePolicy) -> bool {
		if policy.circuit_breaker_failure_threshold == 0 {
			return false;
		}

		let mut state = self
			.state
			.lock()
			.expect("provider circuit breaker state lock must not be poisoned");
		let now = Instant::now();
		match *state {
			CircuitBreakerState::HalfOpen => {
				*state = CircuitBreakerState::Open {
					retry_at: now + Duration::from_millis(policy.circuit_breaker_cooldown_ms),
				};
				true
			}
			CircuitBreakerState::Open { .. } => true,
			CircuitBreakerState::Closed {
				ref mut consecutive_failures,
			} => {
				*consecutive_failures = consecutive_failures.saturating_add(1);
				if *consecutive_failures >= policy.circuit_breaker_failure_threshold {
					*state = CircuitBreakerState::Open {
						retry_at: now + Duration::from_millis(policy.circuit_breaker_cooldown_ms),
					};
					true
				} else {
					false
				}
			}
		}
	}
}

#[derive(Clone, Copy)]
enum CircuitBreakerState {
	Closed { consecutive_failures: u32 },
	Open { retry_at: Instant },
	HalfOpen,
}

fn backoff_for_attempt(attempt_index: usize, policy: &ProviderResiliencePolicy) -> u64 {
	if policy.initial_backoff_ms == 0 {
		return 0;
	}

	let multiplier = 2_u64.saturating_pow(u32::try_from(attempt_index).unwrap_or(u32::MAX));
	policy
		.initial_backoff_ms
		.saturating_mul(multiplier)
		.min(policy.max_backoff_ms)
}

#[cfg(test)]
mod tests {
	use std::collections::VecDeque;
	use std::sync::Arc;
	use std::sync::Mutex;
	use std::sync::atomic::{AtomicUsize, Ordering};

	use async_trait::async_trait;
	use roku_common_types::Metrics;

	use crate::types::{
		GenerationRequest, ModelProfile, ProviderCallError, ProviderResiliencePolicy,
		ProviderResponse, RiskTier, RoutingPolicy,
	};

	use super::*;

	struct StaticProvider {
		name: &'static str,
		output: &'static str,
		prompt_tokens: u64,
		output_tokens: u64,
		latency_ms: u64,
	}

	#[async_trait]
	impl LlmProvider for StaticProvider {
		fn provider_name(&self) -> &'static str {
			self.name
		}

		async fn complete(
			&self,
			_model: &ModelProfile,
			_request: &GenerationRequest,
		) -> Result<ProviderResponse, ProviderCallError> {
			Ok(ProviderResponse {
				output: self.output.to_string(),
				finish_reason: None,
				prompt_tokens: self.prompt_tokens,
				output_tokens: self.output_tokens,
				latency_ms: self.latency_ms,
				tool_calls: None,
			})
		}
	}

	fn sample_request(risk_tier: RiskTier) -> GenerationRequest {
		GenerationRequest {
			system_prompt: None,
			prompt: "summarize project risks".to_string(),
			messages: None,
			expected_output_tokens: 300,
			risk_tier,
			preferred_provider: None,
			budget_tokens_remaining: 4_000,
			budget_cost_remaining_usd: 2.0,
			tools: None,
			model_override: None,
			thinking_effort: None,
		}
	}

	fn router_with_models() -> LlmRouter {
		let mut router = LlmRouter::new(RoutingPolicy {
			max_request_cost_usd: 1.0,
			max_latency_ms: 10_000,
		});
		router.register_provider(StaticProvider {
			name: "provider-a",
			output: "a-response",
			prompt_tokens: 120,
			output_tokens: 220,
			latency_ms: 200,
		});
		router.register_provider(StaticProvider {
			name: "provider-b",
			output: "b-response",
			prompt_tokens: 130,
			output_tokens: 240,
			latency_ms: 250,
		});

		router.register_model(ModelProfile {
			model_id: "a-lite".to_string(),
			provider: "provider-a".to_string(),
			max_context_tokens: 8_000,
			cost_per_1k_tokens_usd: 0.05,
			max_risk_tier: RiskTier::Medium,
			route_priority: 20,
		});
		router.register_model(ModelProfile {
			model_id: "b-pro".to_string(),
			provider: "provider-b".to_string(),
			max_context_tokens: 16_000,
			cost_per_1k_tokens_usd: 0.20,
			max_risk_tier: RiskTier::Critical,
			route_priority: 90,
		});
		router
	}

	struct SequenceProvider {
		name: &'static str,
		responses: Mutex<VecDeque<Result<ProviderResponse, ProviderCallError>>>,
		invocations: AtomicUsize,
	}

	impl SequenceProvider {
		fn new(
			name: &'static str,
			responses: Vec<Result<ProviderResponse, ProviderCallError>>,
		) -> Self {
			Self {
				name,
				responses: Mutex::new(responses.into()),
				invocations: AtomicUsize::new(0),
			}
		}

		fn invocations(&self) -> usize {
			self.invocations.load(Ordering::SeqCst)
		}
	}

	#[async_trait]
	impl LlmProvider for Arc<SequenceProvider> {
		fn provider_name(&self) -> &'static str {
			self.name
		}

		async fn complete(
			&self,
			_model: &ModelProfile,
			_request: &GenerationRequest,
		) -> Result<ProviderResponse, ProviderCallError> {
			self.invocations.fetch_add(1, Ordering::SeqCst);
			self.responses
				.lock()
				.expect("sequence provider responses lock must not be poisoned")
				.pop_front()
				.unwrap_or_else(|| {
					Err(ProviderCallError::non_retryable(
						"no more responses configured",
					))
				})
		}
	}

	fn model_profile(provider: &str) -> ModelProfile {
		ModelProfile {
			model_id: format!("{provider}-model"),
			provider: provider.to_string(),
			max_context_tokens: 8_000,
			cost_per_1k_tokens_usd: 0.01,
			max_risk_tier: RiskTier::Critical,
			route_priority: 100,
		}
	}

	#[test]
	fn high_risk_request_prefers_high_risk_model() {
		let router = router_with_models();
		let request = sample_request(RiskTier::High);

		let response = router
			.generate_blocking(&request)
			.expect("generation should succeed");
		assert_eq!(response.provider, "provider-b");
		assert_eq!(response.model_id, "b-pro");
	}

	#[test]
	fn low_risk_request_prefers_lower_cost_model() {
		let router = router_with_models();
		let request = sample_request(RiskTier::Low);

		let response = router
			.generate_blocking(&request)
			.expect("generation should succeed");
		assert_eq!(response.provider, "provider-a");
		assert_eq!(response.model_id, "a-lite");
	}

	#[test]
	fn preferred_provider_is_honored_when_eligible() {
		let router = router_with_models();
		let mut request = sample_request(RiskTier::Low);
		request.preferred_provider = Some("provider-b".to_string());

		let response = router
			.generate_blocking(&request)
			.expect("generation should succeed");
		assert_eq!(response.provider, "provider-b");
	}

	#[test]
	fn request_fails_when_budget_cost_is_too_low() {
		let router = router_with_models();
		let mut request = sample_request(RiskTier::Low);
		request.budget_cost_remaining_usd = 0.0001;

		let error = router
			.generate_blocking(&request)
			.expect_err("request should fail due to budget");
		assert!(matches!(error, LlmAdapterError::NoEligibleModel));
	}

	#[test]
	fn request_fails_when_provider_latency_exceeds_policy() {
		let mut router = LlmRouter::new(RoutingPolicy {
			max_request_cost_usd: 1.0,
			max_latency_ms: 100,
		});
		router.register_provider(StaticProvider {
			name: "provider-a",
			output: "late-response",
			prompt_tokens: 100,
			output_tokens: 100,
			latency_ms: 150,
		});
		router.register_model(ModelProfile {
			model_id: "a-lite".to_string(),
			provider: "provider-a".to_string(),
			max_context_tokens: 8_000,
			cost_per_1k_tokens_usd: 0.05,
			max_risk_tier: RiskTier::High,
			route_priority: 10,
		});

		let error = router
			.generate_blocking(&sample_request(RiskTier::Medium))
			.expect_err("request should fail due to latency");
		assert!(matches!(
			error,
			LlmAdapterError::LatencyExceeded {
				latency_ms: 150,
				max_latency_ms: 100
			}
		));
	}

	#[test]
	fn router_records_provider_metrics() {
		let metrics = Arc::new(Metrics::default());
		let router = router_with_models().with_metrics(metrics.clone());

		router
			.generate_blocking(&sample_request(RiskTier::Low))
			.expect("generation should succeed");

		let snapshot = metrics.snapshot();
		assert_eq!(snapshot.llm_requests_total, 1);
		assert_eq!(snapshot.llm_successes_total, 1);
		assert_eq!(snapshot.llm_failures_total, 0);
		assert_eq!(snapshot.llm_prompt_tokens_total, 120);
		assert_eq!(snapshot.llm_output_tokens_total, 220);

		let provider_metrics = metrics.llm_provider_metrics();
		assert_eq!(provider_metrics.len(), 1);
		assert_eq!(provider_metrics[0].provider, "provider-a");
		assert_eq!(provider_metrics[0].model_id, "a-lite");
	}

	#[test]
	fn router_records_routing_failures_when_no_model_is_eligible() {
		let metrics = Arc::new(Metrics::default());
		let router = router_with_models().with_metrics(metrics.clone());
		let mut request = sample_request(RiskTier::Critical);
		request.budget_cost_remaining_usd = 0.00001;

		let error = router
			.generate_blocking(&request)
			.expect_err("request should fail without an eligible model");
		assert!(matches!(error, LlmAdapterError::NoEligibleModel));

		let snapshot = metrics.snapshot();
		assert_eq!(snapshot.llm_requests_total, 1);
		assert_eq!(snapshot.llm_failures_total, 1);
		assert_eq!(snapshot.llm_routing_failures_total, 1);
		assert!(metrics.llm_provider_metrics().is_empty());
	}

	#[test]
	fn router_retries_retryable_provider_errors_before_succeeding() {
		let provider = Arc::new(SequenceProvider::new(
			"retrying-provider",
			vec![
				Err(ProviderCallError::retryable("transient upstream failure")),
				Ok(ProviderResponse {
					output: "recovered".to_string(),
					finish_reason: None,
					prompt_tokens: 40,
					output_tokens: 12,
					latency_ms: 80,
					tool_calls: None,
				}),
			],
		));
		let mut router = LlmRouter::new(RoutingPolicy::default()).with_provider_resilience_policy(
			ProviderResiliencePolicy {
				max_retries: 2,
				initial_backoff_ms: 0,
				max_backoff_ms: 0,
				circuit_breaker_failure_threshold: 4,
				circuit_breaker_cooldown_ms: 0,
			},
		);
		router.register_provider(Arc::clone(&provider));
		router.register_model(model_profile("retrying-provider"));

		let response = router
			.generate_blocking(&sample_request(RiskTier::Low))
			.expect("request should recover after retry");
		assert_eq!(response.output, "recovered");
		assert_eq!(provider.invocations(), 2);
	}

	#[test]
	fn router_does_not_retry_non_retryable_provider_errors() {
		let provider = Arc::new(SequenceProvider::new(
			"non-retrying-provider",
			vec![Err(ProviderCallError::non_retryable(
				"invalid request payload",
			))],
		));
		let mut router = LlmRouter::new(RoutingPolicy::default()).with_provider_resilience_policy(
			ProviderResiliencePolicy {
				max_retries: 2,
				initial_backoff_ms: 0,
				max_backoff_ms: 0,
				circuit_breaker_failure_threshold: 4,
				circuit_breaker_cooldown_ms: 0,
			},
		);
		router.register_provider(Arc::clone(&provider));
		router.register_model(model_profile("non-retrying-provider"));

		let error = router
			.generate_blocking(&sample_request(RiskTier::Low))
			.expect_err("request should fail immediately");
		assert!(matches!(error, LlmAdapterError::ProviderCallFailed { .. }));
		assert_eq!(provider.invocations(), 1);
	}

	#[test]
	fn router_opens_circuit_after_threshold_and_short_circuits_subsequent_calls() {
		let provider = Arc::new(SequenceProvider::new(
			"breaker-provider",
			vec![
				Err(ProviderCallError::retryable("temporary overload")),
				Err(ProviderCallError::retryable("temporary overload")),
			],
		));
		let mut router = LlmRouter::new(RoutingPolicy::default()).with_provider_resilience_policy(
			ProviderResiliencePolicy {
				max_retries: 0,
				initial_backoff_ms: 0,
				max_backoff_ms: 0,
				circuit_breaker_failure_threshold: 2,
				circuit_breaker_cooldown_ms: 60_000,
			},
		);
		router.register_provider(Arc::clone(&provider));
		router.register_model(model_profile("breaker-provider"));

		router
			.generate_blocking(&sample_request(RiskTier::Low))
			.expect_err("first request should fail");
		let second_error = router
			.generate_blocking(&sample_request(RiskTier::Low))
			.expect_err("second request should open the circuit");
		assert!(matches!(
			second_error,
			LlmAdapterError::ProviderCallFailed { .. }
		));

		let third_error = router
			.generate_blocking(&sample_request(RiskTier::Low))
			.expect_err("third request should be short-circuited");
		assert!(matches!(
			third_error,
			LlmAdapterError::CircuitOpen { provider, .. } if provider == "breaker-provider"
		));
		assert_eq!(provider.invocations(), 2);
	}

	#[test]
	fn router_allows_probe_after_circuit_cooldown_and_closes_on_success() {
		let provider = Arc::new(SequenceProvider::new(
			"recovering-provider",
			vec![
				Err(ProviderCallError::retryable("temporary overload")),
				Ok(ProviderResponse {
					output: "healthy-again".to_string(),
					finish_reason: None,
					prompt_tokens: 30,
					output_tokens: 10,
					latency_ms: 60,
					tool_calls: None,
				}),
			],
		));
		let mut router = LlmRouter::new(RoutingPolicy::default()).with_provider_resilience_policy(
			ProviderResiliencePolicy {
				max_retries: 0,
				initial_backoff_ms: 0,
				max_backoff_ms: 0,
				circuit_breaker_failure_threshold: 1,
				circuit_breaker_cooldown_ms: 0,
			},
		);
		router.register_provider(Arc::clone(&provider));
		router.register_model(model_profile("recovering-provider"));

		router
			.generate_blocking(&sample_request(RiskTier::Low))
			.expect_err("first request should open the circuit");
		let response = router
			.generate_blocking(&sample_request(RiskTier::Low))
			.expect("second request should probe and close the circuit");
		assert_eq!(response.output, "healthy-again");
		assert_eq!(provider.invocations(), 2);
	}

	#[test]
	fn generate_json_value_parses_structured_output() {
		let mut router = LlmRouter::new(RoutingPolicy::default());
		router.register_provider(StaticProvider {
			name: "json-provider",
			output: r#"{"intent_family":"chat","confidence":0.9}"#,
			prompt_tokens: 12,
			output_tokens: 8,
			latency_ms: 25,
		});
		router.register_model(ModelProfile {
			model_id: "json-model".to_string(),
			provider: "json-provider".to_string(),
			max_context_tokens: 8_000,
			cost_per_1k_tokens_usd: 0.0,
			max_risk_tier: RiskTier::Low,
			route_priority: 100,
		});

		let response = router
			.generate_json_value_blocking(&sample_request(RiskTier::Low))
			.expect("structured json should parse");
		assert_eq!(response.value["intent_family"], "chat");
	}

	#[test]
	fn generate_json_value_rejects_truncated_finish_reason() {
		struct TruncatedProvider;

		#[async_trait]
		impl LlmProvider for TruncatedProvider {
			fn provider_name(&self) -> &'static str {
				"truncated-provider"
			}

			async fn complete(
				&self,
				_model: &ModelProfile,
				_request: &GenerationRequest,
			) -> Result<ProviderResponse, ProviderCallError> {
				Ok(ProviderResponse {
					output: r#"{"intent_family":"chat"}"#.to_string(),
					finish_reason: Some("length".to_string()),
					prompt_tokens: 10,
					output_tokens: 5,
					latency_ms: 20,
					tool_calls: None,
				})
			}
		}

		let mut router = LlmRouter::new(RoutingPolicy::default());
		router.register_provider(TruncatedProvider);
		router.register_model(ModelProfile {
			model_id: "truncated-model".to_string(),
			provider: "truncated-provider".to_string(),
			max_context_tokens: 8_000,
			cost_per_1k_tokens_usd: 0.0,
			max_risk_tier: RiskTier::Low,
			route_priority: 100,
		});

		let error = router
			.generate_json_value_blocking(&sample_request(RiskTier::Low))
			.expect_err("finish_reason=length must be rejected");
		assert!(matches!(
			error,
			StructuredGenerationError::ParseGuard(StructuredOutputError::FinishReasonLength)
		));
	}
}
