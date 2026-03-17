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

//! Command-side adapter into the Roku-owned entry registry.
//!
//! `roku-cmd` still owns local layout discovery and typed runtime config
//! loading. Provider registration, resolution, and runtime bundle assembly live
//! in `roku-entry-registry`; this module only bridges command-local inputs into
//! that registry.

use roku_entry_registry::{
	EntryMemoryConfig, EntryRegistryError, EntryRuntimeLayout, ResolvedEntryRuntimeBundle,
	resolve_entry_runtime_bundle as resolve_entry_runtime_bundle_from_registry,
	resolve_memory_subsystem as resolve_memory_subsystem_from_registry,
};
use roku_memory::ResolvedMemorySubsystem;

use crate::CommandError;
use crate::memory_runtime_config::MemoryRuntimeConfig;
use crate::storage::LocalStorageLayout;

pub(crate) fn resolve_memory_subsystem(
	memory_config: &MemoryRuntimeConfig,
) -> Result<ResolvedMemorySubsystem, CommandError> {
	resolve_memory_subsystem_from_registry(memory_config_view(memory_config))
		.map_err(map_entry_registry_error)
}

pub(crate) fn resolve_entry_runtime_bundle(
	memory_config: &MemoryRuntimeConfig,
	layout: &LocalStorageLayout,
) -> Result<ResolvedEntryRuntimeBundle, CommandError> {
	resolve_entry_runtime_bundle_from_registry(
		memory_config_view(memory_config),
		&entry_layout(layout),
	)
	.map_err(map_entry_registry_error)
}

fn memory_config_view(memory_config: &MemoryRuntimeConfig) -> EntryMemoryConfig<'_> {
	EntryMemoryConfig {
		core: &memory_config.core,
		sqlite: &memory_config.backends.sqlite,
		#[cfg(feature = "memory-openviking")]
		openviking: Some(&memory_config.backends.openviking),
	}
}

fn entry_layout(layout: &LocalStorageLayout) -> EntryRuntimeLayout {
	EntryRuntimeLayout {
		control_plane_sqlite_path: layout.sqlite_path.clone(),
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
	fn resolves_sqlite_entry_bundle_from_provider_neutral_config() {
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
			sqlite_path: tempdir.path().join("control-plane.db"),
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

		assert_eq!(bundle.memory.long_term.backend_name(), "noop");
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
