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

use std::sync::Arc;

use roku_common_types::{ToolContract, ToolRuntimeContract, ToolSideEffectPolicy};
use roku_plugin_host::{
	RuntimeConstraints, SandboxProfile, Tool, ToolDescriptor, ToolFailure, ToolInvocationRequest,
	ToolSchema,
};
use serde_json::Value;

use crate::descriptor_convert::mcp_tool_name;
use crate::transport::McpConnection;

/// A host `Tool` implementation that forwards invocations to an MCP server.
pub struct McpTool {
	connection: Arc<McpConnection>,
	tool_meta: rmcp::model::Tool,
	prefixed_name: String,
}

impl McpTool {
	/// Create a new wrapper around a single MCP tool from a live connection.
	pub fn new(connection: Arc<McpConnection>, tool_meta: rmcp::model::Tool) -> Self {
		let prefixed_name = mcp_tool_name(connection.server_name(), &tool_meta.name);
		Self {
			connection,
			tool_meta,
			prefixed_name,
		}
	}
}

impl Tool for McpTool {
	fn descriptor(&self) -> ToolDescriptor {
		let input_schema = extract_required_fields(&self.tool_meta);
		let contract = build_contract(&self.tool_meta);

		ToolDescriptor {
			name: self.prefixed_name.clone(),
			version: "1.0.0".to_string(),
			input_schema: ToolSchema {
				required_fields: input_schema,
			},
			output_schema: "json".to_string(),
			required_capabilities: vec![],
			runtime_constraints: RuntimeConstraints {
				timeout_ms: 30_000,
				max_retries: 1,
				retry_backoff_ms: 1000,
				sandbox_profile: SandboxProfile::NoIsolation,
				deterministic_hooks: false,
				allowed_read_roots: vec![],
				allowed_write_roots: vec![],
			},
			contract: Some(contract),
		}
	}

	fn invoke(&self, request: ToolInvocationRequest) -> Result<Value, ToolFailure> {
		let tool_name = self.tool_meta.name.to_string();
		let input = request.input.clone();

		let result = self
			.connection
			.call_tool_blocking(&tool_name, input)
			.map_err(|e| ToolFailure::terminal(e.to_string()))?;

		if result.is_error == Some(true) {
			let msg = extract_text_content(&result.content);
			return Err(ToolFailure::terminal(msg));
		}

		Ok(call_result_to_value(result))
	}
}

fn extract_required_fields(tool: &rmcp::model::Tool) -> Vec<String> {
	let schema = tool.input_schema.as_ref();
	let Some(required) = schema.get("required").and_then(|v| v.as_array()) else {
		return vec![];
	};
	required
		.iter()
		.filter_map(|v| v.as_str().map(String::from))
		.collect()
}

fn build_contract(tool: &rmcp::model::Tool) -> ToolContract {
	let side_effects = match tool.annotations.as_ref() {
		Some(a) if a.read_only_hint == Some(true) => ToolSideEffectPolicy::ReadOnly,
		Some(a) if a.destructive_hint == Some(true) => ToolSideEffectPolicy::ExternalMutation,
		_ => ToolSideEffectPolicy::ExternalMutation,
	};

	ToolContract {
		runtime: ToolRuntimeContract {
			side_effects,
			..Default::default()
		},
		..Default::default()
	}
}

fn call_result_to_value(result: rmcp::model::CallToolResult) -> Value {
	if let Some(structured) = result.structured_content {
		return structured;
	}
	Value::String(extract_text_content(&result.content))
}

fn extract_text_content(content: &[rmcp::model::Content]) -> String {
	content
		.iter()
		.filter_map(|c| c.as_text().map(|t| t.text.as_str()))
		.collect::<Vec<_>>()
		.join("\n")
}
