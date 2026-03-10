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

use crate::{PluginId, PluginManifest, PluginSource};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginStatus {
	Enabled,
	Disabled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum PluginDisableReason {
	ExplicitlyDisabled,
	DeniedByPolicy,
	NotEnabledByProfile,
	DisabledByDefault,
	MissingRequiredEnv { variables: Vec<String> },
	MissingAnyRequiredEnv { groups: Vec<Vec<String>> },
	MissingExternalBinaries { binaries: Vec<String> },
	AdmissionRejected { detail: String },
	ShadowedByHigherPrecedence { winner: String },
	ReservedBundledPluginId,
	UnsupportedExternalPlugin,
	NonBundledSideEffectRequiresAllow,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginRegistryEntry {
	pub manifest: PluginManifest,
	pub source: PluginSource,
	pub status: PluginStatus,
	pub disable_reason: Option<PluginDisableReason>,
	pub warnings: Vec<String>,
	pub required: bool,
	pub implementation_available: bool,
	pub selected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginRegistrySnapshot {
	entries: Vec<PluginRegistryEntry>,
	reserved_bundled_ids: Vec<PluginId>,
	permissive: bool,
}

impl PluginRegistrySnapshot {
	pub fn new(entries: Vec<PluginRegistryEntry>, reserved_bundled_ids: Vec<PluginId>) -> Self {
		Self {
			entries,
			reserved_bundled_ids,
			permissive: false,
		}
	}

	pub fn permissive() -> Self {
		Self {
			entries: Vec::new(),
			reserved_bundled_ids: Vec::new(),
			permissive: true,
		}
	}

	pub fn entries(&self) -> &[PluginRegistryEntry] {
		&self.entries
	}

	pub fn reserved_bundled_ids(&self) -> &[PluginId] {
		&self.reserved_bundled_ids
	}

	pub fn is_permissive(&self) -> bool {
		self.permissive
	}

	pub fn enabled_entries(&self) -> impl Iterator<Item = &PluginRegistryEntry> {
		self.entries
			.iter()
			.filter(|entry| entry.selected && entry.status == PluginStatus::Enabled)
	}

	pub fn entry(&self, plugin_id: &str) -> Option<&PluginRegistryEntry> {
		self.entries
			.iter()
			.find(|entry| entry.selected && entry.manifest.id.as_str() == plugin_id)
	}

	pub fn is_plugin_enabled(&self, plugin_id: &str) -> bool {
		match self.entry(plugin_id) {
			Some(entry) => entry.status == PluginStatus::Enabled,
			None => self.permissive,
		}
	}

	pub fn with_runtime_disable(&self, plugin_id: &str, reason: PluginDisableReason) -> Self {
		let mut snapshot = self.clone();
		if let Some(entry) = snapshot
			.entries
			.iter_mut()
			.find(|entry| entry.selected && entry.manifest.id.as_str() == plugin_id)
		{
			entry.status = PluginStatus::Disabled;
			entry.disable_reason = Some(reason);
		}
		snapshot
	}

	pub fn enabled_ids(&self) -> Vec<String> {
		self.enabled_entries()
			.map(|entry| entry.manifest.id.to_string())
			.collect()
	}

	pub fn disabled_summaries(&self) -> Vec<(String, String)> {
		self.entries
			.iter()
			.filter(|entry| entry.selected && entry.status == PluginStatus::Disabled)
			.map(|entry| {
				let reason = entry
					.disable_reason
					.as_ref()
					.map(|reason| format!("{reason:?}"))
					.unwrap_or_else(|| "disabled".to_string());
				(entry.manifest.id.to_string(), reason)
			})
			.collect()
	}
}

impl Default for PluginRegistrySnapshot {
	fn default() -> Self {
		Self::permissive()
	}
}
