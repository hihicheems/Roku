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

//! Startup-side composition for `runtime.memory.*`.
//!
//! The provider-neutral top-level schema now lives in `roku-memory`.
//! This module keeps only the process-local startup glue that layers adapter
//! subtrees on top of that core config and applies env overrides/materialized
//! provider artifacts for this command process.

use std::ops::{Deref, DerefMut};
use std::path::PathBuf;

use roku_memory::{
	MemoryBackendId, MemoryRuntimeConfig as CoreMemoryRuntimeConfig,
	MemoryRuntimeConfigError as CoreMemoryRuntimeConfigError,
	MemoryRuntimeConfigPatch as CoreMemoryRuntimeConfigPatch,
};
use roku_plugin_memory_openviking::{
	OpenVikingRuntimeConfig, OpenVikingRuntimeConfigError, OpenVikingRuntimeConfigPatch,
};
use roku_plugin_memory_sqlite::{
	SqliteMemoryConfig, SqliteMemoryConfigError, SqliteMemoryConfigPatch,
};
use serde::Deserialize;
use thiserror::Error;

#[cfg(test)]
pub const HARD_MAX_MEMORY_REQUEST_TIMEOUT_MS: u64 = 120_000;
#[cfg(test)]
pub const HARD_MAX_OPENVIKING_EMBED_MAX_CONCURRENT: usize = 64;
#[cfg(test)]
pub const HARD_MAX_OPENVIKING_VLM_MAX_CONCURRENT: usize = 64;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemoryRuntimeConfig {
	pub core: CoreMemoryRuntimeConfig,
	pub backends: MemoryBackendConfigs,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryRuntimeConfigPatch {
	#[serde(flatten)]
	pub core: CoreMemoryRuntimeConfigPatch,
	#[serde(default)]
	pub backends: Option<MemoryBackendConfigsPatch>,
}

/// Provider-specific config subtree anchored under `runtime.memory.backends.*`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemoryBackendConfigs {
	pub openviking: OpenVikingRuntimeConfig,
	pub sqlite: SqliteMemoryConfig,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryBackendConfigsPatch {
	#[serde(default)]
	pub openviking: Option<OpenVikingRuntimeConfigPatch>,
	#[serde(default)]
	pub sqlite: Option<SqliteMemoryConfigPatch>,
}

#[derive(Debug, Error)]
pub enum MemoryRuntimeConfigError {
	#[error("invalid environment variable {key}: {message}")]
	InvalidEnv { key: &'static str, message: String },
	#[error(transparent)]
	Core(#[from] CoreMemoryRuntimeConfigError),
	#[error(transparent)]
	OpenViking(#[from] OpenVikingRuntimeConfigError),
	#[error(transparent)]
	Sqlite(#[from] SqliteMemoryConfigError),
}

impl Deref for MemoryRuntimeConfig {
	type Target = CoreMemoryRuntimeConfig;

	fn deref(&self) -> &Self::Target {
		&self.core
	}
}

impl DerefMut for MemoryRuntimeConfig {
	fn deref_mut(&mut self) -> &mut Self::Target {
		&mut self.core
	}
}

impl MemoryRuntimeConfig {
	pub fn apply_patch(&mut self, patch: MemoryRuntimeConfigPatch) {
		self.core.apply_patch(patch.core);
		if let Some(value) = patch.backends {
			self.backends.apply_patch(value);
		}
	}

	pub fn apply_env_overrides(&mut self) -> Result<(), MemoryRuntimeConfigError> {
		if let Some(value) = env_override_bool("ROKU_RUNTIME__MEMORY__ENABLED")? {
			self.enabled = value;
		}
		if let Some(value) = env_override_enum("ROKU_RUNTIME__MEMORY__BACKEND")? {
			self.backend = value;
		}
		if let Some(value) = env_override_bool("ROKU_RUNTIME__MEMORY__RECALL__ENABLED")? {
			self.recall.enabled = value;
		}
		if let Some(value) = env_override_usize("ROKU_RUNTIME__MEMORY__RECALL__TOP_K")? {
			self.recall.top_k = value;
		}
		if let Some(value) = env_override_bool("ROKU_RUNTIME__MEMORY__WRITE__ENABLED")? {
			self.write.enabled = value;
		}
		if let Some(value) = env_override_usize("ROKU_RUNTIME__MEMORY__WRITE__MAX_BATCH_SIZE")? {
			self.write.max_batch_size = value;
		}

		self.backends.openviking.apply_env_overrides(self.enabled)?;
		self.backends.sqlite.apply_env_overrides()?;
		Ok(())
	}

	pub fn validate_and_clamp(&mut self) -> Result<(), MemoryRuntimeConfigError> {
		self.core.validate_and_clamp()?;
		self.backends.openviking.validate_and_clamp()?;
		self.backends.sqlite.validate()?;
		Ok(())
	}

	pub fn materialize_generated_openviking_config(
		&self,
	) -> Result<Option<PathBuf>, MemoryRuntimeConfigError> {
		self.backends
			.openviking
			.materialize_generated_config(
				self.enabled && matches!(self.backend, MemoryBackendId::OpenViking),
			)
			.map_err(MemoryRuntimeConfigError::from)
	}
}

impl MemoryBackendConfigs {
	fn apply_patch(&mut self, patch: MemoryBackendConfigsPatch) {
		if let Some(value) = patch.openviking {
			self.openviking.apply_patch(value);
		}
		if let Some(value) = patch.sqlite {
			self.sqlite.apply_patch(value);
		}
	}
}

fn env_override_string(key: &'static str) -> Option<String> {
	std::env::var(key)
		.ok()
		.map(|value| value.trim().to_string())
		.filter(|value| !value.is_empty())
}

fn env_override_bool(key: &'static str) -> Result<Option<bool>, MemoryRuntimeConfigError> {
	let Some(raw) = env_override_string(key) else {
		return Ok(None);
	};
	match raw.to_ascii_lowercase().as_str() {
		"1" | "true" | "yes" | "on" => Ok(Some(true)),
		"0" | "false" | "no" | "off" => Ok(Some(false)),
		_ => Err(MemoryRuntimeConfigError::InvalidEnv {
			key,
			message: "expected boolean value".to_string(),
		}),
	}
}

fn env_override_usize(key: &'static str) -> Result<Option<usize>, MemoryRuntimeConfigError> {
	let Some(raw) = env_override_string(key) else {
		return Ok(None);
	};
	raw.parse::<usize>()
		.map(Some)
		.map_err(|error| MemoryRuntimeConfigError::InvalidEnv {
			key,
			message: error.to_string(),
		})
}

fn env_override_enum<T>(key: &'static str) -> Result<Option<T>, MemoryRuntimeConfigError>
where
	T: std::str::FromStr<Err = &'static str>,
{
	let Some(raw) = env_override_string(key) else {
		return Ok(None);
	};
	T::from_str(&raw)
		.map(Some)
		.map_err(|message| MemoryRuntimeConfigError::InvalidEnv {
			key,
			message: message.to_string(),
		})
}

#[cfg(test)]
mod tests {
	use roku_memory::{
		HARD_MAX_MEMORY_RECALL_TOP_K, HARD_MAX_MEMORY_WRITE_BATCH_SIZE, MemoryRecallConfig,
		MemoryWriteConfig,
	};

	use super::*;

	#[test]
	fn startup_wrapper_keeps_core_schema_in_roku_memory() {
		let mut config = MemoryRuntimeConfig {
			core: CoreMemoryRuntimeConfig {
				enabled: true,
				backend: MemoryBackendId::OpenViking,
				recall: MemoryRecallConfig {
					enabled: true,
					top_k: 999,
				},
				write: MemoryWriteConfig {
					enabled: true,
					max_batch_size: 999,
				},
			},
			..MemoryRuntimeConfig::default()
		};

		config.validate_and_clamp().expect("config should validate");

		assert_eq!(config.recall.top_k, HARD_MAX_MEMORY_RECALL_TOP_K);
		assert_eq!(
			config.write.max_batch_size,
			HARD_MAX_MEMORY_WRITE_BATCH_SIZE
		);
	}
}
