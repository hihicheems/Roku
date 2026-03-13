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

use crate::runtime_config::{HARD_MAX_WEB_TOP_K, WebToolRuntimeConfig};
use reqwest::blocking::Client;
use roku_plugin_catalog::{CatalogDescriptor, ResourceCost, ResourceKind, ResourceRisk};
use roku_plugin_host::{
	RuntimeConstraints, SandboxProfile, Tool, ToolDescriptor, ToolFailure, ToolInvocationRequest,
	ToolRuntime, ToolRuntimeError, ToolSchema,
};
use serde_json::{Value, json};

#[allow(dead_code)]
pub(crate) fn catalog_descriptors() -> Vec<CatalogDescriptor> {
	catalog_descriptors_with_config(&WebToolRuntimeConfig::default())
}

pub(crate) fn catalog_descriptors_with_config(
	_runtime_config: &WebToolRuntimeConfig,
) -> Vec<CatalogDescriptor> {
	vec![CatalogDescriptor {
		selector: roku_common_types::ResourceSelector::tool("web.search"),
		kind: ResourceKind::Tool,
		name: "web.search".to_string(),
		role: Some("core_web".to_string()),
		description:
			"Search the web through a configured HTTP JSON backend and return structured result summaries."
				.to_string(),
		discoverable: true,
		tags: vec!["web".to_string(), "search".to_string(), "lookup".to_string()],
		examples: vec!["Search the web for the latest Rust edition.".to_string()],
		input_schema: vec!["query".to_string(), "top_k".to_string()],
		risk: ResourceRisk::Low,
		cost: ResourceCost {
			estimated_tokens: 0,
			estimated_latency_ms: 3_000,
		},
		required_capabilities: vec!["web.search".to_string()],
		summary: "Search the web through a configured HTTP backend.".to_string(),
		key_commands: Vec::new(),
		use_cases: Vec::new(),
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
		ToolDescriptor {
			name: "web.search".to_string(),
			version: "1.0.0".to_string(),
			input_schema: ToolSchema {
				required_fields: vec![
					"task_id".to_string(),
					"node_id".to_string(),
					"goal".to_string(),
					"summary".to_string(),
					"conversation_history".to_string(),
					"budget_tokens".to_string(),
					"time_budget_ms".to_string(),
					"query".to_string(),
				],
			},
			output_schema: "result.v1".to_string(),
			required_capabilities: vec!["web.search".to_string()],
			runtime_constraints: RuntimeConstraints {
				timeout_ms: 10_000,
				max_retries: 0,
				retry_backoff_ms: 0,
				sandbox_profile: SandboxProfile::NoIsolation,
				deterministic_hooks: true,
				allowed_read_roots: Vec::new(),
				allowed_write_roots: Vec::new(),
			},
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
		let endpoint = self
			.config
			.endpoint
			.clone()
			.ok_or_else(|| ToolFailure::terminal("web search endpoint is not configured"))?;
		let client = Client::builder()
			.timeout(Duration::from_millis(8_000))
			.build()
			.map_err(|error| {
				ToolFailure::terminal(format!("failed to build search client: {error}"))
			})?;
		let mut builder = client
			.get(endpoint)
			.query(&[("q", query), ("top_k", &top_k.to_string())]);
		if let Some((name, value)) = optional_auth_header() {
			builder = builder.header(name, value);
		}
		let response = builder.send().map_err(|error| {
			ToolFailure::terminal(format!("web search backend request failed: {error}"))
		})?;
		let status = response.status();
		if !status.is_success() {
			return Err(ToolFailure::terminal(format!(
				"web search backend returned http {}",
				status.as_u16()
			)));
		}
		let payload = response.json::<Value>().map_err(|error| {
			ToolFailure::terminal(format!("web search backend returned invalid json: {error}"))
		})?;
		let results = parse_results(&payload, top_k);
		let message = render_result_message(query, &results);
		Ok(json!({
			"message": message,
			"query": query,
			"results": results,
			"top_k": top_k,
		}))
	}
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
