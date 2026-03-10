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

use roku_plugin_core::{PluginDisableReason, PluginPolicyConfig};

use crate::discovery::DiscoveredPluginCandidate;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AdmissionOutcome {
	pub rejection: Option<PluginDisableReason>,
	pub warnings: Vec<String>,
}

pub(crate) fn admission_outcome(
	candidate: &DiscoveredPluginCandidate,
	policy: &PluginPolicyConfig,
) -> AdmissionOutcome {
	let mut warnings = candidate.warnings.clone();
	let Some(root) = candidate.source.root.as_ref() else {
		return AdmissionOutcome {
			rejection: None,
			warnings,
		};
	};
	let Some(manifest_path) = candidate.source.manifest_path.as_ref() else {
		return AdmissionOutcome {
			rejection: None,
			warnings,
		};
	};

	if !path_is_within_root(manifest_path, root) {
		return AdmissionOutcome {
			rejection: Some(PluginDisableReason::AdmissionRejected {
				detail: "manifest path escapes discovery root".to_string(),
			}),
			warnings,
		};
	}

	if is_world_writable(root) || is_world_writable(manifest_path) {
		return AdmissionOutcome {
			rejection: Some(PluginDisableReason::AdmissionRejected {
				detail: "world-writable plugin path is rejected".to_string(),
			}),
			warnings,
		};
	}

	if manifest_path.is_symlink() || root.is_symlink() {
		warnings.push("plugin path resolves through a symlink".to_string());
	}

	if !candidate.source.is_bundled()
		&& candidate.manifest.capabilities.has_side_effects
		&& !policy.allows(&candidate.manifest.id)
	{
		return AdmissionOutcome {
			rejection: Some(PluginDisableReason::NonBundledSideEffectRequiresAllow),
			warnings,
		};
	}

	AdmissionOutcome {
		rejection: None,
		warnings,
	}
}

fn path_is_within_root(path: &Path, root: &Path) -> bool {
	let Ok(canonical_root) = fs::canonicalize(root) else {
		return false;
	};
	let Ok(canonical_path) = fs::canonicalize(path) else {
		return false;
	};
	canonical_path.starts_with(canonical_root)
}

#[cfg(unix)]
fn is_world_writable(path: &Path) -> bool {
	use std::os::unix::fs::PermissionsExt;

	fs::metadata(path)
		.map(|metadata| metadata.permissions().mode() & 0o002 != 0)
		.unwrap_or(false)
}

#[cfg(not(unix))]
fn is_world_writable(_path: &Path) -> bool {
	false
}
