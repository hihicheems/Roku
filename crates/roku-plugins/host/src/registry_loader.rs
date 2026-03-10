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

use std::collections::{BTreeMap, BTreeSet};

use roku_plugin_core::{
	PluginDisableReason, PluginPolicyConfig, PluginRegistryEntry, PluginRegistrySnapshot,
};

use crate::discovery::DiscoveredPluginCandidate;
use crate::startup::{
	admission_disable_reason, entry_enabled, entry_with_disabled_reason, policy_disable_reason,
	requirements_disable_reason,
};

pub(crate) fn load_registry_snapshot(
	mut candidates: Vec<DiscoveredPluginCandidate>,
	policy: &PluginPolicyConfig,
) -> PluginRegistrySnapshot {
	candidates.sort_by(|left, right| {
		left.source
			.precedence
			.cmp(&right.source.precedence)
			.then_with(|| left.manifest.id.cmp(&right.manifest.id))
			.then_with(|| left.source.manifest_path.cmp(&right.source.manifest_path))
	});

	let reserved_bundled_ids = candidates
		.iter()
		.filter(|candidate| candidate.required)
		.map(|candidate| candidate.manifest.id.clone())
		.collect::<BTreeSet<_>>();
	let mut winners = BTreeMap::<String, usize>::new();
	let mut entries = Vec::<PluginRegistryEntry>::new();

	for candidate in candidates {
		let plugin_id = candidate.manifest.id.to_string();
		if !candidate.source.is_bundled() && reserved_bundled_ids.contains(&candidate.manifest.id) {
			entries.push(entry_with_disabled_reason(
				candidate,
				PluginDisableReason::ReservedBundledPluginId,
				false,
			));
			continue;
		}

		if winners.contains_key(&plugin_id) {
			entries.push(entry_with_disabled_reason(
				candidate,
				PluginDisableReason::ShadowedByHigherPrecedence { winner: plugin_id },
				false,
			));
			continue;
		}

		let (admission_reason, admission_warnings) = admission_disable_reason(&candidate, policy);
		let mut candidate = candidate;
		candidate.warnings.extend(admission_warnings);
		let reason = if !candidate.implementation_available && !candidate.source.is_bundled() {
			Some(PluginDisableReason::UnsupportedExternalPlugin)
		} else {
			policy_disable_reason(&candidate, policy)
				.or(admission_reason)
				.or_else(|| requirements_disable_reason(&candidate))
		};

		let entry = match reason {
			Some(reason) => entry_with_disabled_reason(candidate, reason, true),
			None => entry_enabled(candidate),
		};
		winners.insert(plugin_id, entries.len());
		entries.push(entry);
	}

	PluginRegistrySnapshot::new(entries, reserved_bundled_ids.into_iter().collect())
}

#[cfg(test)]
mod tests {
	use std::fs;

	use roku_plugin_core::{
		PluginCapabilities, PluginDisableReason, PluginId, PluginKind, PluginManifest,
		PluginPolicyConfig, PluginRequirements, PluginSource, PluginSourceKind, PluginStatus,
	};

	use crate::discovery::DiscoveredPluginCandidate;

	use super::load_registry_snapshot;

	fn candidate(
		id: &str,
		kind: PluginSourceKind,
		precedence: u8,
		required: bool,
		implementation_available: bool,
	) -> DiscoveredPluginCandidate {
		DiscoveredPluginCandidate {
			manifest: PluginManifest {
				id: PluginId::new(id).expect("id should be valid"),
				kind: PluginKind::Provider,
				enabled_by_default: true,
				capabilities: PluginCapabilities::default(),
				requirements: PluginRequirements::default(),
			},
			source: PluginSource::new(kind, None, None, precedence),
			warnings: Vec::new(),
			required,
			implementation_available,
		}
	}

	#[test]
	fn required_bundled_plugin_cannot_be_shadowed() {
		let snapshot = load_registry_snapshot(
			vec![
				candidate(
					"builtin-tools",
					PluginSourceKind::ConfigPath,
					0,
					false,
					true,
				),
				candidate("builtin-tools", PluginSourceKind::Bundled, 4, true, true),
			],
			&PluginPolicyConfig::default(),
		);

		let bundled = snapshot.entry("builtin-tools").expect("entry should exist");
		assert_eq!(bundled.status, PluginStatus::Enabled);
		assert!(snapshot.entries().iter().any(|entry| {
			entry.disable_reason == Some(PluginDisableReason::ReservedBundledPluginId)
		}));
	}

	#[test]
	fn unsupported_external_plugin_stays_disabled() {
		let snapshot = load_registry_snapshot(
			vec![candidate(
				"external-search",
				PluginSourceKind::WorkspaceRoot,
				2,
				false,
				false,
			)],
			&PluginPolicyConfig::default(),
		);

		let entry = snapshot
			.entry("external-search")
			.expect("entry should exist in snapshot");
		assert_eq!(entry.status, PluginStatus::Disabled);
		assert_eq!(
			entry.disable_reason,
			Some(PluginDisableReason::UnsupportedExternalPlugin)
		);
	}

	#[test]
	fn world_writable_path_is_rejected() {
		let temp_dir = tempfile::tempdir().expect("tempdir should exist");
		let plugin_dir = temp_dir.path().join("plugin");
		fs::create_dir_all(&plugin_dir).expect("plugin dir should be created");
		#[cfg(unix)]
		{
			use std::os::unix::fs::PermissionsExt;

			let mut permissions = fs::metadata(&plugin_dir)
				.expect("metadata should exist")
				.permissions();
			permissions.set_mode(0o777);
			fs::set_permissions(&plugin_dir, permissions).expect("permissions should update");
		}

		let snapshot = load_registry_snapshot(
			vec![DiscoveredPluginCandidate {
				manifest: PluginManifest {
					id: PluginId::new("external-plugin").expect("id should be valid"),
					kind: PluginKind::Provider,
					enabled_by_default: true,
					capabilities: PluginCapabilities::default(),
					requirements: PluginRequirements::default(),
				},
				source: PluginSource::new(
					PluginSourceKind::WorkspaceRoot,
					Some(plugin_dir.clone()),
					Some(plugin_dir.join("roku.plugin.toml")),
					2,
				),
				warnings: Vec::new(),
				required: false,
				implementation_available: true,
			}],
			&PluginPolicyConfig::default(),
		);

		let entry = snapshot
			.entry("external-plugin")
			.expect("entry should exist");
		assert_eq!(entry.status, PluginStatus::Disabled);
		assert!(matches!(
			entry.disable_reason,
			Some(PluginDisableReason::AdmissionRejected { .. })
		));
	}

	#[test]
	fn missing_env_disables_optional_plugin() {
		let snapshot = load_registry_snapshot(
			vec![DiscoveredPluginCandidate {
				manifest: PluginManifest {
					id: PluginId::new("openrouter").expect("id should be valid"),
					kind: PluginKind::Provider,
					enabled_by_default: true,
					capabilities: PluginCapabilities::default(),
					requirements: PluginRequirements {
						env: vec!["ROKU_TEST_MISSING_ENV".to_string()],
						..PluginRequirements::default()
					},
				},
				source: PluginSource::new(PluginSourceKind::Bundled, None, None, 4),
				warnings: Vec::new(),
				required: false,
				implementation_available: true,
			}],
			&PluginPolicyConfig::default(),
		);

		let entry = snapshot.entry("openrouter").expect("entry should exist");
		assert_eq!(entry.status, PluginStatus::Disabled);
		assert!(matches!(
			entry.disable_reason,
			Some(PluginDisableReason::MissingRequiredEnv { .. })
		));
	}

	#[test]
	fn allowlist_overrides_profile_but_entry_override_wins() {
		let policy = PluginPolicyConfig::from_toml(
			r#"
profile = "minimal"
allow = ["telegram"]

[entries.telegram]
enabled = false
"#,
		)
		.expect("policy should parse");
		let snapshot = load_registry_snapshot(
			vec![DiscoveredPluginCandidate {
				manifest: PluginManifest {
					id: PluginId::new("telegram").expect("id should be valid"),
					kind: PluginKind::Connector,
					enabled_by_default: false,
					capabilities: PluginCapabilities::default(),
					requirements: PluginRequirements::default(),
				},
				source: PluginSource::new(PluginSourceKind::Bundled, None, None, 4),
				warnings: Vec::new(),
				required: false,
				implementation_available: true,
			}],
			&policy,
		);

		let entry = snapshot.entry("telegram").expect("entry should exist");
		assert_eq!(entry.status, PluginStatus::Disabled);
		assert_eq!(
			entry.disable_reason,
			Some(PluginDisableReason::ExplicitlyDisabled)
		);
	}
}
