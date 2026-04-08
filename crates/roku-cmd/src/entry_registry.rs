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

//! Command-side private adapter-catalog shim into the Roku-owned entry registry.
//!
//! `roku-cmd` still owns local layout discovery and typed runtime config
//! loading. Provider registration, resolution, and runtime bundle assembly now
//! live in `roku-memory::registry::entry`; this module only injects the
//! concrete adapter registrations/builders that the provider-neutral entry API
//! needs.
//!
//! This shim must remain thin. It may inject concrete registrations/builders,
//! project command-local config/layout into provider-neutral entry inputs, and
//! map errors back into `CommandError`. It must not grow backend selection,
//! fallback, bundle-shape definitions, resolved-bundle caching, or any other
//! second-registry behavior.

use roku_memory::registry::{
	EntryAdapterCatalog, EntryControlPlaneBuilder, EntryMemoryConfig, EntryRegistryError,
	EntryRuntimeLayout, ResolvedEntryRuntimeBundle,
	resolve_entry_runtime_bundle as resolve_entry_runtime_bundle_from_registry,
	resolve_memory_subsystem as resolve_memory_subsystem_from_registry,
};
use roku_memory::{ControlPlaneDataPlane, ResolvedMemorySubsystem};
#[cfg(feature = "memory-openviking")]
use roku_plugin_memory_openviking::OpenVikingMemorySubsystemRegistration;
use roku_plugin_memory_sqlite::{
	SqliteControlPlaneConfig, SqliteControlPlaneDataPlane, SqliteMemorySubsystemRegistration,
};

use crate::CommandError;
use crate::memory_runtime_config::MemoryRuntimeConfig;
use crate::storage::LocalStorageLayout;

#[derive(Debug, Clone)]
struct SqliteControlPlaneBuilder {
	config: SqliteControlPlaneConfig,
}

impl SqliteControlPlaneBuilder {
	fn from_memory_config(memory_config: &MemoryRuntimeConfig) -> Self {
		Self {
			config: SqliteControlPlaneConfig::from_memory_config(&memory_config.backends.sqlite),
		}
	}
}

impl EntryControlPlaneBuilder for SqliteControlPlaneBuilder {
	fn build_control_plane(&self) -> Result<ControlPlaneDataPlane, String> {
		SqliteControlPlaneDataPlane::connect(self.config.clone()).map_err(|error| error.to_string())
	}
}

pub(crate) fn resolve_memory_subsystem(
	memory_config: &MemoryRuntimeConfig,
) -> Result<ResolvedMemorySubsystem, CommandError> {
	with_entry_catalog(memory_config, |config, catalog| {
		resolve_memory_subsystem_from_registry(config, catalog)
	})
	.map_err(map_entry_registry_error)
}

pub(crate) fn resolve_entry_runtime_bundle(
	memory_config: &MemoryRuntimeConfig,
	layout: &LocalStorageLayout,
) -> Result<ResolvedEntryRuntimeBundle, CommandError> {
	with_entry_catalog(memory_config, |config, catalog| {
		resolve_entry_runtime_bundle_from_registry(config, &entry_layout(layout), catalog)
	})
	.map_err(map_entry_registry_error)
}

fn with_entry_catalog<T>(
	memory_config: &MemoryRuntimeConfig,
	resolve: impl FnOnce(
		EntryMemoryConfig<'_>,
		&EntryAdapterCatalog<'_>,
	) -> Result<T, EntryRegistryError>,
) -> Result<T, EntryRegistryError> {
	// Concrete adapter references stay private to the command-side shim. The
	// provider-neutral registry continues to own selection and disabled-fallback
	// semantics.
	let sqlite_memory =
		SqliteMemorySubsystemRegistration::new(memory_config.backends.sqlite.clone());
	let sqlite_control_plane = SqliteControlPlaneBuilder::from_memory_config(memory_config);
	let mut catalog = EntryAdapterCatalog::new();
	catalog
		.register_memory(&sqlite_memory)
		.register_control_plane(&sqlite_control_plane);

	#[cfg(feature = "memory-openviking")]
	let openviking_memory =
		OpenVikingMemorySubsystemRegistration::new(memory_config.backends.openviking.clone());

	#[cfg(feature = "memory-openviking")]
	catalog.register_memory(&openviking_memory);

	resolve(memory_config_view(memory_config), &catalog)
}

fn memory_config_view(memory_config: &MemoryRuntimeConfig) -> EntryMemoryConfig<'_> {
	EntryMemoryConfig {
		core: &memory_config.core,
	}
}

fn entry_layout(layout: &LocalStorageLayout) -> EntryRuntimeLayout {
	EntryRuntimeLayout {
		artifact_root: layout.artifact_root.clone(),
		experiment_root: layout.experiment_root.clone(),
	}
}

fn map_entry_registry_error(error: EntryRegistryError) -> CommandError {
	match error {
		EntryRegistryError::Memory(message) => CommandError::MemoryBackend(message),
		EntryRegistryError::ControlPlane(message) => CommandError::ControlPlaneBootstrap(message),
	}
}

#[cfg(test)]
mod tests {
	use roku_memory::MemoryBackendId;

	use super::*;

	#[test]
	fn resolves_sqlite_entry_bundle_from_canonical_memory_config() {
		let tempdir = tempfile::tempdir().expect("tempdir should exist");
		let mut config = MemoryRuntimeConfig {
			core: roku_memory::MemoryRuntimeConfig {
				enabled: true,
				backend: MemoryBackendId::Sqlite,
				..roku_memory::MemoryRuntimeConfig::default()
			},
			..MemoryRuntimeConfig::default()
		};
		config.backends.sqlite.path = tempdir.path().join("memory.db");
		let layout = LocalStorageLayout {
			home_dir: tempdir.path().join(".roku"),
			state_dir: tempdir.path().join(".roku").join("state"),
			legacy_sqlite_compat_path: tempdir.path().join("legacy-control-plane.db"),
			artifact_root: tempdir.path().join("artifacts"),
			experiment_root: tempdir.path().join("experiments"),
			report_root: tempdir.path().join("reports"),
			skill_root: tempdir.path().join("skills"),
			generated_skill_root: tempdir.path().join("skills"),
			tool_config_path: tempdir.path().join("config").join("tools.toml"),
			runtime_config_path: tempdir.path().join("config").join("runtime.toml"),
			plugin_config_path: tempdir.path().join("config").join("plugins.toml"),
			workspace_plugin_root: tempdir.path().join("workspace-plugins"),
			user_plugin_root: tempdir.path().join("user-plugins"),
			prompt_archive_dir: tempdir.path().join("prompts"),
			memory_summary_dir: tempdir.path().join("memory"),
			audit_export_dir: tempdir.path().join("audit"),
			log_dir: tempdir.path().join("logs"),
			run_dir: tempdir.path().join("run"),
			cache_dir: tempdir.path().join("cache"),
		};

		let bundle =
			resolve_entry_runtime_bundle(&config, &layout).expect("entry bundle should resolve");

		assert!(config.backends.sqlite.path.exists());
		assert!(!layout.legacy_sqlite_compat_path.exists());
		assert_eq!(bundle.memory.long_term.backend_name(), "sqlite-fts5");
		assert!(
			bundle
				.control_plane
				.task_repo
				.load_task(&roku_common_types::TaskId("missing".to_string()))
				.expect("control-plane task repo should respond")
				.is_none()
		);
	}
}
