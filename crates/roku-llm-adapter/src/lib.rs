//! Multi-provider model routing with budget and risk-aware controls.

mod openrouter;
mod router;
mod types;

pub use openrouter::{
	OpenRouterBootstrapError, OpenRouterConfig, OpenRouterProvider, build_openrouter_router,
	build_openrouter_router_with_metrics,
};
pub use router::{LlmProvider, LlmRouter};
pub use types::{
	GenerationRequest, LlmAdapterError, LlmResponse, ModelProfile, ProviderCallError,
	ProviderResiliencePolicy, ProviderResponse, RiskTier, RoutingPolicy,
};
