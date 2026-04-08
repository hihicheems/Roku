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

//! Roku-owned provider-neutral memory configuration.
//!
//! This module owns the top-level meaning of `runtime.memory.*`:
//! whether memory is enabled, which backend id is selected, and which
//! provider-neutral recall/write limits apply.
//!
//! Adapter-specific subtrees such as `runtime.memory.backends.openviking.*`
//! or `runtime.memory.backends.sqlite.*` remain outside this module. Entry
//! startup may compose those subtrees alongside this config, but it does not
//! own the provider-neutral schema itself.

use serde::Deserialize;
use thiserror::Error;

use crate::MemoryBackendId;

pub const HARD_MAX_MEMORY_RECALL_TOP_K: usize = 64;
pub const HARD_MAX_MEMORY_WRITE_BATCH_SIZE: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryRuntimeConfig {
	pub enabled: bool,
	pub backend: MemoryBackendId,
	pub recall: MemoryRecallConfig,
	pub write: MemoryWriteConfig,
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

#[derive(Debug, Error)]
pub enum MemoryRuntimeConfigError {
	#[error("invalid runtime.memory configuration for {field}: {message}")]
	InvalidConfig {
		field: &'static str,
		message: String,
	},
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

impl Default for MemoryRuntimeConfig {
	fn default() -> Self {
		Self {
			enabled: true,
			backend: MemoryBackendId::Sqlite,
			recall: MemoryRecallConfig::default(),
			write: MemoryWriteConfig::default(),
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
		Ok(())
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

fn invalid_config(field: &'static str, message: &str) -> MemoryRuntimeConfigError {
	MemoryRuntimeConfigError::InvalidConfig {
		field,
		message: message.to_string(),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn provider_neutral_memory_config_clamps_limits() {
		let mut config = MemoryRuntimeConfig {
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
		};

		config.validate_and_clamp().expect("config should validate");

		assert_eq!(config.recall.top_k, HARD_MAX_MEMORY_RECALL_TOP_K);
		assert_eq!(
			config.write.max_batch_size,
			HARD_MAX_MEMORY_WRITE_BATCH_SIZE
		);
	}

	#[test]
	fn provider_neutral_memory_config_defaults_to_sqlite() {
		let config = MemoryRuntimeConfig::default();

		assert!(config.enabled);
		assert_eq!(config.backend, MemoryBackendId::Sqlite);
		assert!(config.recall.enabled);
		assert!(config.write.enabled);
	}
}
