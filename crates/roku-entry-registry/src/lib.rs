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

//! Roku-owned entry registry for runtime bundle assembly.
//!
//! This crate is intentionally limited to registration, resolution, and bundle
//! assembly. It does not define memory semantics, control-plane semantics,
//! provider-specific config schemas, or runtime policy.

use std::path::PathBuf;

use roku_artifact_store::ArtifactStore;
use roku_control_plane::ControlPlaneDataPlane;
use roku_experiment_registry::ExperimentRegistry;
use roku_memory::{MemoryEntryRegistry, MemoryRuntimeConfig, ResolvedMemorySubsystem};
use roku_plugin_control_plane_sqlite::{SqliteControlPlaneConfig, SqliteControlPlaneDataPlane};
#[cfg(feature = "memory-openviking")]
use roku_plugin_memory_openviking::{
	OpenVikingMemorySubsystemRegistration, OpenVikingRuntimeConfig,
};
use roku_plugin_memory_sqlite::{SqliteMemoryConfig, SqliteMemorySubsystemRegistration};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryRuntimeLayout {
	pub control_plane_sqlite_path: PathBuf,
	pub artifact_root: PathBuf,
	pub experiment_root: PathBuf,
}

pub struct EntryMemoryConfig<'a> {
	pub core: &'a MemoryRuntimeConfig,
	pub sqlite: &'a SqliteMemoryConfig,
	#[cfg(feature = "memory-openviking")]
	pub openviking: Option<&'a OpenVikingRuntimeConfig>,
}

pub struct ResolvedEntryRuntimeBundle {
	pub memory: ResolvedMemorySubsystem,
	pub control_plane: ControlPlaneDataPlane,
	pub artifact_store: ArtifactStore,
	pub experiment_registry: ExperimentRegistry,
}

#[derive(Debug, Error)]
pub enum EntryRegistryError {
	#[error("memory bundle resolution failed: {0}")]
	Memory(String),
	#[error("control-plane bundle resolution failed: {0}")]
	ControlPlane(String),
}

pub fn resolve_memory_subsystem(
	config: EntryMemoryConfig<'_>,
) -> Result<ResolvedMemorySubsystem, EntryRegistryError> {
	let sqlite = SqliteMemorySubsystemRegistration::new(config.sqlite.clone());
	let mut registry = MemoryEntryRegistry::new();
	registry.register(&sqlite);

	#[cfg(feature = "memory-openviking")]
	let openviking = config
		.openviking
		.map(|config| OpenVikingMemorySubsystemRegistration::new(config.clone()));

	#[cfg(feature = "memory-openviking")]
	if let Some(openviking) = openviking.as_ref() {
		registry.register(openviking);
	}

	registry
		.resolve_subsystem(config.core.enabled, config.core.backend)
		.map_err(|error| EntryRegistryError::Memory(error.to_string()))
}

pub fn resolve_entry_runtime_bundle(
	memory: EntryMemoryConfig<'_>,
	layout: &EntryRuntimeLayout,
) -> Result<ResolvedEntryRuntimeBundle, EntryRegistryError> {
	let memory = resolve_memory_subsystem(memory)?;
	let control_plane = SqliteControlPlaneDataPlane::connect(SqliteControlPlaneConfig::new(
		layout.control_plane_sqlite_path.clone(),
	))
	.map_err(|error| EntryRegistryError::ControlPlane(error.to_string()))?;

	Ok(ResolvedEntryRuntimeBundle {
		memory,
		control_plane,
		artifact_store: ArtifactStore::file_backed(layout.artifact_root.clone()),
		experiment_registry: ExperimentRegistry::file_backed(layout.experiment_root.clone()),
	})
}

#[cfg(test)]
mod tests {
	use super::*;
	use roku_memory::MemoryBackendId;

	#[test]
	fn resolves_sqlite_memory_subsystem_without_openviking_feature() {
		let tempdir = tempfile::tempdir().expect("tempdir should exist");
		let memory = resolve_memory_subsystem(EntryMemoryConfig {
			core: &MemoryRuntimeConfig {
				enabled: true,
				backend: MemoryBackendId::Sqlite,
				..MemoryRuntimeConfig::default()
			},
			sqlite: &SqliteMemoryConfig {
				path: tempdir.path().join("memory.db"),
			},
			#[cfg(feature = "memory-openviking")]
			openviking: None,
		})
		.expect("sqlite memory should resolve");

		assert_eq!(memory.long_term.backend_name(), "noop");
	}

	#[test]
	fn resolves_entry_runtime_bundle_with_sqlite_control_plane() {
		let tempdir = tempfile::tempdir().expect("tempdir should exist");
		let bundle = resolve_entry_runtime_bundle(
			EntryMemoryConfig {
				core: &MemoryRuntimeConfig {
					enabled: true,
					backend: MemoryBackendId::Sqlite,
					..MemoryRuntimeConfig::default()
				},
				sqlite: &SqliteMemoryConfig {
					path: tempdir.path().join("memory.db"),
				},
				#[cfg(feature = "memory-openviking")]
				openviking: None,
			},
			&EntryRuntimeLayout {
				control_plane_sqlite_path: tempdir.path().join("control-plane.db"),
				artifact_root: tempdir.path().join("artifacts"),
				experiment_root: tempdir.path().join("experiments"),
			},
		)
		.expect("entry bundle should resolve");

		assert_eq!(bundle.memory.long_term.backend_name(), "noop");
		assert!(
			bundle
				.control_plane
				.task_repo
				.load_task(&roku_common_types::TaskId("missing".to_string()))
				.expect("control-plane repo should respond")
				.is_none()
		);
	}
}
