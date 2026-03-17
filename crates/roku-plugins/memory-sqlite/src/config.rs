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

//! Typed configuration for the SQLite memory adapter.

use std::env;
use std::path::PathBuf;

use serde::Deserialize;
use serde_json::{Value, json};
use thiserror::Error;

const DEFAULT_SQLITE_PATH: &str = ".roku/state/control-plane.db";
const SQLITE_PATH_ENV: &str = "ROKU_RUNTIME__MEMORY__BACKENDS__SQLITE__PATH";
const SQLITE_PATH_LEGACY_ENV: &str = "ROKU_SQLITE_PATH";

/// Validated configuration for SQLite-backed memory adapters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqliteMemoryConfig {
	/// SQLite database path used by continuity/session/pending-loop adapters.
	pub path: PathBuf,
}

/// Partial runtime patch for [`SqliteMemoryConfig`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SqliteMemoryConfigPatch {
	pub path: Option<PathBuf>,
}

/// Validation and env override errors for [`SqliteMemoryConfig`].
#[derive(Debug, Error)]
pub enum SqliteMemoryConfigError {
	#[error("invalid environment variable {key}: {message}")]
	InvalidEnv { key: &'static str, message: String },
	#[error("invalid runtime.memory.backends.sqlite configuration for {field}: {message}")]
	InvalidConfig {
		field: &'static str,
		message: String,
	},
}

impl Default for SqliteMemoryConfig {
	fn default() -> Self {
		Self {
			path: PathBuf::from(DEFAULT_SQLITE_PATH),
		}
	}
}

impl SqliteMemoryConfig {
	/// Applies a parsed runtime.toml patch.
	pub fn apply_patch(&mut self, patch: SqliteMemoryConfigPatch) {
		if let Some(value) = patch.path {
			self.path = value;
		}
	}

	/// Applies canonical and legacy env overrides.
	pub fn apply_env_overrides(&mut self) -> Result<(), SqliteMemoryConfigError> {
		if let Some(value) = env_override_path_with_legacy(SQLITE_PATH_ENV, SQLITE_PATH_LEGACY_ENV)?
		{
			self.path = value;
		}
		Ok(())
	}

	/// Validates that adapter-required fields are present.
	pub fn validate(&mut self) -> Result<(), SqliteMemoryConfigError> {
		if self.path.as_os_str().is_empty() {
			return Err(SqliteMemoryConfigError::InvalidConfig {
				field: "runtime.memory.backends.sqlite.path",
				message: "value cannot be empty".to_string(),
			});
		}
		Ok(())
	}

	/// Returns a redacted config summary suitable for operator-facing reports.
	pub fn summary_json(&self) -> Value {
		json!({
			"path": self.path.display().to_string(),
		})
	}
}

fn env_override_path_with_legacy(
	canonical: &'static str,
	legacy: &'static str,
) -> Result<Option<PathBuf>, SqliteMemoryConfigError> {
	if let Some(value) = env_override_path(canonical)? {
		return Ok(Some(value));
	}
	env_override_path(legacy)
}

fn env_override_path(key: &'static str) -> Result<Option<PathBuf>, SqliteMemoryConfigError> {
	let Some(raw) = env::var(key).ok() else {
		return Ok(None);
	};
	let value = raw.trim();
	if value.is_empty() {
		return Err(SqliteMemoryConfigError::InvalidEnv {
			key,
			message: "expected non-empty path value".to_string(),
		});
	}
	Ok(Some(PathBuf::from(value)))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn sqlite_config_defaults_to_control_plane_path() {
		let config = SqliteMemoryConfig::default();
		assert_eq!(config.path, PathBuf::from(".roku/state/control-plane.db"));
	}
}
