use std::env;
use std::sync::Arc;
use std::time::Instant;

use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use roku_observability::Metrics;
use serde::Deserialize;
use serde_json::{Value, json};
use thiserror::Error;

use crate::router::{LlmProvider, LlmRouter};
use crate::types::{
	GenerationRequest, ModelProfile, ProviderResponse, RiskTier, RoutingPolicy,
	estimate_prompt_tokens,
};

const OPENROUTER_PROVIDER: &str = "openrouter";
const DEFAULT_OPENROUTER_URL: &str = "https://openrouter.ai/api/v1/chat/completions";
const DEFAULT_OPENROUTER_MODEL: &str = "openrouter/free";

#[derive(Debug, Clone, PartialEq)]
pub struct OpenRouterConfig {
	pub api_key: String,
	pub model: Option<String>,
	pub app_name: Option<String>,
	pub site_url: Option<String>,
	pub base_url: String,
	pub max_context_tokens: u64,
	pub cost_per_1k_tokens_usd: f64,
	pub max_request_cost_usd: f64,
	pub max_latency_ms: u64,
}

impl OpenRouterConfig {
	pub fn from_env() -> Result<Self, OpenRouterBootstrapError> {
		let api_key = env_var_required("OPENROUTER_API_KEY")?;
		let model = env::var("OPENROUTER_MODEL")
			.ok()
			.filter(|value| !value.trim().is_empty())
			.or_else(|| Some(DEFAULT_OPENROUTER_MODEL.to_string()));
		let app_name = env::var("OPENROUTER_APP_NAME")
			.ok()
			.filter(|value| !value.trim().is_empty());
		let site_url = env::var("OPENROUTER_SITE_URL")
			.ok()
			.filter(|value| !value.trim().is_empty());
		let base_url = env::var("OPENROUTER_BASE_URL")
			.ok()
			.filter(|value| !value.trim().is_empty())
			.unwrap_or_else(|| DEFAULT_OPENROUTER_URL.to_string());
		let max_context_tokens = env_var_u64("OPENROUTER_MAX_CONTEXT_TOKENS")?.unwrap_or(128_000);
		let cost_per_1k_tokens_usd =
			env_var_f64("OPENROUTER_COST_PER_1K_TOKENS_USD")?.unwrap_or(0.0);
		let max_request_cost_usd = env_var_f64("OPENROUTER_MAX_REQUEST_COST_USD")?.unwrap_or(1.0);
		let max_latency_ms = env_var_u64("OPENROUTER_MAX_LATENCY_MS")?.unwrap_or(60_000);

		Ok(Self {
			api_key,
			model,
			app_name,
			site_url,
			base_url,
			max_context_tokens,
			cost_per_1k_tokens_usd,
			max_request_cost_usd,
			max_latency_ms,
		})
	}

	fn model_id(&self) -> String {
		self.model
			.clone()
			.unwrap_or_else(|| DEFAULT_OPENROUTER_MODEL.to_string())
	}
}

#[derive(Debug, Error)]
pub enum OpenRouterBootstrapError {
	#[error("missing required environment variable: {0}")]
	MissingEnv(&'static str),
	#[error("invalid environment variable {key}: {message}")]
	InvalidEnv { key: &'static str, message: String },
	#[error("failed to construct openrouter http client: {0}")]
	HttpClient(#[from] reqwest::Error),
}

pub struct OpenRouterProvider {
	client: Client,
	config: OpenRouterConfig,
}

impl OpenRouterProvider {
	pub fn new(config: OpenRouterConfig) -> Result<Self, OpenRouterBootstrapError> {
		let client = Client::builder().build()?;
		Ok(Self { client, config })
	}
}

impl LlmProvider for OpenRouterProvider {
	fn provider_name(&self) -> &'static str {
		OPENROUTER_PROVIDER
	}

	fn complete(
		&self,
		model: &ModelProfile,
		request: &GenerationRequest,
	) -> Result<ProviderResponse, String> {
		let body = build_request_body(model, request);
		let mut headers = HeaderMap::new();
		headers.insert(
			reqwest::header::AUTHORIZATION,
			HeaderValue::from_str(&format!("Bearer {}", self.config.api_key))
				.map_err(|error| format!("invalid authorization header: {error}"))?,
		);
		headers.insert(
			reqwest::header::CONTENT_TYPE,
			HeaderValue::from_static("application/json"),
		);
		if let Some(site_url) = &self.config.site_url {
			headers.insert(
				HeaderName::from_static("http-referer"),
				HeaderValue::from_str(site_url)
					.map_err(|error| format!("invalid HTTP-Referer header: {error}"))?,
			);
		}
		if let Some(app_name) = &self.config.app_name {
			headers.insert(
				HeaderName::from_static("x-title"),
				HeaderValue::from_str(app_name)
					.map_err(|error| format!("invalid X-Title header: {error}"))?,
			);
		}

		let started_at = Instant::now();
		let response = self
			.client
			.post(&self.config.base_url)
			.headers(headers)
			.json(&body)
			.send()
			.map_err(|error| format!("request failed: {error}"))?;
		let status = response.status();
		let response_body = response
			.text()
			.map_err(|error| format!("failed to read response body: {error}"))?;
		if !status.is_success() {
			return Err(format!(
				"openrouter returned status {status}: {response_body}"
			));
		}

		let parsed = parse_response(&response_body)?;
		let latency_ms = u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
		Ok(ProviderResponse {
			output: parsed.output,
			prompt_tokens: parsed.prompt_tokens,
			output_tokens: parsed.output_tokens,
			latency_ms,
		})
	}
}

pub fn build_openrouter_router(
	config: OpenRouterConfig,
) -> Result<LlmRouter, OpenRouterBootstrapError> {
	build_openrouter_router_with_metrics(config, Arc::new(Metrics::default()))
}

pub fn build_openrouter_router_with_metrics(
	config: OpenRouterConfig,
	metrics: Arc<Metrics>,
) -> Result<LlmRouter, OpenRouterBootstrapError> {
	let mut router = LlmRouter::new(RoutingPolicy {
		max_request_cost_usd: config.max_request_cost_usd,
		max_latency_ms: config.max_latency_ms,
	})
	.with_metrics(metrics);
	router.register_provider(OpenRouterProvider::new(config.clone())?);
	router.register_model(ModelProfile {
		model_id: config.model_id(),
		provider: OPENROUTER_PROVIDER.to_string(),
		max_context_tokens: config.max_context_tokens,
		cost_per_1k_tokens_usd: config.cost_per_1k_tokens_usd,
		max_risk_tier: RiskTier::Critical,
		route_priority: 100,
	});
	Ok(router)
}

fn build_request_body(model: &ModelProfile, request: &GenerationRequest) -> Value {
	json!({
		"model": model.model_id,
		"messages": [
			{
				"role": "user",
				"content": request.prompt,
			}
		],
		"max_tokens": request.expected_output_tokens,
	})
}

struct ParsedOpenRouterResponse {
	output: String,
	prompt_tokens: u64,
	output_tokens: u64,
}

fn parse_response(response_body: &str) -> Result<ParsedOpenRouterResponse, String> {
	let response: OpenRouterResponse = serde_json::from_str(response_body)
		.map_err(|error| format!("invalid response json: {error}"))?;
	let choice = response
		.choices
		.into_iter()
		.next()
		.ok_or_else(|| "openrouter response contained no choices".to_string())?;
	let output = match choice.message.content {
		OpenRouterMessageContent::Text(text) => text,
		OpenRouterMessageContent::Parts(parts) => {
			let text = parts
				.into_iter()
				.filter_map(|part| part.text)
				.collect::<Vec<_>>()
				.join("");
			if text.is_empty() {
				return Err("openrouter response content parts contained no text".to_string());
			}
			text
		}
	};

	let prompt_tokens = response
		.usage
		.as_ref()
		.and_then(|usage| usage.prompt_tokens)
		.unwrap_or_else(|| estimate_prompt_tokens(&output));
	let output_tokens = response
		.usage
		.as_ref()
		.and_then(|usage| usage.completion_tokens)
		.unwrap_or_else(|| estimate_prompt_tokens(&output));

	Ok(ParsedOpenRouterResponse {
		output,
		prompt_tokens,
		output_tokens,
	})
}

#[derive(Debug, Deserialize)]
struct OpenRouterResponse {
	choices: Vec<OpenRouterChoice>,
	#[serde(default)]
	usage: Option<OpenRouterUsage>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterChoice {
	message: OpenRouterMessage,
}

#[derive(Debug, Deserialize)]
struct OpenRouterMessage {
	content: OpenRouterMessageContent,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum OpenRouterMessageContent {
	Text(String),
	Parts(Vec<OpenRouterContentPart>),
}

#[derive(Debug, Deserialize)]
struct OpenRouterContentPart {
	#[serde(default)]
	text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterUsage {
	#[serde(default)]
	prompt_tokens: Option<u64>,
	#[serde(default)]
	completion_tokens: Option<u64>,
}

fn env_var_required(key: &'static str) -> Result<String, OpenRouterBootstrapError> {
	let value = env::var(key).map_err(|_| OpenRouterBootstrapError::MissingEnv(key))?;
	if value.trim().is_empty() {
		return Err(OpenRouterBootstrapError::MissingEnv(key));
	}
	Ok(value)
}

fn env_var_u64(key: &'static str) -> Result<Option<u64>, OpenRouterBootstrapError> {
	match env::var(key) {
		Ok(value) if !value.trim().is_empty() => {
			value
				.parse::<u64>()
				.map(Some)
				.map_err(|error| OpenRouterBootstrapError::InvalidEnv {
					key,
					message: error.to_string(),
				})
		}
		Ok(_) | Err(env::VarError::NotPresent) => Ok(None),
		Err(error) => Err(OpenRouterBootstrapError::InvalidEnv {
			key,
			message: error.to_string(),
		}),
	}
}

fn env_var_f64(key: &'static str) -> Result<Option<f64>, OpenRouterBootstrapError> {
	match env::var(key) {
		Ok(value) if !value.trim().is_empty() => {
			value
				.parse::<f64>()
				.map(Some)
				.map_err(|error| OpenRouterBootstrapError::InvalidEnv {
					key,
					message: error.to_string(),
				})
		}
		Ok(_) | Err(env::VarError::NotPresent) => Ok(None),
		Err(error) => Err(OpenRouterBootstrapError::InvalidEnv {
			key,
			message: error.to_string(),
		}),
	}
}

#[cfg(test)]
mod tests {
	use crate::types::{GenerationRequest, RiskTier};

	use super::*;

	fn sample_request() -> GenerationRequest {
		GenerationRequest {
			prompt: "reply with a short greeting".to_string(),
			expected_output_tokens: 64,
			risk_tier: RiskTier::Low,
			preferred_provider: None,
			budget_tokens_remaining: 1024,
			budget_cost_remaining_usd: 0.1,
		}
	}

	#[test]
	fn request_body_uses_free_router_by_default() {
		let body = build_request_body(
			&ModelProfile {
				model_id: DEFAULT_OPENROUTER_MODEL.to_string(),
				provider: "openrouter".to_string(),
				max_context_tokens: 128_000,
				cost_per_1k_tokens_usd: 0.0,
				max_risk_tier: RiskTier::Critical,
				route_priority: 100,
			},
			&sample_request(),
		);

		assert_eq!(body["model"], "openrouter/free");
		assert_eq!(body["messages"][0]["role"], "user");
	}

	#[test]
	fn parse_response_reads_string_content() {
		let parsed = parse_response(
			r#"{
				"choices":[{"message":{"content":"hello from openrouter"}}],
				"usage":{"prompt_tokens":10,"completion_tokens":4}
			}"#,
		)
		.expect("response should parse");

		assert_eq!(parsed.output, "hello from openrouter");
		assert_eq!(parsed.prompt_tokens, 10);
		assert_eq!(parsed.output_tokens, 4);
	}

	#[test]
	fn parse_response_reads_text_parts() {
		let parsed = parse_response(
			r#"{
				"choices":[{"message":{"content":[{"text":"hello "},{"text":"world"}]}}],
				"usage":{"prompt_tokens":12,"completion_tokens":6}
			}"#,
		)
		.expect("response should parse");

		assert_eq!(parsed.output, "hello world");
		assert_eq!(parsed.prompt_tokens, 12);
		assert_eq!(parsed.output_tokens, 6);
	}
}
