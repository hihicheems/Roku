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

use serde::{Deserialize, Serialize};

use super::{PluginId, PluginKind};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginManifest {
	pub id: PluginId,
	pub kind: PluginKind,
	#[serde(default)]
	pub enabled_by_default: bool,
	#[serde(default)]
	pub capabilities: PluginCapabilities,
	#[serde(default)]
	pub requirements: PluginRequirements,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PluginCapabilities {
	#[serde(default)]
	pub provides_tools: Vec<String>,
	#[serde(default)]
	pub provides_connectors: Vec<String>,
	#[serde(default)]
	pub provides_providers: Vec<String>,
	#[serde(default)]
	pub provides_skill_sources: Vec<String>,
	#[serde(default)]
	pub has_side_effects: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PluginRequirements {
	#[serde(default)]
	pub env: Vec<String>,
	#[serde(default)]
	pub env_any: Vec<Vec<String>>,
	#[serde(default)]
	pub external_bins: Vec<String>,
}
