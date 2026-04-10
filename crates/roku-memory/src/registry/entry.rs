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

//! Provider-neutral entry registry for runtime bundle assembly.
//!
//! This module owns entry bundle shape, bundle assembly, and provider-neutral
//! resolution flow. Composition roots may inject concrete registrations and
//! builders, but they must not tunnel provider-specific config objects or
//! wiring payloads through this API.
//!
//! Responsibilities here are intentionally narrow:
//! - define provider-neutral entry-facing types
//! - resolve the selected memory subsystem through Roku-owned registry logic
//! - assemble the stable runtime bundle contract
//!
//! Non-goals:
//! - define memory semantics
//! - define control-plane semantics
//! - own runtime policy
//! - accept provider-specific config objects
//! - become a generic startup wiring layer

use thiserror::Error;

use crate::{
	ControlPlaneDataPlane, MemoryEntryRegistry, MemoryRuntimeConfig, MemorySubsystemRegistration,
	ResolvedMemorySubsystem,
};

/// Provider-neutral view over memory config needed by entry resolution.
///
/// This stays intentionally small so composition roots cannot tunnel
/// provider-specific config objects through `roku-memory`.
pub struct EntryMemoryConfig<'a> {
	pub core: &'a MemoryRuntimeConfig,
}

/// Stable runtime bundle shape consumed by entry surfaces.
pub struct ResolvedEntryRuntimeBundle {
	pub memory: ResolvedMemorySubsystem,
	pub control_plane: ControlPlaneDataPlane,
}

#[derive(Debug, Error)]
pub enum EntryRegistryError {
	#[error("memory bundle resolution failed: {0}")]
	Memory(String),
	#[error("control-plane bundle resolution failed: {0}")]
	ControlPlane(String),
}

/// Provider-neutral builder contract for the entry-layer control-plane bundle.
///
/// The contract deliberately admits only the final builder capability; caller-
/// owned provider config stays outside `roku-memory`.
pub trait EntryControlPlaneBuilder {
	fn build_control_plane(&self) -> Result<ControlPlaneDataPlane, String>;
}

/// Entry-facing adapter catalog injected by composition roots.
///
/// This catalog is intentionally narrow: it only expresses registered adapter
/// implementations and the control-plane bundle builder needed by the entry
/// registry. Provider-specific config objects must stay outside this API.
#[derive(Default)]
pub struct EntryAdapterCatalog<'a> {
	memory_registrations: Vec<&'a dyn MemorySubsystemRegistration>,
	control_plane_builder: Option<&'a dyn EntryControlPlaneBuilder>,
}

impl<'a> EntryAdapterCatalog<'a> {
	pub fn new() -> Self {
		Self::default()
	}

	pub fn register_memory(
		&mut self,
		registration: &'a dyn MemorySubsystemRegistration,
	) -> &mut Self {
		self.memory_registrations.push(registration);
		self
	}

	pub fn register_control_plane(
		&mut self,
		builder: &'a dyn EntryControlPlaneBuilder,
	) -> &mut Self {
		self.control_plane_builder = Some(builder);
		self
	}
}

pub fn resolve_memory_subsystem(
	config: EntryMemoryConfig<'_>,
	catalog: &EntryAdapterCatalog<'_>,
) -> Result<ResolvedMemorySubsystem, EntryRegistryError> {
	let mut registry = MemoryEntryRegistry::new();
	for registration in &catalog.memory_registrations {
		registry.register(*registration);
	}

	registry
		.resolve_subsystem(config.core.enabled, config.core.backend)
		.map_err(|error| EntryRegistryError::Memory(error.to_string()))
}

pub fn resolve_entry_runtime_bundle(
	memory: EntryMemoryConfig<'_>,
	catalog: &EntryAdapterCatalog<'_>,
) -> Result<ResolvedEntryRuntimeBundle, EntryRegistryError> {
	let memory = resolve_memory_subsystem(memory, catalog)?;
	let builder = catalog.control_plane_builder.ok_or_else(|| {
		EntryRegistryError::ControlPlane("no control-plane builder registered".to_string())
	})?;
	let control_plane = builder
		.build_control_plane()
		.map_err(EntryRegistryError::ControlPlane)?;

	Ok(ResolvedEntryRuntimeBundle {
		memory,
		control_plane,
	})
}

#[cfg(test)]
mod tests {
	use std::sync::Arc;

	use crate::pending_loop::NoopPendingLoopSnapshotBackend;
	use crate::session::{NoopSessionManagementBackend, NoopSessionStateBackend};
	use crate::short_term::NoopShortTermContinuityBackend;
	use crate::{
		ControlPlaneDataPlane, MemoryAdapterAvailability, MemoryBackendId,
		MemorySubsystemRegistration, NoopLongTermMemoryBackend, ResolvedMemorySubsystem,
	};

	use super::{
		EntryAdapterCatalog, EntryControlPlaneBuilder, EntryMemoryConfig, EntryRegistryError,
		resolve_entry_runtime_bundle, resolve_memory_subsystem,
	};

	struct StubControlPlaneBuilder;

	impl EntryControlPlaneBuilder for StubControlPlaneBuilder {
		fn build_control_plane(&self) -> Result<ControlPlaneDataPlane, String> {
			Ok(ControlPlaneDataPlane::in_memory())
		}
	}

	struct StubRegistration {
		availability: MemoryAdapterAvailability,
	}

	impl MemorySubsystemRegistration for StubRegistration {
		fn availability(&self) -> MemoryAdapterAvailability {
			self.availability
		}

		fn resolve_subsystem(&self) -> Result<ResolvedMemorySubsystem, String> {
			Ok(ResolvedMemorySubsystem::with_parts(
				Arc::new(NoopLongTermMemoryBackend),
				Box::new(NoopShortTermContinuityBackend),
				Box::new(NoopSessionStateBackend),
				Box::new(NoopPendingLoopSnapshotBackend),
				Box::new(NoopSessionManagementBackend),
			))
		}
	}

	#[test]
	fn resolves_memory_subsystem_through_entry_catalog() {
		let sqlite = StubRegistration {
			availability: MemoryAdapterAvailability {
				backend: MemoryBackendId::Sqlite,
				long_term: false,
				short_term: true,
				session_state: true,
				pending_loop: true,
				session_management: true,
			},
		};
		let mut catalog = EntryAdapterCatalog::new();
		catalog.register_memory(&sqlite);

		let memory = resolve_memory_subsystem(
			EntryMemoryConfig {
				core: &crate::MemoryRuntimeConfig {
					enabled: true,
					backend: MemoryBackendId::Sqlite,
					..crate::MemoryRuntimeConfig::default()
				},
			},
			&catalog,
		)
		.expect("registered memory should resolve");

		assert_eq!(memory.long_term.backend_name(), "noop");
	}

	#[test]
	fn entry_catalog_keeps_disabled_fallback_inside_provider_neutral_registry() {
		let catalog = EntryAdapterCatalog::new();

		let memory = resolve_memory_subsystem(
			EntryMemoryConfig {
				core: &crate::MemoryRuntimeConfig {
					enabled: true,
					backend: MemoryBackendId::OpenViking,
					..crate::MemoryRuntimeConfig::default()
				},
			},
			&catalog,
		)
		.expect("missing registration should resolve to disabled bundle");

		assert_eq!(memory.long_term.backend_name(), "noop");
		assert!(
			memory
				.short_term
				.load_short_term_continuity("session-1", 8)
				.expect("disabled continuity should respond")
				.is_empty()
		);
		assert!(
			memory
				.session_state
				.load_session_state("session-1")
				.expect("disabled session-state should respond")
				.is_none()
		);
	}

	#[test]
	fn resolves_entry_bundle_with_control_plane_builder() {
		let sqlite = StubRegistration {
			availability: MemoryAdapterAvailability {
				backend: MemoryBackendId::Sqlite,
				long_term: false,
				short_term: true,
				session_state: true,
				pending_loop: true,
				session_management: true,
			},
		};
		let builder = StubControlPlaneBuilder;
		let mut catalog = EntryAdapterCatalog::new();
		catalog
			.register_memory(&sqlite)
			.register_control_plane(&builder);

		let bundle = resolve_entry_runtime_bundle(
			EntryMemoryConfig {
				core: &crate::MemoryRuntimeConfig {
					enabled: true,
					backend: MemoryBackendId::Sqlite,
					..crate::MemoryRuntimeConfig::default()
				},
			},
			&catalog,
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

	#[test]
	fn entry_bundle_requires_control_plane_builder() {
		let sqlite = StubRegistration {
			availability: MemoryAdapterAvailability {
				backend: MemoryBackendId::Sqlite,
				long_term: false,
				short_term: true,
				session_state: true,
				pending_loop: true,
				session_management: true,
			},
		};
		let mut catalog = EntryAdapterCatalog::new();
		catalog.register_memory(&sqlite);

		let result = resolve_entry_runtime_bundle(
			EntryMemoryConfig {
				core: &crate::MemoryRuntimeConfig {
					enabled: true,
					backend: MemoryBackendId::Sqlite,
					..crate::MemoryRuntimeConfig::default()
				},
			},
			&catalog,
		);

		assert!(matches!(result, Err(EntryRegistryError::ControlPlane(_))));
	}
}
