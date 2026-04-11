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
use std::sync::LazyLock;
use std::time::Duration;

use crate::contract::{
	contract_input_schema, contract_tool_schema, grounding_contract, grounding_contract_simple,
	input_contract, input_field, output_contract, runtime_contract, selection_contract,
};
use crate::runtime_config::{HARD_MAX_WEB_TOP_K, WebToolRuntimeConfig};
use reqwest::blocking::Client;
use roku_common_types::{CatalogDescriptor, ResourceCost, ResourceKind, ResourceRisk};
use roku_common_types::{
	ExtractionHint, GroundingStrategy, ToolContract, ToolOutputEnvelope, ToolRetryPolicy,
	ToolSideEffectPolicy,
};
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
	vec![
		CatalogDescriptor {
			selector: roku_common_types::ResourceSelector::tool("WebSearch"),
			kind: ResourceKind::Tool,
			name: "WebSearch".to_string(),
			role: Some("core_web".to_string()),
			description: "Use this when you have a concrete search query and need fresh external search results from the configured backend. Do not use it for filesystem questions, broad research planning without a query, or as a substitute for final synthesis. It returns structured result summaries that usually need a follow-up explanation or comparison before the final answer."
				.to_string(),
			selection_hint: "Search the web for current information. Use when the user asks about docs, package versions, APIs, news, or anything that requires up-to-date knowledge beyond training data."
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
				estimated_latency_ms: if runtime_config.endpoint.is_some()
					|| runtime_config.tavily_api_key.is_some()
				{
					3_000
				} else {
					500
				},
			},
			required_capabilities: vec!["WebSearch".to_string()],
			summary: "Run a concrete web query and return structured search results.".to_string(),
			key_commands: Vec::new(),
			use_cases: Vec::new(),
			contract: Some(contract),
		},
		CatalogDescriptor {
			selector: roku_common_types::ResourceSelector::tool("WebFetch"),
			kind: ResourceKind::Tool,
			name: "WebFetch".to_string(),
			role: Some("core_web".to_string()),
			description: "Use this when you have a specific URL and need to read its content. Do not use it for searching the web or when a URL is not yet known. It fetches the page and returns extracted text content."
				.to_string(),
			selection_hint: "Fetch and read content from a URL. Use when the user provides a URL, references a webpage, or you need to read online documentation.".to_string(),
			discoverable: true,
			tags: vec![
				"web".to_string(),
				"fetch".to_string(),
				"url".to_string(),
				"read".to_string(),
			],
			examples: vec!["Read the content at https://example.com/docs".to_string()],
			input_schema: vec!["url".to_string()],
			risk: ResourceRisk::Low,
			cost: ResourceCost {
				estimated_tokens: 0,
				estimated_latency_ms: 3_000,
			},
			required_capabilities: vec!["WebFetch".to_string()],
			summary: "Fetch a URL and return its text content.".to_string(),
			key_commands: Vec::new(),
			use_cases: Vec::new(),
			contract: Some(ToolContract {
				grounding: grounding_contract_simple(
					GroundingStrategy::UrlBased,
					&["url"],
					Some("url"),
					false,
					ExtractionHint::FetchUrl,
				),
				..ToolContract::default()
			}),
		},
	]
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
	runtime.register_tool(WebFetchTool {
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
			name: "WebSearch".to_string(),
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
			required_capabilities: vec!["WebSearch".to_string()],
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

		if let Some(endpoint) = self.config.endpoint.clone() {
			self.invoke_custom_backend(query, top_k, &endpoint)
		} else if let Some(api_key) = self.config.tavily_api_key.clone() {
			self.invoke_tavily(query, top_k, &api_key)
		} else {
			Ok(error_output(
				"search_not_configured",
				"web search is not configured. Set TAVILY_API_KEY to use the built-in Tavily \
				 provider, or set ROKU_WEB_SEARCH_URL to use a custom backend.",
				query,
				top_k,
				None,
			))
		}
	}
}

impl WebSearchTool {
	fn invoke_custom_backend(
		&self,
		query: &str,
		top_k: usize,
		endpoint: &str,
	) -> Result<Value, ToolFailure> {
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

	fn invoke_tavily(
		&self,
		query: &str,
		top_k: usize,
		api_key: &str,
	) -> Result<Value, ToolFailure> {
		const TAVILY_ENDPOINT: &str = "https://api.tavily.com/search";
		let client = Client::builder()
			.timeout(Duration::from_millis(8_000))
			.build()
			.map_err(|error| {
				ToolFailure::terminal(format!("failed to build tavily client: {error}"))
			})?;
		let body = json!({
			"query": query,
			"max_results": top_k,
			"api_key": api_key,
		});
		let response = match client.post(TAVILY_ENDPOINT).json(&body).send() {
			Ok(response) => response,
			Err(error) => {
				return Ok(error_output(
					"tavily_request_failed",
					format!("Tavily API request failed: {error}"),
					query,
					top_k,
					None,
				));
			}
		};
		let status = response.status();
		if !status.is_success() {
			let hint = if status.as_u16() == 401 {
				" (invalid or missing TAVILY_API_KEY)"
			} else {
				""
			};
			return Ok(error_output(
				"tavily_http_error",
				format!("Tavily API returned http {}{hint}", status.as_u16()),
				query,
				top_k,
				Some(json!({ "http_status": status.as_u16() })),
			));
		}
		let payload = match response.json::<Value>() {
			Ok(payload) => payload,
			Err(error) => {
				return Ok(error_output(
					"tavily_invalid_json",
					format!("Tavily API returned invalid json: {error}"),
					query,
					top_k,
					None,
				));
			}
		};
		// Tavily returns { results: [{ title, url, content, score }] }
		// parse_results already handles "content" as a snippet alias.
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
				"provider": "tavily",
			}),
		)
		.into_value())
	}
}

// ---------------------------------------------------------------------------
// WebFetch
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct WebFetchTool {
	config: WebToolRuntimeConfig,
}

impl Tool for WebFetchTool {
	fn descriptor(&self) -> ToolDescriptor {
		let runtime_constraints = RuntimeConstraints {
			timeout_ms: self.config.fetch_timeout_ms,
			max_retries: 0,
			retry_backoff_ms: 0,
			sandbox_profile: SandboxProfile::NoIsolation,
			deterministic_hooks: true,
			allowed_read_roots: Vec::new(),
			allowed_write_roots: Vec::new(),
		};
		let contract = ToolContract {
			grounding: grounding_contract_simple(
				GroundingStrategy::UrlBased,
				&["url"],
				Some("url"),
				false,
				ExtractionHint::FetchUrl,
			),
			..ToolContract::default()
		};
		ToolDescriptor {
			name: "WebFetch".to_string(),
			version: "1.0.0".to_string(),
			input_schema: contract_tool_schema(Some(&contract), &["url"]),
			output_schema: "tool_observation.v1".to_string(),
			required_capabilities: vec!["WebFetch".to_string()],
			runtime_constraints,
			contract: Some(contract),
		}
	}

	fn invoke(&self, request: ToolInvocationRequest) -> Result<Value, ToolFailure> {
		let url = request
			.input
			.get("url")
			.and_then(Value::as_str)
			.filter(|value| !value.trim().is_empty())
			.ok_or_else(|| ToolFailure::terminal("missing required field `url`"))?;

		let timeout_ms = self.config.fetch_timeout_ms;
		let max_bytes = self.config.max_fetch_bytes;

		let client = Client::builder()
			.timeout(Duration::from_millis(timeout_ms))
			.build()
			.map_err(|error| {
				ToolFailure::terminal(format!("failed to build fetch client: {error}"))
			})?;

		let response = match client.get(url).send() {
			Ok(response) => response,
			Err(error) => {
				return Ok(fetch_error_output(
					"connection_error",
					format!("failed to fetch url: {error}"),
					url,
				));
			}
		};

		let status = response.status();
		if !status.is_success() {
			let reason = status.canonical_reason().unwrap_or("Unknown");
			return Ok(fetch_error_output(
				"http_error",
				format!(
					"HTTP {} {reason}. The URL may be invalid, private, or temporarily unavailable.",
					status.as_u16()
				),
				url,
			));
		}

		let content_type = response
			.headers()
			.get("content-type")
			.and_then(|value| value.to_str().ok())
			.unwrap_or("unknown")
			.to_string();

		// Read bytes and cap at max_bytes to protect against oversized payloads.
		let bytes = response.bytes().map_err(|error| {
			ToolFailure::terminal(format!("failed to read response body: {error}"))
		})?;
		let truncated = bytes.len() > max_bytes;
		// Cap at max_bytes, then walk back to a valid UTF-8 char boundary
		// before converting, so we never produce replacement characters from
		// a truncation split.
		let cap = if truncated { max_bytes } else { bytes.len() };
		let mut boundary = cap;
		while boundary > 0 && std::str::from_utf8(&bytes[..boundary]).is_err() {
			boundary -= 1;
		}
		let raw = std::str::from_utf8(&bytes[..boundary]).unwrap_or("");
		let is_html = content_type.contains("html");
		let content = if is_html {
			extract_readable_text(raw)
		} else {
			raw.to_string()
		};
		let byte_length = content.len();

		Ok(ToolOutputEnvelope::new(
			true,
			Option::<String>::None,
			false,
			format!("Fetched {} ({byte_length} bytes).", url),
			json!({
				"url": url,
				"content": content,
				"content_type": content_type,
				"byte_length": byte_length,
				"truncated": truncated,
			}),
		)
		.into_value())
	}
}

fn fetch_error_output(error_type: &str, message: impl Into<String>, url: &str) -> Value {
	// Connection-level errors (DNS, refused) are terminal — truly unrecoverable.
	// HTTP-level errors (4xx, 5xx) are non-terminal — let the LLM try alternatives.
	let terminal = error_type != "http_error";
	ToolOutputEnvelope::new(
		false,
		Some(error_type),
		terminal,
		message,
		json!({ "url": url }),
	)
	.into_value()
}

// Patterns used by extract_readable_text — compiled once at program start.
static RE_SCRIPT: LazyLock<regex::Regex> = LazyLock::new(|| {
	regex::Regex::new(r"(?si)<script[^>]*>.*?</script>").expect("script regex should compile")
});
static RE_STYLE: LazyLock<regex::Regex> = LazyLock::new(|| {
	regex::Regex::new(r"(?si)<style[^>]*>.*?</style>").expect("style regex should compile")
});
static RE_NOSCRIPT: LazyLock<regex::Regex> = LazyLock::new(|| {
	regex::Regex::new(r"(?si)<noscript[^>]*>.*?</noscript>").expect("noscript regex should compile")
});
static RE_NAV: LazyLock<regex::Regex> = LazyLock::new(|| {
	regex::Regex::new(r"(?si)<nav[^>]*>.*?</nav>").expect("nav regex should compile")
});
static RE_HEADER: LazyLock<regex::Regex> = LazyLock::new(|| {
	regex::Regex::new(r"(?si)<header[^>]*>.*?</header>").expect("header regex should compile")
});
static RE_FOOTER: LazyLock<regex::Regex> = LazyLock::new(|| {
	regex::Regex::new(r"(?si)<footer[^>]*>.*?</footer>").expect("footer regex should compile")
});
static RE_ASIDE: LazyLock<regex::Regex> = LazyLock::new(|| {
	regex::Regex::new(r"(?si)<aside[^>]*>.*?</aside>").expect("aside regex should compile")
});
static RE_MAIN: LazyLock<regex::Regex> = LazyLock::new(|| {
	regex::Regex::new(r"(?si)<main[^>]*>(.*?)</main>").expect("main regex should compile")
});
static RE_ARTICLE: LazyLock<regex::Regex> = LazyLock::new(|| {
	regex::Regex::new(r"(?si)<article[^>]*>(.*?)</article>").expect("article regex should compile")
});
static RE_BODY: LazyLock<regex::Regex> = LazyLock::new(|| {
	regex::Regex::new(r"(?si)<body[^>]*>(.*?)</body>").expect("body regex should compile")
});
// Matches an opening <h1>–<h6> tag with optional attributes, the inner
// content (non-greedy), and any closing </hN> tag. The regex crate does not
// support backreferences, so we accept any </hN> closing tag and let Rust
// code extract the level from the captured opening tag name.
static RE_HEADING: LazyLock<regex::Regex> = LazyLock::new(|| {
	regex::Regex::new(r"(?is)<(h[1-6])[^>]*>(.*?)</h[1-6]>").expect("heading regex should compile")
});
static RE_BLOCK: LazyLock<regex::Regex> = LazyLock::new(|| {
	regex::Regex::new(r"(?i)</?(p|div|section)[^>]*>").expect("block element regex should compile")
});
static RE_LI: LazyLock<regex::Regex> =
	LazyLock::new(|| regex::Regex::new(r"(?i)<li[^>]*>").expect("li regex should compile"));
static RE_BR: LazyLock<regex::Regex> =
	LazyLock::new(|| regex::Regex::new(r"(?i)<br\s*/?>").expect("br regex should compile"));
static RE_LINK: LazyLock<regex::Regex> = LazyLock::new(|| {
	regex::Regex::new(r#"(?is)<a[^>]+href="([^"]*)"[^>]*>(.*?)</a>"#)
		.expect("anchor regex should compile")
});
static RE_PRE_CODE: LazyLock<regex::Regex> = LazyLock::new(|| {
	regex::Regex::new(r"(?si)<(pre|code)[^>]*>(.*?)</(pre|code)>")
		.expect("pre/code regex should compile")
});
static RE_REMAINING_TAGS: LazyLock<regex::Regex> =
	LazyLock::new(|| regex::Regex::new(r"<[^>]+>").expect("remaining tags regex should compile"));
static RE_NUMERIC_ENTITY: LazyLock<regex::Regex> =
	LazyLock::new(|| regex::Regex::new(r"&#(\d+);").expect("numeric entity regex should compile"));
static RE_HEX_ENTITY: LazyLock<regex::Regex> = LazyLock::new(|| {
	regex::Regex::new(r"(?i)&#x([0-9a-f]+);").expect("hex entity regex should compile")
});
static RE_MULTI_BLANK: LazyLock<regex::Regex> =
	LazyLock::new(|| regex::Regex::new(r"\n{3,}").expect("multi-blank regex should compile"));

/// Converts raw HTML into model-friendly plain text. Removes noise elements
/// (scripts, styles, nav, header, footer, aside), tries to scope down to the
/// main content area, converts structural tags to readable markers, preserves
/// links and code blocks, and collapses excess whitespace.
fn extract_readable_text(raw: &str) -> String {
	// Step 1: Remove script / style / noscript blocks entirely (including content).
	let s = RE_SCRIPT.replace_all(raw, "");
	let s = RE_STYLE.replace_all(&s, "");
	let s = RE_NOSCRIPT.replace_all(&s, "");

	// Step 2: Remove nav / header / footer / aside blocks and their content.
	let s = RE_NAV.replace_all(&s, "");
	let s = RE_HEADER.replace_all(&s, "");
	let s = RE_FOOTER.replace_all(&s, "");
	let s = RE_ASIDE.replace_all(&s, "");

	// Step 3: Extract content from <main>, <article>, or <body> in priority order.
	let scoped: std::borrow::Cow<str> = if let Some(cap) = RE_MAIN.captures(&s) {
		cap.get(1)
			.map(|m| m.as_str())
			.unwrap_or(&s)
			.to_owned()
			.into()
	} else if let Some(cap) = RE_ARTICLE.captures(&s) {
		cap.get(1)
			.map(|m| m.as_str())
			.unwrap_or(&s)
			.to_owned()
			.into()
	} else if let Some(cap) = RE_BODY.captures(&s) {
		cap.get(1)
			.map(|m| m.as_str())
			.unwrap_or(&s)
			.to_owned()
			.into()
	} else {
		s
	};

	// Step 4a: Fence <pre>/<code> blocks before further tag stripping so we
	// don't lose their whitespace-sensitive content.
	let s = RE_PRE_CODE.replace_all(&scoped, |caps: &regex::Captures| {
		let inner = caps.get(2).map(|m| m.as_str()).unwrap_or("");
		// Strip any tags inside the code block (nested <span> etc.) then fence.
		let inner_clean = RE_REMAINING_TAGS.replace_all(inner, "");
		format!("\n```\n{inner_clean}\n```\n")
	});

	// Step 4b: Convert headings h1-h6 to markdown-style prefix.
	let s = RE_HEADING.replace_all(&s, |caps: &regex::Captures| {
		let tag = caps.get(1).map(|m| m.as_str()).unwrap_or("h1");
		let level: usize = tag
			.trim_start_matches(['h', 'H'])
			.parse()
			.unwrap_or(1)
			.min(6);
		let prefix = "#".repeat(level);
		let inner = caps.get(2).map(|m| m.as_str()).unwrap_or("");
		// Strip tags inside heading text.
		let inner_clean = RE_REMAINING_TAGS.replace_all(inner, "");
		format!("\n\n{prefix} {inner_clean}\n\n")
	});

	// Step 4c: Convert <p>, <div>, <section> to paragraph breaks.
	let s = RE_BLOCK.replace_all(&s, "\n\n");

	// Step 4d: Convert <li> to list marker.
	let s = RE_LI.replace_all(&s, "\n- ");

	// Step 4e: Convert <br> to newline.
	let s = RE_BR.replace_all(&s, "\n");

	// Step 4f: Expand <a href="URL">text</a> to `text (URL)`.
	let s = RE_LINK.replace_all(&s, |caps: &regex::Captures| {
		let href = caps.get(1).map(|m| m.as_str()).unwrap_or("");
		let text_raw = caps.get(2).map(|m| m.as_str()).unwrap_or("");
		let text = RE_REMAINING_TAGS.replace_all(text_raw, "");
		let text = text.trim();
		if href.is_empty() || href == text {
			text.to_string()
		} else {
			format!("{text} ({href})")
		}
	});

	// Step 5: Strip all remaining HTML tags.
	let s = RE_REMAINING_TAGS.replace_all(&s, "");

	// Step 6: Decode HTML entities.
	let s = s
		.replace("&amp;", "&")
		.replace("&lt;", "<")
		.replace("&gt;", ">")
		.replace("&nbsp;", " ")
		.replace("&quot;", "\"")
		.replace("&apos;", "'");

	// Decode numeric entities &#NNN;
	let s = RE_NUMERIC_ENTITY.replace_all(&s, |caps: &regex::Captures| {
		let n: u32 = caps[1].parse().unwrap_or(0);
		char::from_u32(n).map(|c| c.to_string()).unwrap_or_default()
	});

	// Decode hex entities &#xHHH;
	let s = RE_HEX_ENTITY.replace_all(&s, |caps: &regex::Captures| {
		let n = u32::from_str_radix(&caps[1], 16).unwrap_or(0);
		char::from_u32(n).map(|c| c.to_string()).unwrap_or_default()
	});

	// Step 7: Collapse 3+ consecutive newlines to 2.
	let s = RE_MULTI_BLANK.replace_all(&s, "\n\n");

	// Step 8: Trim lines outside code blocks, preserve indentation inside.
	let mut result_lines = Vec::new();
	let mut in_code_block = false;
	for line in s.lines() {
		if line.trim_start().starts_with("```") {
			in_code_block = !in_code_block;
			result_lines.push(line.trim().to_string());
		} else if in_code_block {
			// Preserve indentation inside code blocks.
			result_lines.push(line.trim_end().to_string());
		} else {
			result_lines.push(line.trim().to_string());
		}
	}
	result_lines.join("\n").trim().to_string()
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
				"Commonly confused with a direct final_answer for knowledge questions that do not actually require fresh web results.",
				"Commonly confused with Find when the word `search` refers to workspace files rather than the public web.",
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
					"Optional result count capped by the configured WebSearch maximum.",
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
		grounding: grounding_contract(
			GroundingStrategy::PatternBased,
			&["query"],
			Some("query"),
			false,
			ExtractionHint::WebQuery,
			serde_json::Map::from_iter([("top_k".to_string(), json!(5))]),
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
	fn no_search_config_returns_actionable_error() {
		let tool = WebSearchTool {
			config: WebToolRuntimeConfig::default(),
		};
		let output = tool
			.invoke(invocation_request(json!({
				"query": "latest Rust edition",
			})))
			.expect("WebSearch should return a structured observation");
		let envelope = serde_json::from_value::<ToolOutputEnvelope>(output)
			.expect("WebSearch should emit ToolOutputEnvelope");

		assert!(!envelope.ok);
		assert_eq!(
			envelope.error_type.as_deref(),
			Some("search_not_configured")
		);
		assert!(envelope.terminal);
		// Message should mention how to configure search.
		assert!(
			envelope.message.contains("TAVILY_API_KEY")
				|| envelope.message.contains("ROKU_WEB_SEARCH_URL")
		);
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
				..WebToolRuntimeConfig::default()
			},
		};
		let output = tool
			.invoke(invocation_request(json!({
				"query": "latest Rust edition",
				"top_k": 3,
			})))
			.expect("WebSearch success should return a structured observation");
		let envelope = serde_json::from_value::<ToolOutputEnvelope>(output)
			.expect("WebSearch success should emit ToolOutputEnvelope");

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

	// -----------------------------------------------------------------------
	// Tavily provider tests
	// -----------------------------------------------------------------------

	#[test]
	fn tavily_response_parsed_to_search_results() {
		// Verify that the Tavily response shape (title, url, content) maps correctly
		// to the existing SearchResult format via parse_results().
		let tavily_payload = json!({
			"results": [
				{
					"title": "Rust async book",
					"url": "https://rust-lang.github.io/async-book/",
					"content": "Official async Rust book covering futures and executors.",
					"score": 0.95,
				},
				{
					"title": "Tokio docs",
					"url": "https://docs.rs/tokio",
					"content": "Tokio async runtime documentation.",
					"score": 0.88,
				},
			]
		});
		let results = parse_results(&tavily_payload, 5);
		assert_eq!(results.len(), 2);
		assert_eq!(results[0]["title"], "Rust async book");
		assert_eq!(results[0]["url"], "https://rust-lang.github.io/async-book/");
		// "content" should be returned as "snippet"
		assert_eq!(
			results[0]["snippet"],
			"Official async Rust book covering futures and executors."
		);
		assert_eq!(results[1]["title"], "Tokio docs");
	}

	#[test]
	fn tavily_top_k_limits_results() {
		let tavily_payload = json!({
			"results": [
				{ "title": "A", "url": "https://a.test/", "content": "a" },
				{ "title": "B", "url": "https://b.test/", "content": "b" },
				{ "title": "C", "url": "https://c.test/", "content": "c" },
			]
		});
		let results = parse_results(&tavily_payload, 2);
		assert_eq!(results.len(), 2);
	}

	#[test]
	fn custom_backend_takes_priority_over_tavily() {
		// When both endpoint and tavily_api_key are set, endpoint wins.
		let endpoint = spawn_mock_search_server(
			r#"{"results":[{"title":"Custom","url":"https://custom.test/","snippet":"from custom"}]}"#,
		);
		let tool = WebSearchTool {
			config: WebToolRuntimeConfig {
				endpoint: Some(endpoint),
				tavily_api_key: Some("fake-tavily-key".to_string()),
				default_top_k: 5,
				..WebToolRuntimeConfig::default()
			},
		};
		let output = tool
			.invoke(invocation_request(json!({ "query": "test query" })))
			.expect("WebSearch should return a structured observation");
		let envelope = serde_json::from_value::<ToolOutputEnvelope>(output)
			.expect("WebSearch should emit ToolOutputEnvelope");
		assert!(envelope.ok);
		// Response should carry "endpoint" key, not "provider": "tavily"
		assert!(envelope.data.get("endpoint").is_some());
		assert_eq!(
			envelope
				.data
				.get("results")
				.and_then(Value::as_array)
				.map(Vec::len),
			Some(1)
		);
	}

	// -----------------------------------------------------------------------
	// WebFetch tests
	// -----------------------------------------------------------------------

	fn spawn_mock_http_server(
		status: u16,
		content_type: &'static str,
		body: &'static str,
	) -> String {
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
			let reason = if status == 200 { "OK" } else { "Error" };
			let response = format!(
				"HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
				body.len()
			);
			let _ = stream.write_all(response.as_bytes());
		});
		format!("http://{address}/page")
	}

	fn fetch_tool() -> WebFetchTool {
		WebFetchTool {
			config: WebToolRuntimeConfig::default(),
		}
	}

	#[test]
	fn web_fetch_successful() {
		let url = spawn_mock_http_server(200, "text/plain", "Hello, World!");
		let output = fetch_tool()
			.invoke(invocation_request(json!({ "url": url })))
			.expect("WebFetch should succeed");
		let envelope = serde_json::from_value::<ToolOutputEnvelope>(output)
			.expect("WebFetch should emit ToolOutputEnvelope");
		assert!(envelope.ok);
		assert!(!envelope.terminal);
		assert_eq!(envelope.data["content"], "Hello, World!");
		assert_eq!(envelope.data["truncated"], false);
	}

	#[test]
	fn web_fetch_http_error() {
		let url = spawn_mock_http_server(404, "text/plain", "Not Found");
		let output = fetch_tool()
			.invoke(invocation_request(json!({ "url": url })))
			.expect("WebFetch should return structured error");
		let envelope = serde_json::from_value::<ToolOutputEnvelope>(output)
			.expect("WebFetch should emit ToolOutputEnvelope");
		assert!(!envelope.ok);
		assert_eq!(envelope.error_type.as_deref(), Some("http_error"));
		assert!(
			!envelope.terminal,
			"HTTP errors should be non-terminal so LLM can recover"
		);
	}

	#[test]
	fn web_fetch_strips_html_tags() {
		// Verifies the integration path: HTML content-type triggers extraction,
		// no raw tags leak into the output, and text + entities are preserved.
		let html = "<html><body><h1>Title</h1><p>Content &amp; more</p></body></html>";
		let url = spawn_mock_http_server(200, "text/html", html);
		let output = fetch_tool()
			.invoke(invocation_request(json!({ "url": url })))
			.expect("WebFetch should succeed");
		let envelope = serde_json::from_value::<ToolOutputEnvelope>(output)
			.expect("WebFetch should emit ToolOutputEnvelope");
		assert!(envelope.ok);
		let content = envelope.data["content"].as_str().unwrap();
		assert!(!content.contains('<'));
		assert!(content.contains("Title"));
		assert!(content.contains("Content & more"));
	}

	// -----------------------------------------------------------------------
	// extract_readable_text unit tests
	// -----------------------------------------------------------------------

	#[test]
	fn extract_readable_text_basic_html() {
		let html = "<html><body><h1>Title</h1><p>Content</p></body></html>";
		let result = extract_readable_text(html);
		assert!(result.contains("# Title"), "h1 should become # prefix");
		assert!(
			result.contains("Content"),
			"paragraph text should be present"
		);
		assert!(!result.contains('<'), "no raw tags should remain");
	}

	#[test]
	fn extract_readable_text_removes_script_and_style() {
		let html = "<html><body><script>alert('x');</script><style>.foo{color:red}</style><p>Visible</p></body></html>";
		let result = extract_readable_text(html);
		assert!(result.contains("Visible"), "visible text should survive");
		assert!(
			!result.contains("alert"),
			"script content should be removed"
		);
		assert!(
			!result.contains("color:red"),
			"style content should be removed"
		);
	}

	#[test]
	fn extract_readable_text_code_block_preserved() {
		let html = "<html><body><p>Example:</p><pre>fn main() {}</pre></body></html>";
		let result = extract_readable_text(html);
		assert!(result.contains("fn main()"), "code content should survive");
		assert!(result.contains("```"), "code should be wrapped in fences");
	}

	#[test]
	fn extract_readable_text_link_expansion() {
		let html = r#"<html><body><a href="https://example.com">click here</a></body></html>"#;
		let result = extract_readable_text(html);
		assert!(
			result.contains("click here (https://example.com)"),
			"link should be expanded to text (URL) format, got: {result}"
		);
	}

	#[test]
	fn extract_readable_text_entity_decoding() {
		let html = "<html><body><p>&amp; &lt; &gt; &nbsp; &quot; &#65; &#x42;</p></body></html>";
		let result = extract_readable_text(html);
		assert!(result.contains('&'), "& entity should decode");
		assert!(result.contains('<'), "< entity should decode");
		assert!(result.contains('>'), "> entity should decode");
		assert!(result.contains('"'), "\" entity should decode");
		assert!(result.contains('A'), "&#65; should decode to A");
		assert!(result.contains('B'), "&#x42; should decode to B");
	}

	#[test]
	fn extract_readable_text_no_excess_blank_lines() {
		let html = "<html><body><p>A</p><p>B</p><p>C</p><p>D</p></body></html>";
		let result = extract_readable_text(html);
		// Should not have 3+ consecutive newlines anywhere
		assert!(
			!result.contains("\n\n\n"),
			"should not have 3+ consecutive newlines, got: {result:?}"
		);
	}

	#[test]
	fn web_fetch_truncates_at_max_bytes() {
		// Use a body larger than max_fetch_bytes (default 102400).
		// We use a small custom config to test truncation without a huge body.
		let body: &'static str = Box::leak("A".repeat(200).into_boxed_str());
		let url = spawn_mock_http_server(200, "text/plain", body);
		let tool = WebFetchTool {
			config: WebToolRuntimeConfig {
				max_fetch_bytes: 50,
				..Default::default()
			},
		};
		let output = tool
			.invoke(invocation_request(json!({ "url": url })))
			.expect("WebFetch should succeed with truncation");
		let envelope = serde_json::from_value::<ToolOutputEnvelope>(output)
			.expect("WebFetch should emit ToolOutputEnvelope");
		assert!(envelope.ok);
		assert_eq!(envelope.data["truncated"], true);
		let content = envelope.data["content"].as_str().unwrap();
		assert!(content.len() <= 50);
	}

	#[test]
	fn web_fetch_truncation_respects_utf8_boundary() {
		// "你好" is 6 bytes in UTF-8 (3 bytes each). With max_bytes=4,
		// truncation must not split a character.
		let body = "你好世界";
		let url = spawn_mock_http_server(200, "text/plain", body);
		let tool = WebFetchTool {
			config: WebToolRuntimeConfig {
				max_fetch_bytes: 4,
				..Default::default()
			},
		};
		let output = tool
			.invoke(invocation_request(json!({ "url": url })))
			.expect("WebFetch should not panic on multi-byte boundary");
		let envelope = serde_json::from_value::<ToolOutputEnvelope>(output)
			.expect("WebFetch should emit ToolOutputEnvelope");
		assert!(envelope.ok);
		assert_eq!(envelope.data["truncated"], true);
		let content = envelope.data["content"].as_str().unwrap();
		// Should only contain "你" (3 bytes fits within boundary ≤ 4)
		assert_eq!(content, "你");
	}
}
