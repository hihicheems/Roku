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

//! Multi-provider model routing with budget and risk-aware controls.

mod bootstrap;
pub mod model_cost;
mod providers;
mod retry;
mod router;
mod types;

pub use bootstrap::LlmProviderKind;
pub use providers::anthropic::{
	AnthropicBootstrapError, AnthropicConfig, AnthropicProvider, AnthropicRuntimeConfig,
	AnthropicRuntimeConfigPatch, anthropic_api_key_from_env, build_anthropic_router_with_metrics,
};
pub use providers::openai::{
	OpenAiBootstrapError, OpenAiConfig, OpenAiProvider, OpenAiRuntimeConfig,
	OpenAiRuntimeConfigPatch, build_openai_router_with_metrics, openai_api_key_from_env,
};
pub use providers::openai_responses::{
	OpenAiResponsesBootstrapError, OpenAiResponsesConfig, OpenAiResponsesProvider,
	build_openai_responses_router_with_metrics,
};
pub use providers::openrouter::{
	OpenRouterBootstrapError, OpenRouterConfig, OpenRouterProvider, OpenRouterRuntimeConfig,
	OpenRouterRuntimeConfigPatch, build_openrouter_router, build_openrouter_router_with_metrics,
};
pub use router::{LlmProvider, LlmRouter};
pub use types::{
	GenerationRequest, LlmAdapterError, LlmResponse, Message, ModelCostProfile, ModelProfile,
	ProviderCallError, ProviderResiliencePolicy, ProviderResponse, RiskTier, RoutingPolicy,
	StreamChunk, StructuredGenerationError, StructuredJsonResponse, StructuredOutputError,
	SystemPromptBlock, SystemPromptSections, ThinkingEffort, ToolCallBlock, ToolDefinition,
};
