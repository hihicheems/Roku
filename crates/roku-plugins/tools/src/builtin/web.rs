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

use std::env;
use std::time::Duration;

use crate::contract::{
	contract_input_schema, contract_tool_schema, input_contract, input_field, output_contract,
	runtime_contract, selection_contract,
};
use crate::runtime_config::{HARD_MAX_WEB_TOP_K, WebToolRuntimeConfig};
use reqwest::blocking::Client;
use roku_common_types::{ToolContract, ToolOutputEnvelope, ToolRetryPolicy, ToolSideEffectPolicy};
use roku_plugin_catalog::{CatalogDescriptor, ResourceCost, ResourceKind, ResourceRisk};
use roku_plugin_host::{
	RuntimeConstraints, SandboxProfile, Tool, ToolDescriptor, ToolFailure, ToolInvocationRequest,
	ToolRuntime, ToolRuntimeError,
};
use serde_json::{Value, json};

#[allow(dead_code)]
pub(crate) fn catalog_descriptors() -> Vec<CatalogDescriptor> {
	catalog_descriptors_with_config(&WebToolRuntimeConfig::default())
}

pub(crate) fn catalog_descriptors_with_config(
	runtime_config: &WebToolRuntimeConfig,
) -> Vec<CatalogDescriptor> {
	let contract = web_contract();
	vec![CatalogDescriptor {
		selector: roku_common_types::ResourceSelector::tool("web.search"),
		kind: ResourceKind::Tool,
		name: "web.search".to_string(),
		role: Some("core_web".to_string()),
		description: "Use this when you have a concrete search query and need fresh external search results from the configured backend. Do not use it for filesystem questions, broad research planning without a query, or as a substitute for final synthesis. It returns structured result summaries that usually need a follow-up explanation or comparison before the final answer."
			.to_string(),
		selection_hint: "Run a concrete web search query to gather fresh external results."
			.to_string(),
		discoverable: true,
		tags: vec!["web".to_string(), "search".to_string(), "lookup".to_string()],
		examples: vec!["Search the web for the latest Rust edition.".to_string()],
		input_schema: contract_input_schema(
			Some(&contract),
			&["query".to_string(), "top_k".to_string()],
		),
		risk: ResourceRisk::Low,
		cost: ResourceCost {
			estimated_tokens: 0,
			estimated_latency_ms: runtime_config
				.endpoint
				.as_ref()
				.map(|_| 3_000)
				.unwrap_or(500),
		},
		required_capabilities: vec!["web.search".to_string()],
		summary: "Run a concrete web query and return structured search results.".to_string(),
		key_commands: Vec::new(),
		use_cases: Vec::new(),
		contract: Some(contract),
	}]
}

#[allow(dead_code)]
pub(crate) fn register_tools(runtime: &mut ToolRuntime) -> Result<(), ToolRuntimeError> {
	register_tools_with_config(runtime, &WebToolRuntimeConfig::default())
}

pub(crate) fn register_tools_with_config(
	runtime: &mut ToolRuntime,
	config: &WebToolRuntimeConfig,
) -> Result<(), ToolRuntimeError> {
	runtime.register_tool(WebSearchTool {
		config: config.clone(),
	})?;
	Ok(())
}

#[derive(Clone)]
struct WebSearchTool {
	config: WebToolRuntimeConfig,
}

impl Tool for WebSearchTool {
	fn descriptor(&self) -> ToolDescriptor {
		let runtime_constraints = RuntimeConstraints {
			timeout_ms: 10_000,
			max_retries: 0,
			retry_backoff_ms: 0,
			sandbox_profile: SandboxProfile::NoIsolation,
			deterministic_hooks: true,
			allowed_read_roots: Vec::new(),
			allowed_write_roots: Vec::new(),
		};
		let contract = web_contract();
		ToolDescriptor {
			name: "web.search".to_string(),
			version: "1.0.0".to_string(),
			input_schema: contract_tool_schema(
				Some(&contract),
				&[
					"task_id",
					"node_id",
					"goal",
					"summary",
					"conversation_history",
					"budget_tokens",
					"time_budget_ms",
				],
			),
			output_schema: contract.output.observation_schema.clone(),
			required_capabilities: vec!["web.search".to_string()],
			runtime_constraints,
			contract: Some(contract),
		}
	}

	fn invoke(&self, request: ToolInvocationRequest) -> Result<Value, ToolFailure> {
		let query = request
			.input
			.get("query")
			.and_then(Value::as_str)
			.filter(|value| !value.trim().is_empty())
			.ok_or_else(|| ToolFailure::terminal("missing required field `query`"))?;
		let top_k = request
			.input
			.get("top_k")
			.and_then(Value::as_u64)
			.and_then(|value| usize::try_from(value).ok())
			.unwrap_or(self.config.default_top_k)
			.clamp(1, HARD_MAX_WEB_TOP_K);
		let Some(endpoint) = self.config.endpoint.clone() else {
			return Ok(error_output(
				"endpoint_not_configured",
				"web search endpoint is not configured",
				query,
				top_k,
				None,
			));
		};
		let client = Client::builder()
			.timeout(Duration::from_millis(8_000))
			.build()
			.map_err(|error| {
				ToolFailure::terminal(format!("failed to build search client: {error}"))
			})?;
		let mut builder = client
			.get(&endpoint)
			.query(&[("q", query), ("top_k", &top_k.to_string())]);
		if let Some((name, value)) = optional_auth_header() {
			builder = builder.header(name, value);
		}
		let response = match builder.send() {
			Ok(response) => response,
			Err(error) => {
				return Ok(error_output(
					"backend_request_failed",
					format!("web search backend request failed: {error}"),
					query,
					top_k,
					Some(json!({ "endpoint": endpoint })),
				));
			}
		};
		let status = response.status();
		if !status.is_success() {
			return Ok(error_output(
				"backend_http_error",
				format!("web search backend returned http {}", status.as_u16()),
				query,
				top_k,
				Some(json!({
					"endpoint": endpoint,
					"http_status": status.as_u16(),
				})),
			));
		}
		let payload = match response.json::<Value>() {
			Ok(payload) => payload,
			Err(error) => {
				return Ok(error_output(
					"backend_invalid_json",
					format!("web search backend returned invalid json: {error}"),
					query,
					top_k,
					Some(json!({ "endpoint": endpoint })),
				));
			}
		};
		let results = parse_results(&payload, top_k);
		let message = render_result_message(query, &results);
		Ok(ToolOutputEnvelope::new(
			true,
			Option::<String>::None,
			false,
			message,
			json!({
				"query": query,
				"results": results,
				"top_k": top_k,
				"endpoint": endpoint,
			}),
		)
		.into_value())
	}
}

fn error_output(
	error_type: &str,
	message: impl Into<String>,
	query: &str,
	top_k: usize,
	extra: Option<Value>,
) -> Value {
	let mut data = json!({
		"query": query,
		"top_k": top_k,
	});
	if let Some(extra) = extra
		&& let Some(object) = extra.as_object()
	{
		for (key, value) in object {
			data[key] = value.clone();
		}
	}
	ToolOutputEnvelope::new(false, Some(error_type), true, message, data).into_value()
}

fn optional_auth_header() -> Option<(String, String)> {
	let raw = env::var("ROKU_WEB_SEARCH_AUTH_HEADER").ok()?;
	let (name, value) = raw.split_once(':')?;
	let header_name = name.trim();
	let header_value = value.trim();
	(!header_name.is_empty() && !header_value.is_empty())
		.then(|| (header_name.to_string(), header_value.to_string()))
}

fn parse_results(payload: &Value, top_k: usize) -> Vec<Value> {
	let items = payload
		.get("results")
		.and_then(Value::as_array)
		.cloned()
		.or_else(|| payload.as_array().cloned())
		.unwrap_or_default();
	items
		.into_iter()
		.take(top_k)
		.filter_map(|item| {
			let object = item.as_object()?;
			let title = string_field(object, &["title", "name"])?;
			let url = string_field(object, &["url", "link"])?;
			let snippet =
				string_field(object, &["snippet", "summary", "content"]).unwrap_or_default();
			Some(json!({
				"title": title,
				"url": url,
				"snippet": snippet,
			}))
		})
		.collect()
}

fn string_field(object: &serde_json::Map<String, Value>, candidates: &[&str]) -> Option<String> {
	candidates.iter().find_map(|candidate| {
		object
			.get(*candidate)
			.and_then(Value::as_str)
			.map(str::to_string)
	})
}

fn render_result_message(query: &str, results: &[Value]) -> String {
	if results.is_empty() {
		return format!("No web results were returned for `{query}`.");
	}
	let lines = results
		.iter()
		.enumerate()
		.map(|(index, item)| {
			let title = item
				.get("title")
				.and_then(Value::as_str)
				.unwrap_or("(untitled)");
			let url = item.get("url").and_then(Value::as_str).unwrap_or("");
			let snippet = item
				.get("snippet")
				.and_then(Value::as_str)
				.filter(|value| !value.trim().is_empty())
				.unwrap_or("No snippet.");
			format!("{}. {} - {} ({})", index + 1, title, snippet, url)
		})
		.collect::<Vec<_>>();
	format!("Top web results for `{query}`:\n{}", lines.join("\n"))
}

fn web_contract() -> ToolContract {
	let runtime_constraints = RuntimeConstraints {
		timeout_ms: 10_000,
		max_retries: 0,
		retry_backoff_ms: 0,
		sandbox_profile: SandboxProfile::NoIsolation,
		deterministic_hooks: true,
		allowed_read_roots: Vec::new(),
		allowed_write_roots: Vec::new(),
	};
	ToolContract {
		selection: selection_contract(
			&[
				"Use when the user asks for a concrete web lookup and a fresh external search query is available.",
				"Best for gathering current external search results that will be synthesized later in the loop.",
			],
			&[
				"Do not use for local filesystem, table, or Python execution tasks.",
				"Do not use for vague research planning without a concrete search query.",
			],
			&[
				"Commonly confused with general.execute for knowledge questions that do not actually require fresh web results.",
				"Commonly confused with fs.find when the word `search` refers to workspace files rather than the public web.",
			],
		),
		input: input_contract(
			vec![
				input_field(
					"query",
					true,
					"A concrete external search query to send to the configured backend.",
					&["Reject when the request does not contain a concrete search query."],
				),
				input_field(
					"top_k",
					false,
					"Optional result count capped by the configured web.search maximum.",
					&["Reject when zero or larger than the hard maximum result count."],
				),
			],
			&["A web search backend endpoint must be configured before execution."],
		),
		output: output_contract(
			"Returns a bounded set of structured web results with title, url, snippet, query, and top_k.",
			"No-result searches still return ok=true with an explicit message saying that no results were returned.",
			&[
				"Missing endpoint configuration, backend request failures, HTTP errors, and invalid JSON all surface as explicit error_type values.",
			],
			true,
			false,
		),
		runtime: runtime_contract(
			&runtime_constraints,
			ToolSideEffectPolicy::ExternalMutation,
			ToolRetryPolicy::Never,
		),
	}
}

#[cfg(test)]
mod tests {
	use std::io::{Read, Write};
	use std::net::TcpListener;
	use std::thread;

	use super::*;
	use roku_common_types::ToolOutputEnvelope;

	fn invocation_request(input: Value) -> ToolInvocationRequest {
		ToolInvocationRequest {
			invocation_key: "test-web-search".to_string(),
			attempt: 1,
			input,
			sandbox_profile: SandboxProfile::NoIsolation,
			attachments: Vec::new(),
			allowed_read_roots: Vec::new(),
			allowed_write_roots: Vec::new(),
		}
	}

	fn spawn_mock_search_server(body: &'static str) -> String {
		let listener = TcpListener::bind("127.0.0.1:0").expect("mock listener should bind");
		let address = listener
			.local_addr()
			.expect("mock listener should expose an address");
		thread::spawn(move || {
			let Ok((mut stream, _)) = listener.accept() else {
				return;
			};
			let mut buffer = [0_u8; 1024];
			let _ = stream.read(&mut buffer);
			let response = format!(
				"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
				body.len(),
				body
			);
			let _ = stream.write_all(response.as_bytes());
		});
		format!("http://{address}/search")
	}

	#[test]
	fn descriptor_exposes_unified_contract() {
		let descriptor = WebSearchTool {
			config: WebToolRuntimeConfig::default(),
		}
		.descriptor();

		assert_eq!(descriptor.output_schema, "tool_observation.v1");
		assert!(descriptor.contract.is_some());
	}

	#[test]
	fn missing_endpoint_returns_terminal_envelope() {
		let tool = WebSearchTool {
			config: WebToolRuntimeConfig::default(),
		};
		let output = tool
			.invoke(invocation_request(json!({
				"query": "latest Rust edition",
			})))
			.expect("web.search should return a structured observation");
		let envelope = serde_json::from_value::<ToolOutputEnvelope>(output)
			.expect("web.search should emit ToolOutputEnvelope");

		assert!(!envelope.ok);
		assert_eq!(
			envelope.error_type.as_deref(),
			Some("endpoint_not_configured")
		);
		assert!(envelope.terminal);
	}

	#[test]
	fn successful_search_returns_structured_results() {
		let endpoint = spawn_mock_search_server(
			r#"{"results":[{"title":"Rust Edition","url":"https://example.test/rust","snippet":"2024 edition"}]}"#,
		);
		let tool = WebSearchTool {
			config: WebToolRuntimeConfig {
				endpoint: Some(endpoint),
				default_top_k: 5,
			},
		};
		let output = tool
			.invoke(invocation_request(json!({
				"query": "latest Rust edition",
				"top_k": 3,
			})))
			.expect("web.search success should return a structured observation");
		let envelope = serde_json::from_value::<ToolOutputEnvelope>(output)
			.expect("web.search success should emit ToolOutputEnvelope");

		assert!(envelope.ok);
		assert!(!envelope.terminal);
		assert_eq!(
			envelope
				.data
				.get("results")
				.and_then(Value::as_array)
				.map(Vec::len),
			Some(1)
		);
	}
}
