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

use std::fs;
use std::path::Path;

use roku_common_types::ToolContract;
use roku_common_types::{ResourceCost, ResourceRisk};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuiltinToolRole {
	SkillInstall,
	SkillExecute,
	Inventory,
	Research,
	Data,
	Review,
	General,
}

impl BuiltinToolRole {
	pub fn as_str(self) -> &'static str {
		match self {
			Self::SkillInstall => "skill_install",
			Self::SkillExecute => "skill_execute",
			Self::Inventory => "inventory",
			Self::Research => "research",
			Self::Data => "data",
			Self::Review => "review",
			Self::General => "general",
		}
	}
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfiguredTool {
	pub name: String,
	pub role: BuiltinToolRole,
	#[serde(default = "default_discoverable")]
	pub discoverable: bool,
	#[serde(default)]
	pub terminal_output: bool,
	pub description: String,
	#[serde(default)]
	pub selection_hint: String,
	#[serde(default)]
	pub tags: Vec<String>,
	#[serde(default)]
	pub examples: Vec<String>,
	#[serde(default)]
	pub input_schema: Vec<String>,
	#[serde(default)]
	pub risk: ResourceRisk,
	#[serde(default)]
	pub cost: ResourceCost,
	#[serde(default)]
	pub required_capabilities: Vec<String>,
	#[serde(default)]
	pub contract: Option<ToolContract>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCatalogConfig {
	pub tools: Vec<ConfiguredTool>,
}

#[derive(Debug, Error)]
pub enum ToolCatalogConfigError {
	#[error("failed to read tool catalog config: {0}")]
	Io(#[from] std::io::Error),
	#[error("failed to parse tool catalog config: {0}")]
	Parse(#[from] toml::de::Error),
	#[error("tool catalog config must define at least one tool")]
	Empty,
}

impl ToolCatalogConfig {
	pub fn from_path(path: &Path) -> Result<Self, ToolCatalogConfigError> {
		let content = fs::read_to_string(path)?;
		Self::from_toml(&content)
	}

	pub fn from_toml(content: &str) -> Result<Self, ToolCatalogConfigError> {
		let config = toml::from_str::<Self>(content)?;
		if config.tools.is_empty() {
			return Err(ToolCatalogConfigError::Empty);
		}
		Ok(config)
	}

	pub fn tool_for_role(&self, role: BuiltinToolRole) -> Option<&ConfiguredTool> {
		self.tools.iter().find(|tool| tool.role == role)
	}
}

impl Default for ToolCatalogConfig {
	fn default() -> Self {
		Self::from_toml(include_str!("../../../../config/tools.toml"))
			.expect("embedded tool catalog config should be valid")
	}
}

fn default_discoverable() -> bool {
	true
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn default_config_exposes_expected_roles() {
		let config = ToolCatalogConfig::default();
		assert!(config.tool_for_role(BuiltinToolRole::Inventory).is_some());
		assert!(config.tool_for_role(BuiltinToolRole::Research).is_some());
		assert!(config.tool_for_role(BuiltinToolRole::Data).is_some());
		assert!(config.tool_for_role(BuiltinToolRole::Review).is_some());
		assert!(
			config
				.tool_for_role(BuiltinToolRole::SkillInstall)
				.is_some()
		);
		assert!(
			config
				.tool_for_role(BuiltinToolRole::SkillExecute)
				.is_some()
		);
		assert!(
			config
				.tool_for_role(BuiltinToolRole::Inventory)
				.is_some_and(|tool| tool.terminal_output)
		);
		// general.execute removed from catalog — General role is no longer present
		assert!(config.tool_for_role(BuiltinToolRole::General).is_none());
	}

	#[test]
	fn tool_catalog_rejects_unknown_fields() {
		let error = ToolCatalogConfig::from_toml(
			r#"
[[tools]]
name = "inventory.describe"
role = "inventory"
description = "Describe all available tools, skills, and capability families."
selection_hint = "List or explain what tools and skills are available."
extra = "not-allowed"
"#,
		)
		.expect_err("unknown fields should fail");

		assert!(matches!(error, ToolCatalogConfigError::Parse(_)));
		assert!(error.to_string().contains("unknown field `extra`"));
	}

	#[test]
	fn tool_catalog_allows_missing_selection_hint_for_compatibility() {
		let config = ToolCatalogConfig::from_toml(
			r#"
[[tools]]
name = "inventory.describe"
role = "inventory"
description = "Describe all available tools, skills, and capability families."
"#,
		)
		.expect("missing selection_hint should fall back to description");

		assert_eq!(config.tools.len(), 1);
		assert!(config.tools[0].selection_hint.is_empty());
	}
}
