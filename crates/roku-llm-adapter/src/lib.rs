//! Multi-provider model routing with budget and risk-aware controls.

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
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
	fn supports(&self, request: &GenerationRequest) -> bool {
		if let Some(preferred_provider) = &request.preferred_provider
			&& preferred_provider != &self.provider
		{
			return false;
		}

		let estimated_prompt_tokens = estimate_prompt_tokens(&request.prompt);
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
pub struct GenerationRequest {
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
	pub prompt_tokens: u64,
	pub output_tokens: u64,
	pub total_tokens: u64,
	pub estimated_cost_usd: f64,
	pub latency_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderResponse {
	pub output: String,
	pub prompt_tokens: u64,
	pub output_tokens: u64,
	pub latency_ms: u64,
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
	#[error("provider call failed for {provider}/{model_id}: {message}")]
	ProviderCallFailed {
		provider: String,
		model_id: String,
		message: String,
	},
}

pub trait LlmProvider: Send + Sync {
	fn provider_name(&self) -> &'static str;
	fn complete(
		&self,
		model: &ModelProfile,
		request: &GenerationRequest,
	) -> Result<ProviderResponse, String>;
}

#[derive(Default)]
pub struct LlmRouter {
	policy: RoutingPolicy,
	models: Vec<ModelProfile>,
	providers: HashMap<String, Arc<dyn LlmProvider>>,
}

impl LlmRouter {
	pub fn new(policy: RoutingPolicy) -> Self {
		Self {
			policy,
			models: Vec::new(),
			providers: HashMap::new(),
		}
	}

	pub fn register_model(&mut self, model: ModelProfile) {
		self.models.push(model);
	}

	pub fn register_provider<P>(&mut self, provider: P)
	where
		P: LlmProvider + 'static,
	{
		self.providers
			.insert(provider.provider_name().to_string(), Arc::new(provider));
	}

	pub fn generate(&self, request: &GenerationRequest) -> Result<LlmResponse, LlmAdapterError> {
		let selected_model = self.select_model(request)?;
		let provider = self
			.providers
			.get(&selected_model.provider)
			.ok_or_else(|| {
				LlmAdapterError::ProviderNotRegistered(selected_model.provider.clone())
			})?;

		let provider_response = provider
			.complete(selected_model, request)
			.map_err(|message| LlmAdapterError::ProviderCallFailed {
				provider: selected_model.provider.clone(),
				model_id: selected_model.model_id.clone(),
				message,
			})?;

		if provider_response.latency_ms > self.policy.max_latency_ms {
			return Err(LlmAdapterError::LatencyExceeded {
				latency_ms: provider_response.latency_ms,
				max_latency_ms: self.policy.max_latency_ms,
			});
		}

		let total_tokens = provider_response
			.prompt_tokens
			.saturating_add(provider_response.output_tokens);
		if total_tokens > request.budget_tokens_remaining {
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
			return Err(LlmAdapterError::BudgetExceeded(format!(
				"cost budget exceeded: cost={estimated_cost_usd:.4} budget={:.4} policy_max={:.4}",
				request.budget_cost_remaining_usd, self.policy.max_request_cost_usd
			)));
		}

		Ok(LlmResponse {
			provider: selected_model.provider.clone(),
			model_id: selected_model.model_id.clone(),
			output: provider_response.output,
			prompt_tokens: provider_response.prompt_tokens,
			output_tokens: provider_response.output_tokens,
			total_tokens,
			estimated_cost_usd,
			latency_ms: provider_response.latency_ms,
		})
	}

	fn select_model(&self, request: &GenerationRequest) -> Result<&ModelProfile, LlmAdapterError> {
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
}

fn estimate_prompt_tokens(prompt: &str) -> u64 {
	u64::try_from(prompt.split_whitespace().count())
		.unwrap_or(u64::MAX)
		.max(1)
}

fn estimate_cost_usd(tokens: u64, cost_per_1k_tokens_usd: f64) -> f64 {
	(tokens as f64 / 1000.0) * cost_per_1k_tokens_usd
}

#[cfg(test)]
mod tests {
	use super::*;

	struct StaticProvider {
		name: &'static str,
		output: &'static str,
		prompt_tokens: u64,
		output_tokens: u64,
		latency_ms: u64,
	}

	impl LlmProvider for StaticProvider {
		fn provider_name(&self) -> &'static str {
			self.name
		}

		fn complete(
			&self,
			_model: &ModelProfile,
			_request: &GenerationRequest,
		) -> Result<ProviderResponse, String> {
			Ok(ProviderResponse {
				output: self.output.to_string(),
				prompt_tokens: self.prompt_tokens,
				output_tokens: self.output_tokens,
				latency_ms: self.latency_ms,
			})
		}
	}

	fn sample_request(risk_tier: RiskTier) -> GenerationRequest {
		GenerationRequest {
			prompt: "summarize project risks".to_string(),
			expected_output_tokens: 300,
			risk_tier,
			preferred_provider: None,
			budget_tokens_remaining: 4_000,
			budget_cost_remaining_usd: 2.0,
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

	#[test]
	fn high_risk_request_prefers_high_risk_model() {
		let router = router_with_models();
		let request = sample_request(RiskTier::High);

		let response = router
			.generate(&request)
			.expect("generation should succeed");
		assert_eq!(response.provider, "provider-b");
		assert_eq!(response.model_id, "b-pro");
	}

	#[test]
	fn low_risk_request_prefers_lower_cost_model() {
		let router = router_with_models();
		let request = sample_request(RiskTier::Low);

		let response = router
			.generate(&request)
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
			.generate(&request)
			.expect("generation should succeed");
		assert_eq!(response.provider, "provider-b");
	}

	#[test]
	fn request_fails_when_budget_cost_is_too_low() {
		let router = router_with_models();
		let mut request = sample_request(RiskTier::Low);
		request.budget_cost_remaining_usd = 0.0001;

		let error = router
			.generate(&request)
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
			.generate(&sample_request(RiskTier::Medium))
			.expect_err("request should fail due to latency");
		assert!(matches!(
			error,
			LlmAdapterError::LatencyExceeded {
				latency_ms: 150,
				max_latency_ms: 100
			}
		));
	}
}
