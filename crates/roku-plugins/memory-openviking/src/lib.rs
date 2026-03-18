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
mod registration;
mod runtime_state_doc;

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
pub use registration::{OpenVikingMemoryRegistration, OpenVikingMemorySubsystemRegistration};
