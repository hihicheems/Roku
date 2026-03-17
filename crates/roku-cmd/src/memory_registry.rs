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

#[cfg(test)]
mod tests {
	use roku_common_types::{ConversationRole, ConversationTurn, PendingLoopBinding};
	use roku_memory::{MemoryBackendId, SessionState};

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

		let mut subsystem =
			resolve_memory_subsystem(&config).expect("sqlite subsystem should resolve");

		subsystem
			.short_term
			.append_continuity_turn(
				"session-1",
				ConversationTurn {
					role: ConversationRole::User,
					content: "hello".to_string(),
					created_at_unix_ms: 1,
				},
			)
			.expect("short-term continuity should append");
		subsystem
			.session_state
			.save_session_state(
				"session-1",
				SessionState {
					planning_mode: None,
					pending_loop: Some(PendingLoopBinding {
						run_id: "loop-1".to_string(),
						loop_state_json: "{\"status\":\"waiting\"}".to_string(),
					}),
				},
			)
			.expect("session state should save");
		subsystem
			.pending_loop
			.save_pending_loop_snapshot("session-1", None)
			.expect("pending loop snapshot should save");

		assert_eq!(
			subsystem
				.short_term
				.load_short_term_continuity("session-1", 8)
				.expect("continuity should load")
				.len(),
			1
		);
		assert_eq!(
			subsystem
				.session_state
				.load_session_state("session-1")
				.expect("session state should load")
				.expect("session state should exist")
				.pending_loop,
			None
		);
		assert_eq!(
			subsystem
				.pending_loop
				.load_pending_loop_snapshot("session-1")
				.expect("pending loop snapshot should load"),
			None
		);
	}

	#[cfg(not(feature = "memory-openviking"))]
	#[test]
	fn unregistered_openviking_backend_falls_back_to_disabled_bundle() {
		let config = MemoryRuntimeConfig {
			core: roku_memory::MemoryRuntimeConfig {
				enabled: true,
				backend: MemoryBackendId::OpenViking,
				..roku_memory::MemoryRuntimeConfig::default()
			},
			..MemoryRuntimeConfig::default()
		};

		let subsystem =
			resolve_memory_subsystem(&config).expect("missing adapter should not hard fail");

		assert_eq!(subsystem.long_term.backend_name(), "noop");
		assert!(
			subsystem
				.short_term
				.load_short_term_continuity("session-1", 8)
				.expect("noop continuity should load")
				.is_empty()
		);
	}
}
