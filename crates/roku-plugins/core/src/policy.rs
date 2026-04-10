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

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{PluginCoreError, PluginId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PluginProfile {
	#[default]
	Minimal,
	Messaging,
	Full,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PluginEntryPolicy {
	pub enabled: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginPolicyConfig {
	#[serde(default)]
	pub paths: Vec<PathBuf>,
	#[serde(default)]
	pub profile: PluginProfile,
	#[serde(default)]
	pub allow: Vec<PluginId>,
	#[serde(default)]
	pub deny: Vec<PluginId>,
	#[serde(default)]
	pub entries: BTreeMap<String, PluginEntryPolicy>,
}

impl Default for PluginPolicyConfig {
	fn default() -> Self {
		Self {
			paths: Vec::new(),
			profile: PluginProfile::Minimal,
			allow: Vec::new(),
			deny: Vec::new(),
			entries: BTreeMap::new(),
		}
	}
}

impl PluginPolicyConfig {
	pub fn from_toml(content: &str) -> Result<Self, PluginCoreError> {
		toml::from_str(content).map_err(|error| PluginCoreError::PolicyParse(error.to_string()))
	}

	pub fn entry_enabled(&self, plugin_id: &PluginId) -> Option<bool> {
		self.entries
			.get(plugin_id.as_str())
			.and_then(|entry| entry.enabled)
	}

	pub fn allows(&self, plugin_id: &PluginId) -> bool {
		self.allow.iter().any(|candidate| candidate == plugin_id)
	}

	pub fn denies(&self, plugin_id: &PluginId) -> bool {
		self.deny.iter().any(|candidate| candidate == plugin_id)
	}
}

#[cfg(test)]
mod tests {
	use super::{PluginPolicyConfig, PluginProfile};

	#[test]
	fn parse_default_policy_file_shape() {
		let config = PluginPolicyConfig::from_toml(
			r#"
profile = "minimal"
allow = ["telegram"]
deny = ["mcp"]

[entries.openrouter]
enabled = true
"#,
		)
		.expect("policy config should parse");

		assert_eq!(config.profile, PluginProfile::Minimal);
		assert_eq!(config.allow.len(), 1);
		assert_eq!(config.deny.len(), 1);
		assert_eq!(config.entries["openrouter"].enabled, Some(true));
	}
}
