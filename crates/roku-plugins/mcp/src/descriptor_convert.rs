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

use roku_common_types::{
	ResourceSelector, ToolContract, ToolRuntimeContract, ToolSideEffectPolicy,
};
use roku_plugin_catalog::{CatalogDescriptor, ResourceCost, ResourceKind, ResourceRisk};

/// Replace any character that is not ASCII alphanumeric or `_` with `_`.
pub fn normalize_server_name(name: &str) -> String {
	name.chars()
		.map(|c| {
			if c.is_ascii_alphanumeric() || c == '_' {
				c
			} else {
				'_'
			}
		})
		.collect()
}

/// Build the canonical prefixed tool name: `mcp__{normalized_server}__{tool_name}`.
pub fn mcp_tool_name(server_name: &str, tool_name: &str) -> String {
	format!("mcp__{}__{}", normalize_server_name(server_name), tool_name)
}

/// Convert a slice of rmcp `Tool` values to `CatalogDescriptor` entries.
pub fn mcp_tools_to_catalog_descriptors(
	server_name: &str,
	tools: &[rmcp::model::Tool],
) -> Vec<CatalogDescriptor> {
	tools
		.iter()
		.map(|tool| tool_to_catalog_descriptor(server_name, tool))
		.collect()
}

fn side_effects_from_annotations(tool: &rmcp::model::Tool) -> ToolSideEffectPolicy {
	match tool.annotations.as_ref() {
		Some(a) if a.read_only_hint == Some(true) => ToolSideEffectPolicy::ReadOnly,
		Some(a) if a.destructive_hint == Some(true) => ToolSideEffectPolicy::ExternalMutation,
		_ => ToolSideEffectPolicy::ExternalMutation,
	}
}

fn tool_to_catalog_descriptor(server_name: &str, tool: &rmcp::model::Tool) -> CatalogDescriptor {
	let prefixed_name = mcp_tool_name(server_name, &tool.name);
	let description = tool.description.as_deref().unwrap_or("").to_string();

	let input_schema = extract_required_fields(tool);
	let side_effects = side_effects_from_annotations(tool);

	let contract = ToolContract {
		runtime: ToolRuntimeContract {
			side_effects,
			..Default::default()
		},
		..Default::default()
	};

	CatalogDescriptor {
		selector: ResourceSelector::tool(&prefixed_name),
		kind: ResourceKind::Tool,
		name: prefixed_name.clone(),
		role: None,
		description: description.clone(),
		selection_hint: description,
		discoverable: true,
		tags: vec!["mcp".to_string(), server_name.to_string()],
		examples: vec![],
		input_schema,
		risk: ResourceRisk::Medium,
		cost: ResourceCost {
			estimated_tokens: 500,
			estimated_latency_ms: 5000,
		},
		required_capabilities: vec![],
		summary: format!("MCP tool from server '{}'", server_name),
		key_commands: vec![],
		use_cases: vec![],
		contract: Some(contract),
	}
}

/// Extract required field names from the tool's JSON Schema `input_schema`.
///
/// Reads the `"required"` array (per JSON Schema spec), not the
/// `"properties"` object, so only mandatory fields are returned.
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

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn normalize_server_name_replaces_non_alnum() {
		assert_eq!(normalize_server_name("my-server"), "my_server");
		assert_eq!(normalize_server_name("my server"), "my_server");
		assert_eq!(normalize_server_name("my.server"), "my_server");
		assert_eq!(normalize_server_name("my_server"), "my_server");
		assert_eq!(normalize_server_name("ABC123"), "ABC123");
	}

	#[test]
	fn mcp_tool_name_format() {
		assert_eq!(
			mcp_tool_name("my-server", "do_thing"),
			"mcp__my_server__do_thing"
		);
		assert_eq!(mcp_tool_name("plain", "list"), "mcp__plain__list");
	}
}
