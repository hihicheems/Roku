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
use std::path::{Path, PathBuf};

use crate::{
	PluginCapabilities, PluginDisableReason, PluginId, PluginManifest, PluginPolicyConfig,
	PluginProfile, PluginRegistryEntry, PluginRegistrySnapshot, PluginRequirements, PluginStatus,
};
use roku_common_types::{LogLevel, LogRecord, emit_global_log};
use thiserror::Error;

use crate::admission::admission_outcome;
use crate::discovery::{DiscoveredPluginCandidate, discover_plugin_candidates};
use crate::registry_loader::load_registry_snapshot;

#[derive(Debug, Error)]
pub enum PluginHostError {
	#[error("failed to read plugin policy config: {0}")]
	PolicyIo(#[from] std::io::Error),
	#[error(transparent)]
	PluginCore(#[from] crate::PluginCoreError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundledPluginDescriptor {
	pub manifest: PluginManifest,
	pub required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginDiscoveryConfig {
	pub explicit_paths: Vec<PathBuf>,
	pub env_root: Option<PathBuf>,
	pub workspace_root: PathBuf,
	pub user_root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginStartupConfig {
	pub discovery: PluginDiscoveryConfig,
	pub policy: PluginPolicyConfig,
	pub bundled_descriptors: Vec<BundledPluginDescriptor>,
}

pub fn build_plugin_registry_snapshot(
	config: &PluginStartupConfig,
) -> Result<PluginRegistrySnapshot, PluginHostError> {
	let discovered = discover_plugin_candidates(&config.discovery, &config.bundled_descriptors)?;
	let snapshot = load_registry_snapshot(discovered, &config.policy);
	log_inventory_summary(&snapshot);
	Ok(snapshot)
}

pub fn default_bundled_plugin_descriptors(tool_names: &[String]) -> Vec<BundledPluginDescriptor> {
	vec![
		BundledPluginDescriptor {
			manifest: PluginManifest {
				id: PluginId::new("builtin-tools").expect("bundled plugin id should be valid"),
				kind: crate::PluginKind::Toolset,
				enabled_by_default: true,
				capabilities: PluginCapabilities {
					provides_tools: tool_names.to_vec(),
					has_side_effects: true,
					..PluginCapabilities::default()
				},
				requirements: PluginRequirements::default(),
			},
			required: true,
		},
		BundledPluginDescriptor {
			manifest: PluginManifest {
				id: PluginId::new("skill-source-local").expect("bundled plugin id should be valid"),
				kind: crate::PluginKind::SkillSource,
				enabled_by_default: true,
				capabilities: PluginCapabilities {
					provides_skill_sources: vec!["local".to_string()],
					..PluginCapabilities::default()
				},
				requirements: PluginRequirements::default(),
			},
			required: true,
		},
		BundledPluginDescriptor {
			manifest: PluginManifest {
				id: PluginId::new("core-fs").expect("bundled plugin id should be valid"),
				kind: crate::PluginKind::Toolset,
				enabled_by_default: true,
				capabilities: PluginCapabilities {
					provides_tools: vec![
						"Inspect".to_string(),
						"ListDir".to_string(),
						"Read".to_string(),
						"Glob".to_string(),
						"Exists".to_string(),
					],
					..PluginCapabilities::default()
				},
				requirements: PluginRequirements::default(),
			},
			required: false,
		},
		BundledPluginDescriptor {
			manifest: PluginManifest {
				id: PluginId::new("core-command").expect("bundled plugin id should be valid"),
				kind: crate::PluginKind::Toolset,
				enabled_by_default: true,
				capabilities: PluginCapabilities {
					provides_tools: vec!["Bash".to_string()],
					has_side_effects: true,
					..PluginCapabilities::default()
				},
				requirements: PluginRequirements::default(),
			},
			required: false,
		},
		BundledPluginDescriptor {
			manifest: PluginManifest {
				id: PluginId::new("core-table").expect("bundled plugin id should be valid"),
				kind: crate::PluginKind::Toolset,
				enabled_by_default: true,
				capabilities: PluginCapabilities {
					provides_tools: vec![
						"TableInspect".to_string(),
						"TableSheets".to_string(),
						"TablePreview".to_string(),
						"TableSchema".to_string(),
					],
					..PluginCapabilities::default()
				},
				requirements: PluginRequirements::default(),
			},
			required: false,
		},
		BundledPluginDescriptor {
			manifest: PluginManifest {
				id: PluginId::new("core-web").expect("bundled plugin id should be valid"),
				kind: crate::PluginKind::Toolset,
				enabled_by_default: true,
				capabilities: PluginCapabilities {
					provides_tools: vec!["WebSearch".to_string()],
					..PluginCapabilities::default()
				},
				// No env requirements: WebFetch always works; WebSearch handles
				// missing search config at invoke time with an actionable error.
				requirements: PluginRequirements::default(),
			},
			required: false,
		},
		BundledPluginDescriptor {
			manifest: PluginManifest {
				id: PluginId::new("core-python").expect("bundled plugin id should be valid"),
				kind: crate::PluginKind::Toolset,
				enabled_by_default: true,
				capabilities: PluginCapabilities {
					provides_tools: vec!["Python".to_string()],
					has_side_effects: true,
					..PluginCapabilities::default()
				},
				requirements: PluginRequirements {
					external_bins: vec!["python3".to_string()],
					..PluginRequirements::default()
				},
			},
			required: false,
		},
		BundledPluginDescriptor {
			manifest: PluginManifest {
				id: PluginId::new("openrouter").expect("bundled plugin id should be valid"),
				kind: crate::PluginKind::Provider,
				enabled_by_default: true,
				capabilities: PluginCapabilities {
					provides_providers: vec!["llm".to_string()],
					..PluginCapabilities::default()
				},
				requirements: PluginRequirements {
					env: vec!["OPENROUTER_API_KEY".to_string()],
					..PluginRequirements::default()
				},
			},
			required: false,
		},
		BundledPluginDescriptor {
			manifest: PluginManifest {
				id: PluginId::new("telegram").expect("bundled plugin id should be valid"),
				kind: crate::PluginKind::Connector,
				enabled_by_default: false,
				capabilities: PluginCapabilities {
					provides_connectors: vec!["telegram".to_string()],
					..PluginCapabilities::default()
				},
				requirements: PluginRequirements {
					env_any: vec![vec![
						"TELEGRAM_BOT_TOKEN".to_string(),
						"TELOXIDE_TOKEN".to_string(),
					]],
					..PluginRequirements::default()
				},
			},
			required: false,
		},
		BundledPluginDescriptor {
			manifest: PluginManifest {
				id: PluginId::new("mcp").expect("bundled plugin id should be valid"),
				kind: crate::PluginKind::Bridge,
				enabled_by_default: false,
				capabilities: PluginCapabilities {
					provides_connectors: vec!["mcp".to_string()],
					..PluginCapabilities::default()
				},
				requirements: PluginRequirements::default(),
			},
			required: false,
		},
	]
}

pub(crate) fn profile_decision(profile: PluginProfile, plugin_id: &PluginId) -> Option<bool> {
	let enabled = match profile {
		PluginProfile::Minimal => matches!(
			plugin_id.as_str(),
			"builtin-tools"
				| "skill-source-local"
				| "openrouter"
				| "core-fs" | "core-command"
				| "core-table"
				| "core-web" | "core-python"
		),
		PluginProfile::Messaging => matches!(
			plugin_id.as_str(),
			"builtin-tools"
				| "skill-source-local"
				| "openrouter"
				| "core-fs" | "core-command"
				| "core-table"
				| "core-web" | "core-python"
				| "telegram"
		),
		PluginProfile::Full => matches!(
			plugin_id.as_str(),
			"builtin-tools"
				| "skill-source-local"
				| "openrouter"
				| "core-fs" | "core-command"
				| "core-table"
				| "core-web" | "core-python"
				| "telegram" | "mcp"
		),
	};

	if matches!(
		plugin_id.as_str(),
		"builtin-tools"
			| "skill-source-local"
			| "openrouter"
			| "core-fs"
			| "core-table"
			| "core-web"
			| "core-python"
			| "telegram"
			| "mcp"
	) {
		Some(enabled)
	} else {
		None
	}
}

pub(crate) fn policy_disable_reason(
	candidate: &DiscoveredPluginCandidate,
	policy: &PluginPolicyConfig,
) -> Option<PluginDisableReason> {
	let plugin_id = &candidate.manifest.id;
	match policy.entry_enabled(plugin_id) {
		Some(false) if candidate.required => {
			log_policy_warning(
				plugin_id.as_str(),
				"required bundled plugin cannot be disabled by entry override; ignoring override",
			);
		}
		Some(false) => return Some(PluginDisableReason::ExplicitlyDisabled),
		Some(true) => return None,
		None => {}
	}

	if candidate.required {
		return None;
	}
	if policy.denies(plugin_id) {
		return Some(PluginDisableReason::DeniedByPolicy);
	}
	if policy.allows(plugin_id) {
		return None;
	}
	if let Some(enabled) = profile_decision(policy.profile, plugin_id) {
		return if enabled {
			None
		} else {
			Some(PluginDisableReason::NotEnabledByProfile)
		};
	}
	if candidate.manifest.enabled_by_default {
		None
	} else {
		Some(PluginDisableReason::DisabledByDefault)
	}
}

pub(crate) fn requirements_disable_reason(
	candidate: &DiscoveredPluginCandidate,
) -> Option<PluginDisableReason> {
	let missing_env = candidate
		.manifest
		.requirements
		.env
		.iter()
		.filter(|key| {
			env::var(key.as_str())
				.ok()
				.filter(|value| !value.trim().is_empty())
				.is_none()
		})
		.cloned()
		.collect::<Vec<_>>();
	if !missing_env.is_empty() {
		return Some(PluginDisableReason::MissingRequiredEnv {
			variables: missing_env,
		});
	}

	let missing_any = candidate
		.manifest
		.requirements
		.env_any
		.iter()
		.filter(|group| {
			!group.iter().any(|key| {
				env::var(key.as_str())
					.ok()
					.filter(|value| !value.trim().is_empty())
					.is_some()
			})
		})
		.cloned()
		.collect::<Vec<_>>();
	if !missing_any.is_empty() {
		return Some(PluginDisableReason::MissingAnyRequiredEnv {
			groups: missing_any,
		});
	}

	let missing_bins = candidate
		.manifest
		.requirements
		.external_bins
		.iter()
		.filter(|binary| !binary_in_path(binary))
		.cloned()
		.collect::<Vec<_>>();
	if !missing_bins.is_empty() {
		return Some(PluginDisableReason::MissingExternalBinaries {
			binaries: missing_bins,
		});
	}

	None
}

pub(crate) fn admission_disable_reason(
	candidate: &DiscoveredPluginCandidate,
	policy: &PluginPolicyConfig,
) -> (Option<PluginDisableReason>, Vec<String>) {
	let outcome = admission_outcome(candidate, policy);
	(outcome.rejection, outcome.warnings)
}

fn binary_in_path(binary: &str) -> bool {
	let Some(path) = env::var_os("PATH") else {
		return false;
	};
	env::split_paths(&path).any(|directory| binary_exists_in_dir(&directory, binary))
}

fn binary_exists_in_dir(directory: &Path, binary: &str) -> bool {
	let candidate = directory.join(binary);
	if candidate.is_file() {
		return true;
	}
	#[cfg(windows)]
	{
		for extension in ["exe", "cmd", "bat"] {
			if directory.join(format!("{binary}.{extension}")).is_file() {
				return true;
			}
		}
	}
	false
}

fn log_policy_warning(plugin_id: &str, message: &str) {
	let _ = emit_global_log(
		LogRecord::new("roku-plugin-host", LogLevel::Warn, message.to_string())
			.with_field("plugin_id", plugin_id.to_string()),
	);
}

fn log_inventory_summary(snapshot: &PluginRegistrySnapshot) {
	let disabled = snapshot
		.disabled_summaries()
		.into_iter()
		.map(|(plugin_id, reason)| format!("{plugin_id}:{reason}"))
		.collect::<Vec<_>>();
	let _ = emit_global_log(
		LogRecord::new(
			"roku-plugin-host",
			LogLevel::Info,
			"plugin inventory snapshot built",
		)
		.with_field("enabled_plugins", snapshot.enabled_ids().join(","))
		.with_field("disabled_plugins", disabled.join(",")),
	);
}

pub(crate) fn entry_with_disabled_reason(
	candidate: DiscoveredPluginCandidate,
	reason: PluginDisableReason,
	selected: bool,
) -> PluginRegistryEntry {
	PluginRegistryEntry {
		manifest: candidate.manifest,
		source: candidate.source,
		status: PluginStatus::Disabled,
		disable_reason: Some(reason),
		warnings: candidate.warnings,
		required: candidate.required,
		implementation_available: candidate.implementation_available,
		selected,
	}
}

pub(crate) fn entry_enabled(candidate: DiscoveredPluginCandidate) -> PluginRegistryEntry {
	PluginRegistryEntry {
		manifest: candidate.manifest,
		source: candidate.source,
		status: PluginStatus::Enabled,
		disable_reason: None,
		warnings: candidate.warnings,
		required: candidate.required,
		implementation_available: candidate.implementation_available,
		selected: true,
	}
}

#[cfg(test)]
mod tests {
	use crate::{PluginId, PluginPolicyConfig, PluginProfile};

	use super::profile_decision;

	#[test]
	fn minimal_profile_keeps_openrouter_enabled() {
		let profile = PluginProfile::Minimal;
		let openrouter = PluginId::new("openrouter").expect("id should be valid");
		let telegram = PluginId::new("telegram").expect("id should be valid");

		assert_eq!(profile_decision(profile, &openrouter), Some(true));
		assert_eq!(profile_decision(profile, &telegram), Some(false));
		assert_eq!(
			PluginPolicyConfig::default().profile,
			PluginProfile::Minimal
		);
	}
}
