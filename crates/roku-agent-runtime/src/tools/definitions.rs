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

//! Catalog-based tool definition builder.
//!
//! Builds `ToolEntry` implementations from the `ResourceCatalog` at startup.
//! Each catalog entry with `ResourceKind::Tool` becomes a `CatalogToolEntry`
//! that the registry can query for definitions, visibility, and risk.

use roku_common_types::ResourceSelector;
use roku_plugin_llm::ToolDefinition;
use roku_plugin_tools::{ResourceCatalog, ResourceKind, TAG_RISK_SAFE};
use serde_json::json;

use super::trait_def::ToolEntry;

/// A tool entry built from a `ResourceCatalog` descriptor.
pub(crate) struct CatalogToolEntry {
	name: String,
	description: String,
	read_only: bool,
	parameters: serde_json::Value,
}

impl CatalogToolEntry {
	fn from_catalog_entry(catalog: &ResourceCatalog, tool_name: &str) -> Option<Self> {
		let selector = ResourceSelector::tool(tool_name);
		let descriptor = catalog.descriptor(&selector)?;

		let read_only = descriptor.tags.iter().any(|t| t == TAG_RISK_SAFE);

		let parameters = if descriptor.input_schema.is_empty() {
			json!({"type": "object", "properties": {}})
		} else {
			let mut properties = serde_json::Map::new();
			for key in &descriptor.input_schema {
				properties.insert(key.clone(), json!({"type": "string"}));
			}
			json!({
				"type": "object",
				"properties": properties,
				"required": descriptor.input_schema.iter()
					.filter(|key| *key != "cwd" && *key != "timeout_ms")
					.collect::<Vec<_>>(),
			})
		};

		Some(Self {
			name: tool_name.to_string(),
			description: descriptor.selection_hint.clone(),
			read_only,
			parameters,
		})
	}
}

impl ToolEntry for CatalogToolEntry {
	fn name(&self) -> &str {
		&self.name
	}

	fn description(&self) -> &str {
		&self.description
	}

	fn is_read_only(&self) -> bool {
		self.read_only
	}

	fn tool_definition(&self) -> ToolDefinition {
		ToolDefinition {
			name: self.name.clone(),
			description: self.description.clone(),
			parameters: self.parameters.clone(),
		}
	}
}

/// Build `CatalogToolEntry` instances for all tools in the catalog and register
/// them into the provided registry.
pub(crate) fn register_catalog_tools(
	registry: &mut super::registry::ToolRegistry,
	catalog: &ResourceCatalog,
) {
	for entry in catalog.entries() {
		if entry.kind == ResourceKind::Tool
			&& let Some(tool_entry) = CatalogToolEntry::from_catalog_entry(catalog, &entry.name)
		{
			registry.register(Box::new(tool_entry));
		}
	}
}
