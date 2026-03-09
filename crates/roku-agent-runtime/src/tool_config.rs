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

use roku_resource_catalog::{ResourceCost, ResourceRisk};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuiltinToolRole {
	SkillInstall,
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
			Self::Inventory => "inventory",
			Self::Research => "research",
			Self::Data => "data",
			Self::Review => "review",
			Self::General => "general",
		}
	}
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfiguredTool {
	pub name: String,
	pub role: BuiltinToolRole,
	#[serde(default = "default_discoverable")]
	pub discoverable: bool,
	pub description: String,
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
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCatalogConfig {
	pub tools: Vec<ConfiguredTool>,
}

#[derive(Debug, Error)]
pub enum ToolCatalogConfigError {
	#[error("failed to read tool catalog config: {0}")]
	Io(#[from] std::io::Error),
	#[error("failed to parse tool catalog config: {0}")]
	Parse(#[from] serde_json::Error),
	#[error("tool catalog config must define at least one tool")]
	Empty,
}

impl ToolCatalogConfig {
	pub fn from_path(path: &Path) -> Result<Self, ToolCatalogConfigError> {
		let content = fs::read_to_string(path)?;
		Self::from_json(&content)
	}

	pub fn from_json(content: &str) -> Result<Self, ToolCatalogConfigError> {
		let config = serde_json::from_str::<Self>(content)?;
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
		Self::from_json(include_str!("../../../config/tools.json"))
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
	}
}
