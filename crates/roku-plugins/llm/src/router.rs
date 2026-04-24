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

use crate::retry::backoff_for_attempt;
use crate::token_counter::{TokenCounter, default_counter};
use crate::types::{
	CompactRequest, CompactResponse, GenerationRequest, LlmAdapterError, LlmResponse, ModelProfile,
	ProviderCallError, ProviderResiliencePolicy, ProviderResponse, RiskTier, RoutingPolicy,
	StreamChunk, StructuredGenerationError, StructuredJsonResponse, StructuredOutputError,
	ToolDefinition, estimate_cost_usd,
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

	/// Returns `true` when this provider can handle a [`compact_history`] call.
	///
	/// Default is `false`. Providers that implement the compact endpoint
	/// override this to `true` so that the router can check capability
	/// synchronously before making an async call.
	fn supports_compact_history(&self) -> bool {
		false
	}

	/// Attempt remote context compaction via a dedicated compact endpoint.
	///
	/// Returns `None` when the provider does not support remote compaction
	/// (the default). Providers that implement a compact endpoint return
	/// `Some(Ok(...))` on success or `Some(Err(...))` on failure.
	///
	/// Callers should fall back to local LLM-assisted summarization when
	/// this returns `None`, and to mechanical compaction when `Some(Err(...))`.
	async fn compact_history(
		&self,
		_request: &CompactRequest,
	) -> Option<Result<CompactResponse, ProviderCallError>> {
		None
	}

	/// Whether this provider supports client-side output-slot escalation
	/// (retrying with `max_output_tokens` set to the model ceiling when
	/// `finish_reason == "max_tokens"`).
	///
	/// Returns `true` by default. Providers where the server controls the
	/// output budget (e.g. the OpenAI Responses API link) return `false`.
	fn supports_output_slot_cap(&self) -> bool {
		true
	}

	/// Produce the exact byte sequence this provider would send on the wire
	/// for the `tools` field if given `definitions`.
	///
	/// The default implementation returns the Roku-internal canonical form
	/// (`serde_json::to_vec(definitions)`). Live providers override to match
	/// the provider's actual wire format (for example, OpenAI Chat Completions
	/// wraps each entry in `{"type":"function","function":{...}}`, while
	/// Anthropic uses `input_schema` instead of `parameters`). This is used
	/// by the runtime's cold-start prompt-token estimator to avoid folding a
	/// provider-specific byte bias into the calibration scale.
	fn preview_wire_tool_schema_bytes(&self, definitions: &[ToolDefinition]) -> Vec<u8> {
		serde_json::to_vec(definitions).unwrap_or_default()
	}

	/// Return the token counter this provider uses for pre-flight byte→token
	/// estimation. The default implementation returns a uniform `bytes / 4`
	/// heuristic (see [`crate::token_counter::ByteHeuristicCounter`]), which
	/// tracks the widely-used approximation for OpenAI-family tokenizers on
	/// mixed English and code.
	///
	/// Providers with access to a local tokenizer or a remote
	/// `/count_tokens` endpoint should override this with a higher-fidelity
	/// counter so the runtime estimator converges on real usage without
	/// relying on the calibration EMA to absorb a systematic bias.
	fn token_counter(&self) -> Arc<dyn TokenCounter> {
		default_counter()
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
			cache_creation_input_tokens: provider_response.cache_creation_input_tokens,
			cache_read_input_tokens: provider_response.cache_read_input_tokens,
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
					// Only count content-bearing chunks for retry eligibility.
					// Done is terminal metadata — providers may emit it before
					// returning an error, and counting it would block retries.
					if !matches!(chunk, StreamChunk::Done { .. }) {
						counter.fetch_add(1, Ordering::Relaxed);
					}
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
						cache_creation_input_tokens: response.cache_creation_input_tokens,
						cache_read_input_tokens: response.cache_read_input_tokens,
					});
				}
				Ok(Err(error)) => {
					let attempts_used = attempt_index.saturating_add(1);

					// Surface context-window-exceeded as its own variant so the
					// runtime can react with compact + retry instead of treating
					// it as a generic provider failure. Non-retryable at the
					// router layer, and must NOT be recorded against the circuit
					// breaker — the provider is healthy; the caller sent an
					// oversized prompt.
					if let ProviderCallError::ContextWindowExceeded { detail } = &error {
						return Err(LlmAdapterError::ContextWindowExceeded {
							provider: selected_model.provider.clone(),
							model_id: selected_model.model_id.clone(),
							detail: detail.clone(),
						});
					}

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

					let backoff_delay =
						backoff_for_attempt(attempt_index, &self.resilience_policy, Some(&error));
					if !backoff_delay.is_zero() {
						tokio::time::sleep(backoff_delay).await;
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

					let backoff_delay =
						backoff_for_attempt(attempt_index, &self.resilience_policy, None);
					if !backoff_delay.is_zero() {
						tokio::time::sleep(backoff_delay).await;
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
					// Surface context-window-exceeded as its own variant so the
					// runtime can react with compact + retry. Non-retryable at
					// this layer, and must NOT be recorded against the circuit
					// breaker — the provider is healthy; the caller sent an
					// oversized prompt.
					if let ProviderCallError::ContextWindowExceeded { detail } = &error {
						return Err(LlmAdapterError::ContextWindowExceeded {
							provider: model.provider.clone(),
							model_id: model.model_id.clone(),
							detail: detail.clone(),
						});
					}
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

					let backoff_delay =
						backoff_for_attempt(attempt_index, &self.resilience_policy, Some(&error));
					if !backoff_delay.is_zero() {
						tokio::time::sleep(backoff_delay).await;
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

	/// Returns `true` when at least one registered provider supports remote
	/// context compaction via the compact endpoint.
	pub fn supports_remote_compaction(&self) -> bool {
		self.providers
			.values()
			.any(|p| p.provider.supports_compact_history())
	}

	/// Forward a compaction request to the first provider that supports it.
	///
	/// Returns `None` when no registered provider supports remote compaction.
	/// Returns `Some(Err(...))` when the supporting provider returned an error.
	pub async fn compact_history(
		&self,
		req: &CompactRequest,
	) -> Option<Result<CompactResponse, LlmAdapterError>> {
		for registered in self.providers.values() {
			let result = registered.provider.compact_history(req).await;
			if let Some(r) = result {
				return Some(r.map_err(|e| LlmAdapterError::ProviderCallFailed {
					provider: "compact".to_string(),
					model_id: req.model.clone(),
					message: e.to_string(),
				}));
			}
		}
		None
	}

	/// Returns `true` when the provider that would serve `model_id` reports
	/// that it supports output-slot escalation.
	///
	/// Defaults to `true` when the model is not registered (preserves existing
	/// behavior for callers that do not call this guard).
	pub fn provider_supports_output_slot_cap(&self, model_id: &str) -> bool {
		// Find the model profile to resolve its provider name.
		let provider_name = self
			.models
			.iter()
			.find(|m| m.model_id == model_id)
			.map(|m| m.provider.as_str());
		match provider_name {
			Some(name) => self
				.providers
				.get(name)
				.map(|p| p.provider.supports_output_slot_cap())
				.unwrap_or(true),
			None => true,
		}
	}

	/// Preview the exact bytes the active provider would send on the wire for
	/// the `tools` field given `definitions`.
	///
	/// Picks the provider bound to `model_id`; falls back to the first-highest
	/// priority registered model's provider when `model_id` is unknown. If no
	/// provider is registered at all, returns the Roku-internal canonical form
	/// (this preserves existing behavior for the estimator call sites that
	/// predate the per-provider override).
	///
	/// Prefer [`Self::preview_wire_tool_schema_bytes_for_request`] when the
	/// caller can construct (or has) a `GenerationRequest`: it routes via the
	/// same `select_model` logic the real `generate` call uses, so the
	/// estimator sees the provider that will actually serve this request
	/// — not a priority-based approximation that can disagree when
	/// eligibility filters kick in.
	pub fn preview_wire_tool_schema_bytes(
		&self,
		model_id: Option<&str>,
		definitions: &[ToolDefinition],
	) -> Vec<u8> {
		let provider_name = model_id
			.and_then(|id| self.models.iter().find(|m| m.model_id == id))
			.or_else(|| {
				// No explicit model_id: use the highest-priority registered
				// model as a proxy for "the provider that will most likely
				// serve the next call". Matches select_model's tie-breaker
				// when no `model_override` is in play and keeps the
				// estimator deterministic across turns.
				self.models.iter().max_by_key(|m| m.route_priority)
			})
			.map(|m| m.provider.as_str());
		match provider_name.and_then(|name| self.providers.get(name)) {
			Some(p) => p.provider.preview_wire_tool_schema_bytes(definitions),
			None => serde_json::to_vec(definitions).unwrap_or_default(),
		}
	}

	/// Same as [`Self::preview_wire_tool_schema_bytes`] but resolves the
	/// target provider by running the full `select_model` policy against
	/// `request`. This is the preferred call for the runtime's pre-flight
	/// estimator because it matches what `generate` / `generate_streaming`
	/// would actually route the request to — `model_override` eligibility,
	/// risk-tier filtering, and cost / latency ordering all come into play.
	///
	/// If the request is ineligible for every registered model, or the
	/// matched provider is not registered, returns the Roku-internal
	/// canonical form so the estimator still produces a sensible value
	/// instead of panicking or zero-ing out.
	pub fn preview_wire_tool_schema_bytes_for_request(
		&self,
		request: &GenerationRequest,
		definitions: &[ToolDefinition],
	) -> Vec<u8> {
		match self.select_model(request).ok() {
			Some(model) => match self.providers.get(&model.provider) {
				Some(registered) => registered
					.provider
					.preview_wire_tool_schema_bytes(definitions),
				None => serde_json::to_vec(definitions).unwrap_or_default(),
			},
			None => serde_json::to_vec(definitions).unwrap_or_default(),
		}
	}

	/// Resolve the [`TokenCounter`] that would serve `request`.
	///
	/// Runs the full `select_model` policy so the estimator sees the same
	/// provider that `generate` / `generate_streaming` would pick. Returns
	/// the shared default counter when no eligible model exists, which
	/// keeps the estimator producing a sensible number instead of failing.
	pub fn token_counter_for_request(&self, request: &GenerationRequest) -> Arc<dyn TokenCounter> {
		match self.select_model(request).ok() {
			Some(model) => match self.providers.get(&model.provider) {
				Some(registered) => registered.provider.token_counter(),
				None => default_counter(),
			},
			None => default_counter(),
		}
	}

	/// Return the `model_id` that `select_model` would route `request` to.
	///
	/// Runs the same policy as `generate` / `generate_streaming` — model
	/// override eligibility first, then the cost / risk / budget
	/// ordering. Returns `None` when no registered model can serve the
	/// request. Callers validating the committed-token baseline against
	/// the serving model must use this (not `request.model_override`)
	/// because a null override routes via priority and a populated but
	/// ineligible override falls through to ordering; in both cases the
	/// routed id can differ from the override.
	pub fn selected_model_id_for_request(&self, request: &GenerationRequest) -> Option<String> {
		self.select_model(request).ok().map(|m| m.model_id.clone())
	}

	/// Resolve the [`TokenCounter`] for a given `model_id`.
	///
	/// When `model_id` is `None`, falls back to the highest-priority
	/// registered model (same tie-breaker as
	/// [`Self::preview_wire_tool_schema_bytes`]). Returns the shared
	/// default counter when the model is unknown or its provider is not
	/// registered.
	pub fn token_counter_for_model(&self, model_id: Option<&str>) -> Arc<dyn TokenCounter> {
		let provider_name = model_id
			.and_then(|id| self.models.iter().find(|m| m.model_id == id))
			.or_else(|| self.models.iter().max_by_key(|m| m.route_priority))
			.map(|m| m.provider.as_str());
		match provider_name.and_then(|name| self.providers.get(name)) {
			Some(p) => p.provider.token_counter(),
			None => default_counter(),
		}
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

#[cfg(test)]
mod tests {
	use std::collections::VecDeque;
	use std::sync::Arc;
	use std::sync::Mutex;
	use std::sync::atomic::{AtomicUsize, Ordering};

	use async_trait::async_trait;
	use roku_common_types::Metrics;

	use crate::types::{
		GenerationRequest, Message, ModelProfile, ProviderCallError, ProviderResiliencePolicy,
		ProviderResponse, RiskTier, RoutingPolicy, ToolDefinition,
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
				cache_creation_input_tokens: 0,
				cache_read_input_tokens: 0,
				latency_ms: self.latency_ms,
				tool_calls: None,
				response_id: None,
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
			system_prompt_sections: None,
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
					Err(ProviderCallError::Fatal {
						message: "no more responses configured".to_string(),
					})
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
	fn preview_wire_bytes_for_request_routes_to_select_model_not_max_priority() {
		// Two providers registered. `priority-tag-provider` has the higher
		// `route_priority` — the priority-based `preview_wire_tool_schema_bytes(None, ...)`
		// picks it. `eligible-tag-provider` has a lower priority but cheaper
		// cost, so a request with a tight cost budget excludes the
		// high-priority model at `select_model` time and must route to the
		// eligible one.
		//
		// The two providers emit distinguishable wire bytes via their
		// `preview_wire_tool_schema_bytes` override; the test asserts that
		// `preview_wire_tool_schema_bytes_for_request` tracks the real
		// `select_model` pick rather than the priority max.

		struct TaggedProvider {
			name: &'static str,
			tag: &'static [u8],
		}

		#[async_trait]
		impl LlmProvider for TaggedProvider {
			fn provider_name(&self) -> &'static str {
				self.name
			}

			async fn complete(
				&self,
				_model: &ModelProfile,
				_request: &GenerationRequest,
			) -> Result<ProviderResponse, ProviderCallError> {
				unreachable!("TaggedProvider is only used for schema-preview tests")
			}

			fn preview_wire_tool_schema_bytes(&self, _definitions: &[ToolDefinition]) -> Vec<u8> {
				self.tag.to_vec()
			}
		}

		let mut router = LlmRouter::new(RoutingPolicy {
			max_request_cost_usd: 10.0,
			max_latency_ms: 10_000,
		});
		router.register_provider(TaggedProvider {
			name: "priority-tag-provider",
			tag: b"PRIORITY",
		});
		router.register_provider(TaggedProvider {
			name: "eligible-tag-provider",
			tag: b"ELIGIBLE",
		});
		// High priority, high cost.
		router.register_model(ModelProfile {
			model_id: "priority-model".to_string(),
			provider: "priority-tag-provider".to_string(),
			max_context_tokens: 8_000,
			cost_per_1k_tokens_usd: 5.0,
			max_risk_tier: RiskTier::Critical,
			route_priority: 100,
		});
		// Low priority, low cost.
		router.register_model(ModelProfile {
			model_id: "eligible-model".to_string(),
			provider: "eligible-tag-provider".to_string(),
			max_context_tokens: 8_000,
			cost_per_1k_tokens_usd: 0.01,
			max_risk_tier: RiskTier::Critical,
			route_priority: 1,
		});

		let defs = vec![ToolDefinition {
			name: "tool-a".to_string(),
			description: "desc".to_string(),
			parameters: serde_json::json!({"type": "object"}),
		}];

		// Baseline: priority-based API picks the highest `route_priority` →
		// PRIORITY bytes.
		let priority_bytes = router.preview_wire_tool_schema_bytes(None, &defs);
		assert_eq!(
			priority_bytes, b"PRIORITY",
			"priority-based lookup picks the max-priority provider"
		);

		// Now build a request whose cost budget excludes the high-priority
		// model but leaves the eligible one. Approx prompt+output ≈ 300
		// tokens; at $5/1k that's ~$1.5 for `priority-model` and ~$0.003
		// for `eligible-model`. A budget of $0.01 rejects the former and
		// admits the latter, forcing `select_model` to pick against
		// max-priority.
		let tight_request = GenerationRequest {
			system_prompt: None,
			prompt: String::new(),
			messages: Some(vec![Message::User {
				content: "tiny".to_string(),
			}]),
			expected_output_tokens: 300,
			risk_tier: RiskTier::Low,
			preferred_provider: None,
			budget_tokens_remaining: 4_000,
			budget_cost_remaining_usd: 0.01,
			tools: None,
			model_override: None,
			thinking_effort: None,
			system_prompt_sections: None,
		};
		let routed_bytes = router.preview_wire_tool_schema_bytes_for_request(&tight_request, &defs);
		assert_eq!(
			routed_bytes, b"ELIGIBLE",
			"select_model routes around the priority-max when budget excludes it; \
			 preview_wire_tool_schema_bytes_for_request must follow that pick"
		);
		assert_ne!(
			priority_bytes, routed_bytes,
			"route-aware and priority-based previews must disagree under \
			 this scenario — otherwise the test is not exercising the bug",
		);

		// And: `model_override` explicitly selects priority-model.
		// `select_model` still applies `supports(request)`, so the override
		// only wins when the model actually fits the budget — use a loose
		// request here to verify the override-hits-priority path.
		let override_request = GenerationRequest {
			model_override: Some("priority-model".to_string()),
			budget_cost_remaining_usd: 5.0,
			..tight_request.clone()
		};
		let override_bytes =
			router.preview_wire_tool_schema_bytes_for_request(&override_request, &defs);
		assert_eq!(
			override_bytes, b"PRIORITY",
			"explicit model_override must route to that model's provider"
		);
	}

	#[test]
	fn selected_model_id_for_request_returns_routed_id_not_override() {
		// The committed-token baseline validator compares the stored
		// serving model against what the router will actually pick for
		// the next call. `selected_model_id_for_request` must run the
		// full `select_model` policy so:
		//
		// - A null override returns the routed model (not `None`), so
		//   the default-routing case doesn't permanently mis-match the
		//   committed `resp.model_id`.
		// - An override that falls through eligibility returns the
		//   fallback model (not the override), so baseline validation
		//   catches the model swap.
		// - A populated, eligible override returns that override's id.

		struct NoopProvider {
			name: &'static str,
		}

		#[async_trait]
		impl LlmProvider for NoopProvider {
			fn provider_name(&self) -> &'static str {
				self.name
			}

			async fn complete(
				&self,
				_model: &ModelProfile,
				_request: &GenerationRequest,
			) -> Result<ProviderResponse, ProviderCallError> {
				unreachable!("NoopProvider is only used for routing tests")
			}
		}

		let mut router = LlmRouter::new(RoutingPolicy {
			max_request_cost_usd: 10.0,
			max_latency_ms: 10_000,
		});
		router.register_provider(NoopProvider {
			name: "provider-priority",
		});
		router.register_provider(NoopProvider {
			name: "provider-fallback",
		});
		router.register_model(ModelProfile {
			model_id: "priority-model".to_string(),
			provider: "provider-priority".to_string(),
			max_context_tokens: 8_000,
			cost_per_1k_tokens_usd: 5.0,
			max_risk_tier: RiskTier::Critical,
			route_priority: 100,
		});
		router.register_model(ModelProfile {
			model_id: "fallback-model".to_string(),
			provider: "provider-fallback".to_string(),
			max_context_tokens: 8_000,
			cost_per_1k_tokens_usd: 0.01,
			max_risk_tier: RiskTier::Critical,
			route_priority: 1,
		});

		// Case 1: no override, `RiskTier::Critical` (which exercises the
		// priority-then-cost ordering) → routes to priority-model. The
		// critical point for baseline validation is that the runtime
		// receives a stable id to compare against the committed
		// `resp.model_id`. Previously the runtime passed
		// `request.model_override.as_deref() = None` and the validator
		// always read as a mismatch on the common default-routing path.
		let default_routing = GenerationRequest {
			system_prompt: None,
			prompt: String::new(),
			messages: Some(vec![Message::User {
				content: "x".to_string(),
			}]),
			expected_output_tokens: 100,
			risk_tier: RiskTier::Critical,
			preferred_provider: None,
			budget_tokens_remaining: 4_000,
			budget_cost_remaining_usd: 10.0,
			tools: None,
			model_override: None,
			thinking_effort: None,
			system_prompt_sections: None,
		};
		assert_eq!(
			router.selected_model_id_for_request(&default_routing),
			Some("priority-model".to_string()),
			"null override under Critical risk must resolve to the \
			 priority-max routed model, not `None` (default-routing case)",
		);
		let default_routed = "priority-model".to_string();

		// Case 2: override pinned to a model with a budget that excludes
		// it → router falls through to the other model. Returning the
		// fallback id (not the override id) is what lets the baseline
		// validator detect a mid-session provider swap.
		let tight_override = GenerationRequest {
			model_override: Some("priority-model".to_string()),
			budget_cost_remaining_usd: 0.01,
			..default_routing.clone()
		};
		let tight_routed = router
			.selected_model_id_for_request(&tight_override)
			.expect("tight routing resolved");
		assert_ne!(
			tight_routed, "priority-model",
			"ineligible override must fall through to a different \
			 routed model, not return the override id",
		);

		// Case 3: override pinned with a matching generous budget → the
		// routed id equals the override (override wins eligibility).
		let eligible_override = GenerationRequest {
			model_override: Some("priority-model".to_string()),
			budget_cost_remaining_usd: 10.0,
			..default_routing.clone()
		};
		assert_eq!(
			router.selected_model_id_for_request(&eligible_override),
			Some("priority-model".to_string()),
			"eligible override returns its own id",
		);

		// Case 4: no registered model fits the request → None. The
		// baseline validator treats None as a mismatch and falls back
		// to whole-history estimation.
		let impossible = GenerationRequest {
			budget_cost_remaining_usd: 0.0,
			budget_tokens_remaining: 0,
			..default_routing.clone()
		};
		assert!(
			router.selected_model_id_for_request(&impossible).is_none(),
			"no eligible model → None (signals baseline mismatch)",
		);

		// Sanity: cases 1 and 2 must disagree on the routed id — the
		// test is only meaningful if a model swap is actually observable.
		assert_ne!(
			default_routed, tight_routed,
			"default routing and tight-budget routing must land on \
			 different models for this scenario to exercise the bug",
		);
	}

	#[test]
	fn preview_wire_bytes_for_request_honors_system_prompt_in_eligibility() {
		// Regression: the runtime used to hand `select_model` a request with
		// `system_prompt: None`, so estimator-time eligibility differed from
		// the real generation request's eligibility whenever the system
		// prompt tokens pushed the input over a model's budget. The fix
		// passes the real system prompt into the selection request; this
		// test locks that behavior by constructing a budget tight enough
		// that the "high-priority" model is eligible WITHOUT a system prompt
		// but ineligible WITH one.

		struct TaggedProvider {
			name: &'static str,
			tag: &'static [u8],
		}

		#[async_trait]
		impl LlmProvider for TaggedProvider {
			fn provider_name(&self) -> &'static str {
				self.name
			}

			async fn complete(
				&self,
				_model: &ModelProfile,
				_request: &GenerationRequest,
			) -> Result<ProviderResponse, ProviderCallError> {
				unreachable!("TaggedProvider is only used for schema-preview tests")
			}

			fn preview_wire_tool_schema_bytes(&self, _definitions: &[ToolDefinition]) -> Vec<u8> {
				self.tag.to_vec()
			}
		}

		let mut router = LlmRouter::new(RoutingPolicy {
			max_request_cost_usd: 10.0,
			max_latency_ms: 10_000,
		});
		router.register_provider(TaggedProvider {
			name: "tight-tag-provider",
			tag: b"TIGHT",
		});
		router.register_provider(TaggedProvider {
			name: "roomy-tag-provider",
			tag: b"ROOMY",
		});
		// High priority but tight context window (50 tokens).
		router.register_model(ModelProfile {
			model_id: "tight-model".to_string(),
			provider: "tight-tag-provider".to_string(),
			max_context_tokens: 50,
			cost_per_1k_tokens_usd: 0.01,
			max_risk_tier: RiskTier::Critical,
			route_priority: 100,
		});
		// Lower priority, roomy context window.
		router.register_model(ModelProfile {
			model_id: "roomy-model".to_string(),
			provider: "roomy-tag-provider".to_string(),
			max_context_tokens: 8_000,
			cost_per_1k_tokens_usd: 0.01,
			max_risk_tier: RiskTier::Critical,
			route_priority: 1,
		});

		let defs = vec![ToolDefinition {
			name: "tool-a".to_string(),
			description: "desc".to_string(),
			parameters: serde_json::json!({"type": "object"}),
		}];

		// Messages alone: ~20 tokens — both models are eligible; tight-model
		// wins on priority and its wire bytes are returned.
		let small_message_payload: Vec<String> = (0..20).map(|_| "word".to_string()).collect();
		let request_without_system_prompt = GenerationRequest {
			system_prompt: None,
			prompt: String::new(),
			messages: Some(vec![Message::User {
				content: small_message_payload.join(" "),
			}]),
			expected_output_tokens: 10,
			risk_tier: RiskTier::Low,
			preferred_provider: None,
			budget_tokens_remaining: 10_000,
			budget_cost_remaining_usd: 5.0,
			tools: None,
			model_override: None,
			thinking_effort: None,
			system_prompt_sections: None,
		};
		let without_bytes = router
			.preview_wire_tool_schema_bytes_for_request(&request_without_system_prompt, &defs);
		assert_eq!(
			without_bytes, b"TIGHT",
			"without a system prompt the tight-but-high-priority model is eligible"
		);

		// Adding a large system prompt (~200 whitespace-separated words) pushes
		// tight-model past its 50-token context window. `select_model` must
		// now route to `roomy-model` — and the preview bytes track that pick.
		let large_system_prompt: String = vec!["instruction"; 200].join(" ");
		let request_with_system_prompt = GenerationRequest {
			system_prompt: Some(large_system_prompt),
			..request_without_system_prompt.clone()
		};
		let with_bytes =
			router.preview_wire_tool_schema_bytes_for_request(&request_with_system_prompt, &defs);
		assert_eq!(
			with_bytes, b"ROOMY",
			"including the system prompt excludes the tight-context model, \
			 so select_model must route to the roomy provider"
		);
		assert_ne!(
			without_bytes, with_bytes,
			"without/with system prompt must produce different provider picks \
			 — otherwise the test is not exercising the bug"
		);
	}

	#[test]
	fn preview_wire_bytes_for_request_honors_messages_size_in_eligibility() {
		// Sibling of `...honors_system_prompt_in_eligibility`: the messages
		// axis also feeds `estimate_request_input_tokens`, so growing the
		// message set can flip `select_model`'s eligibility and change the
		// provider the preview bytes come from. The runtime's end-of-turn
		// estimator relies on this: after tool execution has appended
		// messages, the recomputed wire bytes must track the provider the
		// next outbound call would actually route to.

		struct TaggedProvider {
			name: &'static str,
			tag: &'static [u8],
		}

		#[async_trait]
		impl LlmProvider for TaggedProvider {
			fn provider_name(&self) -> &'static str {
				self.name
			}

			async fn complete(
				&self,
				_model: &ModelProfile,
				_request: &GenerationRequest,
			) -> Result<ProviderResponse, ProviderCallError> {
				unreachable!("TaggedProvider is only used for schema-preview tests")
			}

			fn preview_wire_tool_schema_bytes(&self, _definitions: &[ToolDefinition]) -> Vec<u8> {
				self.tag.to_vec()
			}
		}

		let mut router = LlmRouter::new(RoutingPolicy {
			max_request_cost_usd: 10.0,
			max_latency_ms: 10_000,
		});
		router.register_provider(TaggedProvider {
			name: "tight-msg-provider",
			tag: b"TIGHT",
		});
		router.register_provider(TaggedProvider {
			name: "roomy-msg-provider",
			tag: b"ROOMY",
		});
		// High priority but tight context window (30 tokens).
		router.register_model(ModelProfile {
			model_id: "tight-msg-model".to_string(),
			provider: "tight-msg-provider".to_string(),
			max_context_tokens: 30,
			cost_per_1k_tokens_usd: 0.01,
			max_risk_tier: RiskTier::Critical,
			route_priority: 100,
		});
		// Lower priority, roomy context window.
		router.register_model(ModelProfile {
			model_id: "roomy-msg-model".to_string(),
			provider: "roomy-msg-provider".to_string(),
			max_context_tokens: 8_000,
			cost_per_1k_tokens_usd: 0.01,
			max_risk_tier: RiskTier::Critical,
			route_priority: 1,
		});

		let defs = vec![ToolDefinition {
			name: "tool-a".to_string(),
			description: "desc".to_string(),
			parameters: serde_json::json!({"type": "object"}),
		}];

		// Small message set — fits inside the tight model's 30-token window.
		let small_request = GenerationRequest {
			system_prompt: None,
			prompt: String::new(),
			messages: Some(vec![Message::User {
				content: "hi".to_string(),
			}]),
			expected_output_tokens: 5,
			risk_tier: RiskTier::Low,
			preferred_provider: None,
			budget_tokens_remaining: 10_000,
			budget_cost_remaining_usd: 5.0,
			tools: None,
			model_override: None,
			thinking_effort: None,
			system_prompt_sections: None,
		};
		let small_bytes = router.preview_wire_tool_schema_bytes_for_request(&small_request, &defs);
		assert_eq!(
			small_bytes, b"TIGHT",
			"small messages fit the tight-context model's window → TIGHT wins on priority"
		);

		// Grow the messages past the tight model's context window.
		let large_payload: Vec<String> = (0..50).map(|_| "word".to_string()).collect();
		let large_request = GenerationRequest {
			messages: Some(vec![Message::User {
				content: large_payload.join(" "),
			}]),
			..small_request.clone()
		};
		let large_bytes = router.preview_wire_tool_schema_bytes_for_request(&large_request, &defs);
		assert_eq!(
			large_bytes, b"ROOMY",
			"large messages exceed tight-context window → select_model routes to the roomy provider"
		);
		assert_ne!(
			small_bytes, large_bytes,
			"small/large messages must produce different provider picks — \
			 otherwise the test is not exercising the bug the fix addresses"
		);
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
				Err(ProviderCallError::ServerError {
					status: 500,
					message: "transient upstream failure".to_string(),
				}),
				Ok(ProviderResponse {
					output: "recovered".to_string(),
					finish_reason: None,
					prompt_tokens: 40,
					output_tokens: 12,
					cache_creation_input_tokens: 0,
					cache_read_input_tokens: 0,
					latency_ms: 80,
					tool_calls: None,
					response_id: None,
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
			vec![Err(ProviderCallError::InvalidRequest {
				message: "invalid request payload".to_string(),
			})],
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
				Err(ProviderCallError::ServerOverloaded {
					message: "temporary overload".to_string(),
					retry_after: None,
				}),
				Err(ProviderCallError::ServerOverloaded {
					message: "temporary overload".to_string(),
					retry_after: None,
				}),
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
				Err(ProviderCallError::ServerOverloaded {
					message: "temporary overload".to_string(),
					retry_after: None,
				}),
				Ok(ProviderResponse {
					output: "healthy-again".to_string(),
					finish_reason: None,
					prompt_tokens: 30,
					output_tokens: 10,
					cache_creation_input_tokens: 0,
					cache_read_input_tokens: 0,
					latency_ms: 60,
					tool_calls: None,
					response_id: None,
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
					cache_creation_input_tokens: 0,
					cache_read_input_tokens: 0,
					latency_ms: 20,
					tool_calls: None,
					response_id: None,
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

	#[test]
	fn router_surfaces_context_window_exceeded_as_dedicated_variant() {
		let provider = Arc::new(SequenceProvider::new(
			"ctx-overflow-provider",
			vec![Err(ProviderCallError::ContextWindowExceeded {
				detail: "prompt is too long: 215321 tokens > 200000".to_string(),
			})],
		));
		let mut router = LlmRouter::new(RoutingPolicy::default()).with_provider_resilience_policy(
			ProviderResiliencePolicy {
				max_retries: 4,
				initial_backoff_ms: 0,
				max_backoff_ms: 0,
				circuit_breaker_failure_threshold: 4,
				circuit_breaker_cooldown_ms: 0,
			},
		);
		router.register_provider(Arc::clone(&provider));
		router.register_model(model_profile("ctx-overflow-provider"));

		let error = router
			.generate_blocking(&sample_request(RiskTier::Low))
			.expect_err("context window exceeded should surface as dedicated variant");
		match error {
			LlmAdapterError::ContextWindowExceeded {
				provider: ref provider_name,
				ref detail,
				..
			} => {
				assert_eq!(provider_name, "ctx-overflow-provider");
				assert!(detail.contains("215321"));
			}
			other => panic!("expected ContextWindowExceeded, got {other:?}"),
		}
		// The router must NOT retry context-window-exceeded — recovery is the
		// caller's responsibility (compact + retry happens in the runtime).
		assert_eq!(provider.invocations(), 1);
	}

	#[test]
	fn context_window_exceeded_does_not_poison_circuit_breaker() {
		// A context overflow is a caller-side prompt sizing issue, not a
		// provider-health signal. The router must return without counting it
		// against the circuit breaker so that subsequent normal calls in the
		// same run are not rejected with `CircuitOpen`.
		let provider = Arc::new(SequenceProvider::new(
			"ctx-overflow-breaker",
			vec![
				Err(ProviderCallError::ContextWindowExceeded {
					detail: "prompt too long".to_string(),
				}),
				Ok(ProviderResponse {
					output: "second call ok".to_string(),
					finish_reason: None,
					prompt_tokens: 10,
					output_tokens: 5,
					cache_creation_input_tokens: 0,
					cache_read_input_tokens: 0,
					latency_ms: 1,
					tool_calls: None,
					response_id: None,
				}),
			],
		));
		// Threshold 1 + non-zero cooldown — if the overflow were recorded the
		// breaker would trip after the first call and the second call would
		// see `CircuitOpen`.
		let mut router = LlmRouter::new(RoutingPolicy::default()).with_provider_resilience_policy(
			ProviderResiliencePolicy {
				max_retries: 0,
				initial_backoff_ms: 0,
				max_backoff_ms: 0,
				circuit_breaker_failure_threshold: 1,
				circuit_breaker_cooldown_ms: 60_000,
			},
		);
		router.register_provider(Arc::clone(&provider));
		router.register_model(model_profile("ctx-overflow-breaker"));

		let err = router
			.generate_blocking(&sample_request(RiskTier::Low))
			.expect_err("first call must surface ContextWindowExceeded");
		assert!(matches!(err, LlmAdapterError::ContextWindowExceeded { .. }));

		let response = router
			.generate_blocking(&sample_request(RiskTier::Low))
			.expect("second call must succeed; breaker must not have tripped");
		assert_eq!(response.output, "second call ok");
		assert_eq!(provider.invocations(), 2);
	}

	#[test]
	fn streaming_context_window_exceeded_does_not_poison_circuit_breaker() {
		// Parallel regression for the streaming retry path. Same invariant:
		// overflow surfaces as its own variant and must not count against the
		// provider's circuit breaker.
		let rt = tokio::runtime::Runtime::new().expect("tokio runtime must initialize");
		rt.block_on(async {
			let provider = Arc::new(SequenceProvider::new(
				"stream-ctx-overflow-breaker",
				vec![
					Err(ProviderCallError::ContextWindowExceeded {
						detail: "prompt too long".to_string(),
					}),
					Ok(ProviderResponse {
						output: "second call ok".to_string(),
						finish_reason: None,
						prompt_tokens: 10,
						output_tokens: 5,
						cache_creation_input_tokens: 0,
						cache_read_input_tokens: 0,
						latency_ms: 1,
						tool_calls: None,
						response_id: None,
					}),
				],
			));
			let mut router = LlmRouter::new(RoutingPolicy::default())
				.with_provider_resilience_policy(ProviderResiliencePolicy {
					max_retries: 0,
					initial_backoff_ms: 0,
					max_backoff_ms: 0,
					circuit_breaker_failure_threshold: 1,
					circuit_breaker_cooldown_ms: 60_000,
				});
			router.register_provider(Arc::clone(&provider));
			router.register_model(model_profile("stream-ctx-overflow-breaker"));

			let (tx, mut rx) = tokio::sync::mpsc::channel::<StreamChunk>(16);
			let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });

			let err = router
				.generate_streaming(&sample_request(RiskTier::Low), tx)
				.await
				.expect_err("first streaming call must surface ContextWindowExceeded");
			assert!(matches!(err, LlmAdapterError::ContextWindowExceeded { .. }));
			let _ = drain.await;

			let response = router
				.generate(&sample_request(RiskTier::Low))
				.await
				.expect("second call must succeed; breaker must not have tripped");
			assert_eq!(response.output, "second call ok");
			assert_eq!(provider.invocations(), 2);

			// Avoid dropping the embedded blocking runtime from inside this
			// async context.
			std::mem::forget(router);
		});
	}

	// ---------------------------------------------------------------------------
	// compact_history and supports_remote_compaction dispatch tests
	// ---------------------------------------------------------------------------

	/// A provider that supports compact_history and returns a fixed response.
	struct CompactCapableProvider {
		response: CompactResponse,
	}

	#[async_trait]
	impl LlmProvider for CompactCapableProvider {
		fn provider_name(&self) -> &'static str {
			"compact-capable"
		}

		async fn complete(
			&self,
			_model: &ModelProfile,
			_request: &GenerationRequest,
		) -> Result<ProviderResponse, ProviderCallError> {
			Ok(ProviderResponse {
				output: String::new(),
				finish_reason: None,
				prompt_tokens: 0,
				output_tokens: 0,
				cache_creation_input_tokens: 0,
				cache_read_input_tokens: 0,
				latency_ms: 0,
				tool_calls: None,
				response_id: None,
			})
		}

		fn supports_compact_history(&self) -> bool {
			true
		}

		async fn compact_history(
			&self,
			_request: &CompactRequest,
		) -> Option<Result<CompactResponse, ProviderCallError>> {
			Some(Ok(self.response.clone()))
		}

		fn supports_output_slot_cap(&self) -> bool {
			false
		}
	}

	/// A regular provider with no compact support (uses all defaults).
	struct NoCompactProvider;

	#[async_trait]
	impl LlmProvider for NoCompactProvider {
		fn provider_name(&self) -> &'static str {
			"no-compact"
		}

		async fn complete(
			&self,
			_model: &ModelProfile,
			_request: &GenerationRequest,
		) -> Result<ProviderResponse, ProviderCallError> {
			Ok(ProviderResponse {
				output: String::new(),
				finish_reason: None,
				prompt_tokens: 0,
				output_tokens: 0,
				cache_creation_input_tokens: 0,
				cache_read_input_tokens: 0,
				latency_ms: 0,
				tool_calls: None,
				response_id: None,
			})
		}
		// All defaults: supports_compact_history() = false, supports_output_slot_cap() = true
	}

	#[test]
	fn supports_remote_compaction_is_true_when_capable_provider_registered() {
		let mut router = LlmRouter::new(RoutingPolicy::default());
		let compact_resp = CompactResponse {
			output: vec![crate::types::Message::User {
				content: "summary".to_string(),
			}],
			usage: crate::types::CompactUsageSummary::default(),
		};
		router.register_provider(CompactCapableProvider {
			response: compact_resp,
		});
		router.register_model(ModelProfile {
			model_id: "compact-capable-model".to_string(),
			provider: "compact-capable".to_string(),
			max_context_tokens: 8_000,
			cost_per_1k_tokens_usd: 0.01,
			max_risk_tier: RiskTier::Critical,
			route_priority: 100,
		});
		assert!(router.supports_remote_compaction());
		std::mem::forget(router);
	}

	#[test]
	fn supports_remote_compaction_is_false_for_non_compact_provider() {
		let mut router = LlmRouter::new(RoutingPolicy::default());
		router.register_provider(NoCompactProvider);
		router.register_model(model_profile("no-compact"));
		assert!(!router.supports_remote_compaction());
		std::mem::forget(router);
	}

	#[tokio::test]
	async fn router_compact_history_forwards_to_capable_provider() {
		let mut router = LlmRouter::new(RoutingPolicy::default());
		let expected_output = vec![crate::types::Message::User {
			content: "compacted result".to_string(),
		}];
		router.register_provider(CompactCapableProvider {
			response: CompactResponse {
				output: expected_output.clone(),
				usage: crate::types::CompactUsageSummary {
					prompt_tokens: 200,
					output_tokens: 40,
					cached_input_tokens: 0,
				},
			},
		});
		router.register_model(ModelProfile {
			model_id: "compact-capable-model".to_string(),
			provider: "compact-capable".to_string(),
			max_context_tokens: 8_000,
			cost_per_1k_tokens_usd: 0.01,
			max_risk_tier: RiskTier::Critical,
			route_priority: 100,
		});

		let req = CompactRequest {
			model: "compact-capable-model".to_string(),
			instructions: "summarize".to_string(),
			input: vec![],
			tools: vec![],
			parallel_tool_calls: false,
			reasoning: None,
		};
		let result = router.compact_history(&req).await;
		assert!(result.is_some(), "router must forward to capable provider");
		let resp = result.unwrap().expect("capable provider returns Ok");
		assert_eq!(resp.output, expected_output);
		assert_eq!(resp.usage.prompt_tokens, 200);
		std::mem::forget(router);
	}

	#[tokio::test]
	async fn router_compact_history_returns_none_when_no_capable_provider() {
		let mut router = LlmRouter::new(RoutingPolicy::default());
		router.register_provider(NoCompactProvider);
		router.register_model(model_profile("no-compact"));

		let req = CompactRequest {
			model: "no-compact-model".to_string(),
			instructions: "summarize".to_string(),
			input: vec![],
			tools: vec![],
			parallel_tool_calls: false,
			reasoning: None,
		};
		let result = router.compact_history(&req).await;
		assert!(
			result.is_none(),
			"router must return None when no provider supports compact"
		);
		std::mem::forget(router);
	}

	#[test]
	fn provider_supports_output_slot_cap_false_for_no_cap_provider() {
		let mut router = LlmRouter::new(RoutingPolicy::default());
		router.register_provider(CompactCapableProvider {
			response: CompactResponse {
				output: vec![],
				usage: crate::types::CompactUsageSummary::default(),
			},
		});
		router.register_model(ModelProfile {
			model_id: "compact-capable-model".to_string(),
			provider: "compact-capable".to_string(),
			max_context_tokens: 8_000,
			cost_per_1k_tokens_usd: 0.01,
			max_risk_tier: RiskTier::Critical,
			route_priority: 100,
		});
		// CompactCapableProvider::supports_output_slot_cap() = false
		assert!(
			!router.provider_supports_output_slot_cap("compact-capable-model"),
			"router must reflect provider's false capability"
		);
	}

	#[test]
	fn provider_supports_output_slot_cap_true_for_default_provider() {
		let mut router = LlmRouter::new(RoutingPolicy::default());
		router.register_provider(NoCompactProvider);
		router.register_model(model_profile("no-compact"));
		// NoCompactProvider uses default (true)
		assert!(
			router.provider_supports_output_slot_cap("no-compact-model"),
			"default provider must support output-slot escalation"
		);
	}

	#[test]
	fn provider_supports_output_slot_cap_defaults_to_true_for_unknown_model() {
		let router = LlmRouter::new(RoutingPolicy::default()); // empty
		assert!(
			router.provider_supports_output_slot_cap("unknown-model"),
			"unknown model must default to true (safe fallback)"
		);
	}
}
