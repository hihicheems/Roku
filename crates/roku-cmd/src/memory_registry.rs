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

//! Composition-root glue between typed runtime config and Roku's memory entry registry.
//!
//! `roku-cmd` is still responsible for loading process-local config and selecting
//! the requested runtime profile. The actual provider selection and subsystem
//! resolution, however, flows through the Roku-owned memory entry registry so
//! CLI and Telegram do not each rebuild adapter selection logic.

use roku_memory::{MemoryEntryRegistry, ResolvedMemorySubsystem};
#[cfg(feature = "memory-openviking")]
use roku_plugin_memory_openviking::OpenVikingMemorySubsystemRegistration;
use roku_plugin_memory_sqlite::SqliteMemorySubsystemRegistration;

use crate::CommandError;
use crate::memory_runtime_config::MemoryRuntimeConfig;

pub(crate) fn resolve_memory_subsystem(
	memory_config: &MemoryRuntimeConfig,
) -> Result<ResolvedMemorySubsystem, CommandError> {
	let sqlite = SqliteMemorySubsystemRegistration::new(memory_config.backends.sqlite.clone());
	#[cfg(feature = "memory-openviking")]
	let openviking =
		OpenVikingMemorySubsystemRegistration::new(memory_config.backends.openviking.clone());
	let mut registry = MemoryEntryRegistry::new();
	registry.register(&sqlite);

	#[cfg(feature = "memory-openviking")]
	registry.register(&openviking);

	registry
		.resolve_subsystem(memory_config.enabled, memory_config.backend)
		.map_err(|error| CommandError::MemoryBackend(error.to_string()))
}
