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

//! OpenViking adapter for Roku memory contracts.
//!
//! This crate maps the provider-neutral contracts from `roku-memory` onto the
//! OpenViking HTTP API. It is intentionally limited to provider configuration,
//! request/response translation, and backend capability handling.
//!
//! Runtime policy such as when recall happens, what should be written back, and
//! how recalled memories are injected into prompts remains outside this crate.

mod backend;
mod config;

use std::sync::Arc;

use roku_memory::{
	DisabledMemoryLifecyclePolicy, LongTermMemoryBackend, MemoryAdapterAvailability,
	MemoryBackendId, MemorySubsystemRegistration, ResolvedMemorySubsystem,
};

pub use backend::{
	OpenVikingBackendBootstrapError, OpenVikingLongTermMemoryBackend, OpenVikingMemoryAdapters,
	OpenVikingPendingLoopSnapshotAdapter, OpenVikingSessionStateAdapter,
	OpenVikingShortTermContinuityAdapter,
};
pub use config::{
	OpenVikingAdapterConfig, OpenVikingAdapterConfigPatch, OpenVikingBackendConfig,
	OpenVikingBackendConfigError, OpenVikingClientConfig, OpenVikingClientConfigPatch,
	OpenVikingEmbeddingConfig, OpenVikingEmbeddingConfigPatch, OpenVikingEmbeddingInput,
	OpenVikingEmbeddingProvider, OpenVikingProcessConfig, OpenVikingProcessConfigPatch,
	OpenVikingRuntimeConfig, OpenVikingRuntimeConfigError, OpenVikingRuntimeConfigPatch,
	OpenVikingServerConfig, OpenVikingServerConfigPatch, OpenVikingStorageBackend,
	OpenVikingStorageConfig, OpenVikingStorageConfigPatch, OpenVikingVlmConfig,
	OpenVikingVlmConfigPatch, OpenVikingVlmProvider,
};

/// Registration surface for the OpenViking memory adapter.
pub struct OpenVikingMemoryRegistration;

impl OpenVikingMemoryRegistration {
	/// Advertises the currently implemented OpenViking memory capabilities.
	pub const fn availability() -> MemoryAdapterAvailability {
		MemoryAdapterAvailability {
			backend: MemoryBackendId::OpenViking,
			long_term: true,
			short_term: true,
			session_state: true,
			pending_loop: true,
		}
	}

	/// Builds the provider-neutral long-term backend from adapter-owned config.
	pub fn build_long_term_backend(
		config: &OpenVikingRuntimeConfig,
	) -> Result<Arc<dyn LongTermMemoryBackend>, OpenVikingBackendBootstrapError> {
		let backend = OpenVikingLongTermMemoryBackend::new(config.to_backend_config())?;
		Ok(Arc::new(backend))
	}

	/// Connects the full set of currently implemented OpenViking-backed adapters.
	pub fn connect_adapters(
		config: &OpenVikingRuntimeConfig,
	) -> Result<OpenVikingMemoryAdapters, OpenVikingBackendBootstrapError> {
		OpenVikingMemoryAdapters::connect(config.to_backend_config())
	}
}

/// Config-bound OpenViking registration consumed by the Roku entry registry.
#[derive(Debug, Clone)]
pub struct OpenVikingMemorySubsystemRegistration {
	config: OpenVikingRuntimeConfig,
}

impl OpenVikingMemorySubsystemRegistration {
	pub fn new(config: OpenVikingRuntimeConfig) -> Self {
		Self { config }
	}
}

impl MemorySubsystemRegistration for OpenVikingMemorySubsystemRegistration {
	fn availability(&self) -> MemoryAdapterAvailability {
		OpenVikingMemoryRegistration::availability()
	}

	fn resolve_subsystem(&self) -> Result<ResolvedMemorySubsystem, String> {
		let adapters = OpenVikingMemoryRegistration::connect_adapters(&self.config)
			.map_err(|error| error.to_string())?;
		Ok(ResolvedMemorySubsystem::with_parts(
			Arc::new(adapters.long_term),
			Box::new(adapters.short_term),
			Box::new(adapters.session_state),
			Box::new(adapters.pending_loop),
			Arc::new(DisabledMemoryLifecyclePolicy),
		))
	}
}
