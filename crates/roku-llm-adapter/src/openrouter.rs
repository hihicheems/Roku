use std::env;
use std::sync::Arc;
use std::time::Instant;

use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use roku_observability::Metrics;
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

use crate::router::{LlmProvider, LlmRouter};
use crate::types::{
	GenerationRequest, ModelProfile, ProviderResponse, RiskTier, RoutingPolicy,
	estimate_prompt_tokens,
};

const OPENROUTER_PROVIDER: &str = "openrouter";
const DEFAULT_OPENROUTER_URL: &str = "https://openrouter.ai/api/v1/chat/completions";
const DEFAULT_OPENROUTER_PRIMARY_MODEL: &str = "step-3.5-flash:free";
const DEFAULT_OPENROUTER_FALLBACK_MODELS: [&str; 2] = ["deepseek-chat", "gemini-2.0-flash"];

#[derive(Debug, Clone, PartialEq)]
pub struct OpenRouterConfig {
	pub api_key: String,
	pub primary_model: String,
	pub fallback_models: Vec<String>,
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
		let primary_model = env::var("OPENROUTER_PRIMARY_MODEL")
			.ok()
			.filter(|value| !value.trim().is_empty())
			.or_else(|| {
				env::var("OPENROUTER_MODEL")
					.ok()
					.filter(|value| !value.trim().is_empty())
			})
			.unwrap_or_else(|| DEFAULT_OPENROUTER_PRIMARY_MODEL.to_string());
		let fallback_models = env::var("OPENROUTER_FALLBACK_MODELS")
			.ok()
			.map(|value| parse_model_list(&value))
			.filter(|models| !models.is_empty())
			.unwrap_or_else(default_fallback_models);
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

		let primary_model = normalize_model_id(&primary_model);
		let fallback_models = dedupe_model_chain(&primary_model, fallback_models);

		Ok(Self {
			api_key,
			primary_model,
			fallback_models,
			app_name,
			site_url,
			base_url,
			max_context_tokens,
			cost_per_1k_tokens_usd,
			max_request_cost_usd,
			max_latency_ms,
		})
	}

	fn request_fallback_chain(&self, selected_model: &str) -> Vec<String> {
		let mut ordered_models = Vec::with_capacity(self.fallback_models.len().saturating_add(1));
		ordered_models.push(self.primary_model.clone());
		ordered_models.extend(self.fallback_models.iter().cloned());

		let start_index = ordered_models
			.iter()
			.position(|model| model == selected_model)
			.map(|index| index.saturating_add(1))
			.unwrap_or_default();

		dedupe_model_chain(
			selected_model,
			ordered_models.into_iter().skip(start_index).collect(),
		)
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
		let body = build_request_body(&self.config, model, request);
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
				HeaderName::from_static("x-openrouter-title"),
				HeaderValue::from_str(app_name)
					.map_err(|error| format!("invalid X-OpenRouter-Title header: {error}"))?,
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
			eprintln!(
				"[openrouter] provider={} model={} status={} body={}",
				OPENROUTER_PROVIDER,
				model.model_id,
				status,
				truncate_for_log(&response_body, 800),
			);
			return Err(format!(
				"openrouter returned status {status}: {response_body}"
			));
		}

		let parsed = parse_response(&response_body).inspect_err(|error| {
			eprintln!(
				"[openrouter] provider={} model={} parse_error={} body={}",
				OPENROUTER_PROVIDER,
				model.model_id,
				error,
				truncate_for_log(&response_body, 800),
			);
		})?;
		let latency_ms = u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
		let served_model = parsed.served_model_id.as_deref().unwrap_or(&model.model_id);
		eprintln!(
			"[openrouter] provider={} requested_model={} served_model={} status=ok latency_ms={} prompt_tokens={} output_tokens={}",
			OPENROUTER_PROVIDER,
			model.model_id,
			served_model,
			latency_ms,
			parsed.prompt_tokens,
			parsed.output_tokens,
		);
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
		model_id: config.primary_model.clone(),
		provider: OPENROUTER_PROVIDER.to_string(),
		max_context_tokens: config.max_context_tokens,
		cost_per_1k_tokens_usd: config.cost_per_1k_tokens_usd,
		max_risk_tier: RiskTier::Critical,
		route_priority: 100,
	});
	Ok(router)
}

fn build_request_body<'a>(
	config: &'a OpenRouterConfig,
	model: &'a ModelProfile,
	request: &'a GenerationRequest,
) -> OpenAiChatCompletionRequest<'a> {
	let mut messages = Vec::with_capacity(2);
	if let Some(system_prompt) = request.system_prompt.as_deref() {
		messages.push(OpenAiChatCompletionMessage {
			role: "system",
			content: system_prompt,
		});
	}
	messages.push(OpenAiChatCompletionMessage {
		role: "user",
		content: &request.prompt,
	});

	OpenAiChatCompletionRequest {
		model: &model.model_id,
		models: config.request_fallback_chain(&model.model_id),
		messages,
		max_tokens: request.expected_output_tokens,
	}
}

struct ParsedOpenRouterResponse {
	output: String,
	prompt_tokens: u64,
	output_tokens: u64,
	served_model_id: Option<String>,
}

fn parse_response(response_body: &str) -> Result<ParsedOpenRouterResponse, String> {
	let response: Value = serde_json::from_str(response_body)
		.map_err(|error| format!("invalid response json: {error}"))?;
	let message = response
		.get("choices")
		.and_then(Value::as_array)
		.and_then(|choices| choices.first())
		.and_then(|choice| choice.get("message"))
		.ok_or_else(|| "openrouter response contained no message payload".to_string())?;
	let output = extract_content_text(message).ok_or_else(|| {
		format!(
			"openrouter response message contained no readable text: {}",
			truncate_for_log(&message.to_string(), 400)
		)
	})?;

	let prompt_tokens = response
		.get("usage")
		.and_then(|usage| usage.get("prompt_tokens"))
		.and_then(Value::as_u64)
		.unwrap_or_else(|| estimate_prompt_tokens(&output));
	let output_tokens = response
		.get("usage")
		.and_then(|usage| usage.get("completion_tokens"))
		.and_then(Value::as_u64)
		.unwrap_or_else(|| estimate_prompt_tokens(&output));
	let served_model_id = response
		.get("model")
		.and_then(Value::as_str)
		.map(str::to_string);

	Ok(ParsedOpenRouterResponse {
		output,
		prompt_tokens,
		output_tokens,
		served_model_id,
	})
}

#[derive(Debug, Serialize)]
struct OpenAiChatCompletionRequest<'a> {
	model: &'a str,
	#[serde(skip_serializing_if = "Vec::is_empty")]
	models: Vec<String>,
	messages: Vec<OpenAiChatCompletionMessage<'a>>,
	max_tokens: u64,
}

#[derive(Debug, Serialize)]
struct OpenAiChatCompletionMessage<'a> {
	role: &'static str,
	content: &'a str,
}

fn extract_content_text(content: &Value) -> Option<String> {
	match content {
		Value::String(text) => Some(text.clone()),
		Value::Array(parts) => {
			let text = parts
				.iter()
				.filter_map(extract_content_text)
				.collect::<Vec<_>>()
				.join("");
			if text.trim().is_empty() {
				None
			} else {
				Some(text)
			}
		}
		Value::Object(object) => object
			.get("text")
			.and_then(Value::as_str)
			.map(str::to_string)
			.or_else(|| object.get("content").and_then(extract_content_text))
			.or_else(|| {
				object
					.get("refusal")
					.and_then(Value::as_str)
					.map(str::to_string)
			})
			.or_else(|| object.get("reasoning").and_then(extract_content_text)),
		_ => None,
	}
}

fn truncate_for_log(value: &str, max_chars: usize) -> String {
	let mut chars = value.chars();
	let truncated = chars.by_ref().take(max_chars).collect::<String>();
	if chars.next().is_some() {
		format!("{truncated}...")
	} else {
		truncated
	}
}

fn default_fallback_models() -> Vec<String> {
	DEFAULT_OPENROUTER_FALLBACK_MODELS
		.iter()
		.map(|model| normalize_model_id(model))
		.collect()
}

fn parse_model_list(value: &str) -> Vec<String> {
	value
		.split(',')
		.map(str::trim)
		.filter(|model| !model.is_empty())
		.map(normalize_model_id)
		.collect()
}

fn dedupe_model_chain(primary_model: &str, candidates: Vec<String>) -> Vec<String> {
	let mut deduped = Vec::new();
	for candidate in candidates {
		if candidate == primary_model || deduped.iter().any(|model| model == &candidate) {
			continue;
		}
		deduped.push(candidate);
	}
	deduped
}

fn normalize_model_id(model_id: &str) -> String {
	match model_id.trim() {
		"step-3.5-flash" => "stepfun/step-3.5-flash".to_string(),
		"step-3.5-flash:free" => "stepfun/step-3.5-flash:free".to_string(),
		"deepseek-chat" => "deepseek/deepseek-chat".to_string(),
		"gemini-2.0-flash" => "google/gemini-2.0-flash-001".to_string(),
		other => other.to_string(),
	}
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
	use serde_json::json;

	use crate::types::{GenerationRequest, RiskTier};

	use super::*;

	fn sample_request() -> GenerationRequest {
		GenerationRequest {
			system_prompt: None,
			prompt: "reply with a short greeting".to_string(),
			expected_output_tokens: 64,
			risk_tier: RiskTier::Low,
			preferred_provider: None,
			budget_tokens_remaining: 1024,
			budget_cost_remaining_usd: 0.1,
		}
	}

	#[test]
	fn request_body_uses_primary_model_and_fallback_chain_by_default() {
		let config = OpenRouterConfig {
			api_key: "test-key".to_string(),
			primary_model: normalize_model_id(DEFAULT_OPENROUTER_PRIMARY_MODEL),
			fallback_models: default_fallback_models(),
			app_name: None,
			site_url: None,
			base_url: DEFAULT_OPENROUTER_URL.to_string(),
			max_context_tokens: 128_000,
			cost_per_1k_tokens_usd: 0.0,
			max_request_cost_usd: 1.0,
			max_latency_ms: 60_000,
		};
		let body = serde_json::to_value(build_request_body(
			&config,
			&ModelProfile {
				model_id: config.primary_model.clone(),
				provider: "openrouter".to_string(),
				max_context_tokens: 128_000,
				cost_per_1k_tokens_usd: 0.0,
				max_risk_tier: RiskTier::Critical,
				route_priority: 100,
			},
			&sample_request(),
		))
		.expect("request body should serialize");

		assert_eq!(body["model"], "stepfun/step-3.5-flash:free");
		assert_eq!(
			body["models"],
			json!(vec![
				"deepseek/deepseek-chat",
				"google/gemini-2.0-flash-001"
			]),
		);
		assert_eq!(body["messages"][0]["role"], "user");
	}

	#[test]
	fn request_body_includes_system_message_when_present() {
		let config = OpenRouterConfig {
			api_key: "test-key".to_string(),
			primary_model: normalize_model_id(DEFAULT_OPENROUTER_PRIMARY_MODEL),
			fallback_models: default_fallback_models(),
			app_name: None,
			site_url: None,
			base_url: DEFAULT_OPENROUTER_URL.to_string(),
			max_context_tokens: 128_000,
			cost_per_1k_tokens_usd: 0.0,
			max_request_cost_usd: 1.0,
			max_latency_ms: 60_000,
		};
		let body = serde_json::to_value(build_request_body(
			&config,
			&ModelProfile {
				model_id: config.primary_model.clone(),
				provider: "openrouter".to_string(),
				max_context_tokens: 128_000,
				cost_per_1k_tokens_usd: 0.0,
				max_risk_tier: RiskTier::Critical,
				route_priority: 100,
			},
			&GenerationRequest {
				system_prompt: Some("You are Roku.".to_string()),
				..sample_request()
			},
		))
		.expect("request body should serialize");

		assert_eq!(body["messages"][0]["role"], "system");
		assert_eq!(body["messages"][0]["content"], "You are Roku.");
		assert_eq!(body["messages"][1]["role"], "user");
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

	#[test]
	fn parse_response_reads_object_content_payload() {
		let parsed = parse_response(
			r#"{
				"choices":[{"message":{"content":{"type":"text","text":"hello from object payload"}}}],
				"usage":{"prompt_tokens":14,"completion_tokens":5}
			}"#,
		)
		.expect("object-shaped content should parse");

		assert_eq!(parsed.output, "hello from object payload");
		assert_eq!(parsed.prompt_tokens, 14);
		assert_eq!(parsed.output_tokens, 5);
	}

	#[test]
	fn parse_response_falls_back_to_reasoning_when_content_is_null() {
		let parsed = parse_response(
			r#"{
				"model":"deepseek/deepseek-chat",
				"choices":[{"message":{"role":"assistant","content":null,"reasoning":"hello from reasoning"}}],
				"usage":{"prompt_tokens":9,"completion_tokens":3}
			}"#,
		)
		.expect("reasoning fallback should parse");

		assert_eq!(parsed.output, "hello from reasoning");
		assert_eq!(parsed.prompt_tokens, 9);
		assert_eq!(parsed.output_tokens, 3);
		assert_eq!(
			parsed.served_model_id.as_deref(),
			Some("deepseek/deepseek-chat")
		);
	}

	#[test]
	fn normalize_model_id_maps_supported_aliases() {
		assert_eq!(
			normalize_model_id("step-3.5-flash:free"),
			"stepfun/step-3.5-flash:free",
		);
		assert_eq!(
			normalize_model_id("deepseek-chat"),
			"deepseek/deepseek-chat"
		);
		assert_eq!(
			normalize_model_id("gemini-2.0-flash"),
			"google/gemini-2.0-flash-001",
		);
	}
}
