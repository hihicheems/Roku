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

//! Startup-side typed parsing for `runtime.memory.*` during the memory migration.
//!
//! The provider-neutral meaning of `runtime.memory.*` belongs to Roku's memory
//! subsystem. This module only keeps the provider-neutral top-level shape and
//! delegates provider-specific subtrees to adapter crates.

use std::path::PathBuf;

use roku_memory::MemoryBackendId;
use roku_plugin_memory_openviking::{
	OpenVikingRuntimeConfig, OpenVikingRuntimeConfigError, OpenVikingRuntimeConfigPatch,
};
use roku_plugin_memory_sqlite::{
	SqliteMemoryConfig, SqliteMemoryConfigError, SqliteMemoryConfigPatch,
};
use serde::Deserialize;
use thiserror::Error;

pub const HARD_MAX_MEMORY_RECALL_TOP_K: usize = 64;
pub const HARD_MAX_MEMORY_WRITE_BATCH_SIZE: usize = 256;
#[cfg(test)]
pub const HARD_MAX_MEMORY_REQUEST_TIMEOUT_MS: u64 = 120_000;
#[cfg(test)]
pub const HARD_MAX_OPENVIKING_EMBED_MAX_CONCURRENT: usize = 64;
#[cfg(test)]
pub const HARD_MAX_OPENVIKING_VLM_MAX_CONCURRENT: usize = 64;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemoryRuntimeConfig {
	pub enabled: bool,
	pub backend: MemoryBackendId,
	pub recall: MemoryRecallConfig,
	pub write: MemoryWriteConfig,
	pub backends: MemoryBackendConfigs,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryRuntimeConfigPatch {
	pub enabled: Option<bool>,
	pub backend: Option<MemoryBackendId>,
	#[serde(default)]
	pub recall: Option<MemoryRecallConfigPatch>,
	#[serde(default)]
	pub write: Option<MemoryWriteConfigPatch>,
	#[serde(default)]
	pub backends: Option<MemoryBackendConfigsPatch>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryRecallConfig {
	pub enabled: bool,
	pub top_k: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryRecallConfigPatch {
	pub enabled: Option<bool>,
	pub top_k: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryWriteConfig {
	pub enabled: bool,
	pub max_batch_size: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryWriteConfigPatch {
	pub enabled: Option<bool>,
	pub max_batch_size: Option<usize>,
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
	#[error("invalid runtime.memory configuration for {field}: {message}")]
	InvalidConfig {
		field: &'static str,
		message: String,
	},
	#[error(transparent)]
	OpenViking(#[from] OpenVikingRuntimeConfigError),
	#[error(transparent)]
	Sqlite(#[from] SqliteMemoryConfigError),
}

impl Default for MemoryRecallConfig {
	fn default() -> Self {
		Self {
			enabled: true,
			top_k: 8,
		}
	}
}

impl Default for MemoryWriteConfig {
	fn default() -> Self {
		Self {
			enabled: true,
			max_batch_size: 16,
		}
	}
}

impl MemoryRuntimeConfig {
	pub fn apply_patch(&mut self, patch: MemoryRuntimeConfigPatch) {
		if let Some(value) = patch.enabled {
			self.enabled = value;
		}
		if let Some(value) = patch.backend {
			self.backend = value;
		}
		if let Some(value) = patch.recall {
			self.recall.apply_patch(value);
		}
		if let Some(value) = patch.write {
			self.write.apply_patch(value);
		}
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
		if self.recall.top_k == 0 {
			return Err(invalid_config(
				"runtime.memory.recall.top_k",
				"value must be greater than zero",
			));
		}
		if self.write.max_batch_size == 0 {
			return Err(invalid_config(
				"runtime.memory.write.max_batch_size",
				"value must be greater than zero",
			));
		}
		self.recall.top_k = self.recall.top_k.min(HARD_MAX_MEMORY_RECALL_TOP_K);
		self.write.max_batch_size = self
			.write
			.max_batch_size
			.min(HARD_MAX_MEMORY_WRITE_BATCH_SIZE);

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

impl MemoryRecallConfig {
	fn apply_patch(&mut self, patch: MemoryRecallConfigPatch) {
		if let Some(value) = patch.enabled {
			self.enabled = value;
		}
		if let Some(value) = patch.top_k {
			self.top_k = value;
		}
	}
}

impl MemoryWriteConfig {
	fn apply_patch(&mut self, patch: MemoryWriteConfigPatch) {
		if let Some(value) = patch.enabled {
			self.enabled = value;
		}
		if let Some(value) = patch.max_batch_size {
			self.max_batch_size = value;
		}
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

fn invalid_config(field: &'static str, message: &str) -> MemoryRuntimeConfigError {
	MemoryRuntimeConfigError::InvalidConfig {
		field,
		message: message.to_string(),
	}
}
