use std::collections::HashMap;
use std::sync::Arc;

use roku_observability::{LlmInvocationOutcome, Metrics};

use crate::types::{
	GenerationRequest, LlmAdapterError, LlmResponse, ModelProfile, ProviderResponse, RiskTier,
	RoutingPolicy, estimate_cost_usd,
};

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
	metrics: Option<Arc<Metrics>>,
}

impl LlmRouter {
	pub fn new(policy: RoutingPolicy) -> Self {
		Self {
			policy,
			models: Vec::new(),
			providers: HashMap::new(),
			metrics: None,
		}
	}

	pub fn with_metrics(mut self, metrics: Arc<Metrics>) -> Self {
		self.metrics = Some(metrics);
		self
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

		let provider_response = provider
			.complete(selected_model, request)
			.map_err(|message| LlmAdapterError::ProviderCallFailed {
				provider: selected_model.provider.clone(),
				model_id: selected_model.model_id.clone(),
				message,
			});
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
			prompt_tokens: provider_response.prompt_tokens,
			output_tokens: provider_response.output_tokens,
			total_tokens,
			estimated_cost_usd,
			latency_ms: provider_response.latency_ms,
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

#[cfg(test)]
mod tests {
	use std::sync::Arc;

	use roku_observability::Metrics;

	use crate::types::{
		GenerationRequest, ModelProfile, ProviderResponse, RiskTier, RoutingPolicy,
	};

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
			system_prompt: None,
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

	#[test]
	fn router_records_provider_metrics() {
		let metrics = Arc::new(Metrics::default());
		let router = router_with_models().with_metrics(metrics.clone());

		router
			.generate(&sample_request(RiskTier::Low))
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
			.generate(&request)
			.expect_err("request should fail without an eligible model");
		assert!(matches!(error, LlmAdapterError::NoEligibleModel));

		let snapshot = metrics.snapshot();
		assert_eq!(snapshot.llm_requests_total, 1);
		assert_eq!(snapshot.llm_failures_total, 1);
		assert_eq!(snapshot.llm_routing_failures_total, 1);
		assert!(metrics.llm_provider_metrics().is_empty());
	}
}
