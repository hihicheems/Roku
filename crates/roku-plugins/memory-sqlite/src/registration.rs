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

use std::sync::Arc;

use roku_memory::{
	MemoryAdapterAvailability, MemoryBackendId, MemorySubsystemRegistration,
	ResolvedMemorySubsystem,
};

use crate::long_term::SqliteLongTermMemoryBackend;
use crate::store::SqliteMemoryStoreConfig;
use crate::{SqliteMemoryAdapterError, SqliteMemoryAdapters, SqliteMemoryConfig};

/// Registration surface for the SQLite memory adapter.
pub struct SqliteMemoryRegistration;

impl SqliteMemoryRegistration {
	/// Advertises the currently implemented SQLite memory capabilities.
	pub const fn availability() -> MemoryAdapterAvailability {
		MemoryAdapterAvailability {
			backend: MemoryBackendId::Sqlite,
			long_term: true,
			short_term: true,
			session_state: true,
			pending_loop: true,
			session_management: true,
		}
	}

	/// Resolves SQLite-backed continuity/session adapters into a provider-neutral bundle.
	pub fn resolve_subsystem(
		config: SqliteMemoryConfig,
	) -> Result<ResolvedMemorySubsystem, SqliteMemoryAdapterError> {
		let store_config = SqliteMemoryStoreConfig::new(config.path.clone());
		let long_term = SqliteLongTermMemoryBackend::connect(&store_config)
			.map_err(|e| SqliteMemoryAdapterError::Resolution(e.to_string()))?;
		let adapters = SqliteMemoryAdapters::connect(config)?;
		Ok(ResolvedMemorySubsystem::with_parts(
			Arc::new(long_term),
			Box::new(adapters.short_term),
			Box::new(adapters.session_state),
			Box::new(adapters.pending_loop),
			Box::new(adapters.session_management),
		))
	}
}

/// Config-bound SQLite registration consumed by the Roku entry registry.
#[derive(Debug, Clone)]
pub struct SqliteMemorySubsystemRegistration {
	config: SqliteMemoryConfig,
}

impl SqliteMemorySubsystemRegistration {
	pub fn new(config: SqliteMemoryConfig) -> Self {
		Self { config }
	}
}

impl MemorySubsystemRegistration for SqliteMemorySubsystemRegistration {
	fn availability(&self) -> MemoryAdapterAvailability {
		SqliteMemoryRegistration::availability()
	}

	fn resolve_subsystem(&self) -> Result<ResolvedMemorySubsystem, String> {
		SqliteMemoryRegistration::resolve_subsystem(self.config.clone())
			.map_err(|error| error.to_string())
	}
}
