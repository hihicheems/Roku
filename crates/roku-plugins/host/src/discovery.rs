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

use crate::{PluginCoreError, PluginManifest, PluginSource, PluginSourceKind};
use roku_common_types::{LogLevel, LogRecord, emit_global_log};
use walkdir::WalkDir;

use crate::startup::{BundledPluginDescriptor, PluginDiscoveryConfig, PluginHostError};

#[derive(Debug, Clone)]
pub(crate) struct DiscoveredPluginCandidate {
	pub manifest: PluginManifest,
	pub source: PluginSource,
	pub warnings: Vec<String>,
	pub required: bool,
	pub implementation_available: bool,
}

pub(crate) fn discover_plugin_candidates(
	config: &PluginDiscoveryConfig,
	bundled_descriptors: &[BundledPluginDescriptor],
) -> Result<Vec<DiscoveredPluginCandidate>, PluginHostError> {
	let mut candidates = Vec::new();
	let implementation_ids = bundled_descriptors
		.iter()
		.map(|descriptor| descriptor.manifest.id.clone())
		.collect::<Vec<_>>();

	for path in &config.explicit_paths {
		candidates.extend(discover_from_path(
			path,
			PluginSourceKind::ConfigPath,
			0,
			&implementation_ids,
		)?);
	}
	if let Some(path) = &config.env_root {
		candidates.extend(discover_from_path(
			path,
			PluginSourceKind::EnvRoot,
			1,
			&implementation_ids,
		)?);
	}
	candidates.extend(discover_from_path(
		&config.workspace_root,
		PluginSourceKind::WorkspaceRoot,
		2,
		&implementation_ids,
	)?);
	candidates.extend(discover_from_path(
		&config.user_root,
		PluginSourceKind::UserRoot,
		3,
		&implementation_ids,
	)?);
	candidates.extend(
		bundled_descriptors
			.iter()
			.map(|descriptor| DiscoveredPluginCandidate {
				manifest: descriptor.manifest.clone(),
				source: PluginSource::new(PluginSourceKind::Bundled, None, None, 4),
				warnings: Vec::new(),
				required: descriptor.required,
				implementation_available: true,
			}),
	);

	Ok(candidates)
}

fn discover_from_path(
	path: &Path,
	kind: PluginSourceKind,
	precedence: u8,
	implementation_ids: &[crate::PluginId],
) -> Result<Vec<DiscoveredPluginCandidate>, PluginHostError> {
	if !path.exists() {
		if matches!(
			kind,
			PluginSourceKind::ConfigPath | PluginSourceKind::EnvRoot
		) {
			log_discovery_warning(kind, path, "plugin discovery path does not exist");
		}
		return Ok(Vec::new());
	}

	let manifest_paths = if path.is_file() {
		if path.file_name().and_then(|value| value.to_str()) == Some("roku.plugin.toml") {
			vec![path.to_path_buf()]
		} else {
			log_discovery_warning(kind, path, "explicit plugin path is not a manifest file");
			Vec::new()
		}
	} else {
		WalkDir::new(path)
			.follow_links(false)
			.into_iter()
			.filter_map(Result::ok)
			.filter(|entry| entry.file_type().is_file())
			.filter(|entry| entry.file_name() == "roku.plugin.toml")
			.map(|entry| entry.into_path())
			.collect::<Vec<_>>()
	};

	let mut candidates = Vec::new();
	for manifest_path in manifest_paths {
		match parse_manifest_candidate(&manifest_path, kind, precedence, implementation_ids) {
			Ok(candidate) => candidates.push(candidate),
			Err(error) => log_discovery_warning(kind, &manifest_path, &error.to_string()),
		}
	}

	Ok(candidates)
}

fn parse_manifest_candidate(
	manifest_path: &Path,
	kind: PluginSourceKind,
	precedence: u8,
	implementation_ids: &[crate::PluginId],
) -> Result<DiscoveredPluginCandidate, PluginHostError> {
	let content = fs::read_to_string(manifest_path)?;
	let manifest = toml::from_str::<PluginManifest>(&content)
		.map_err(|error| PluginCoreError::ManifestParse(error.to_string()))?;
	let root = manifest_path
		.parent()
		.map(Path::to_path_buf)
		.ok_or_else(|| PluginCoreError::ManifestParse("manifest path has no parent".to_string()))?;
	let canonical_manifest = fs::canonicalize(manifest_path)?;
	let warnings = if canonical_manifest != manifest_path {
		vec!["manifest path resolves through a symlinked location".to_string()]
	} else {
		Vec::new()
	};
	let implementation_available = implementation_ids.iter().any(|id| id == &manifest.id);

	Ok(DiscoveredPluginCandidate {
		manifest,
		source: PluginSource::new(
			kind,
			Some(root),
			Some(manifest_path.to_path_buf()),
			precedence,
		),
		warnings,
		required: false,
		implementation_available,
	})
}

fn log_discovery_warning(kind: PluginSourceKind, path: &Path, message: &str) {
	let _ = emit_global_log(
		LogRecord::new("roku-plugin-host", LogLevel::Warn, message.to_string())
			.with_field("source_kind", format!("{kind:?}"))
			.with_field("path", path.display().to_string()),
	);
}
