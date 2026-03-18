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
	use std::sync::{Mutex, OnceLock};

	use super::*;

	fn env_mutex() -> &'static Mutex<()> {
		static ENV_MUTEX: OnceLock<Mutex<()>> = OnceLock::new();
		ENV_MUTEX.get_or_init(|| Mutex::new(()))
	}

	struct EnvGuard {
		key: &'static str,
		original: Option<String>,
	}

	impl EnvGuard {
		fn set(key: &'static str, value: &str) -> Self {
			let original = std::env::var(key).ok();
			unsafe {
				std::env::set_var(key, value);
			}
			Self { key, original }
		}

		fn remove(key: &'static str) -> Self {
			let original = std::env::var(key).ok();
			unsafe {
				std::env::remove_var(key);
			}
			Self { key, original }
		}
	}

	impl Drop for EnvGuard {
		fn drop(&mut self) {
			if let Some(value) = &self.original {
				unsafe {
					std::env::set_var(self.key, value);
				}
			} else {
				unsafe {
					std::env::remove_var(self.key);
				}
			}
		}
	}

	#[test]
	fn sqlite_config_defaults_to_control_plane_path() {
		let config = SqliteMemoryConfig::default();
		assert_eq!(config.path, PathBuf::from(".roku/state/control-plane.db"));
	}

	#[test]
	fn sqlite_config_accepts_legacy_env_alias() {
		let _env_lock = env_mutex().lock().expect("env mutex should lock");
		let _canonical = EnvGuard::remove("ROKU_RUNTIME__MEMORY__BACKENDS__SQLITE__PATH");
		let _legacy = EnvGuard::set("ROKU_SQLITE_PATH", "/tmp/legacy-control-plane.db");
		let mut config = SqliteMemoryConfig::default();

		config
			.apply_env_overrides()
			.expect("legacy env override should apply");

		assert_eq!(config.path, PathBuf::from("/tmp/legacy-control-plane.db"));
	}

	#[test]
	fn canonical_sqlite_env_takes_precedence_over_legacy_alias() {
		let _env_lock = env_mutex().lock().expect("env mutex should lock");
		let _canonical = EnvGuard::set(
			"ROKU_RUNTIME__MEMORY__BACKENDS__SQLITE__PATH",
			"/tmp/canonical-control-plane.db",
		);
		let _legacy = EnvGuard::set("ROKU_SQLITE_PATH", "/tmp/legacy-control-plane.db");
		let mut config = SqliteMemoryConfig::default();

		config
			.apply_env_overrides()
			.expect("canonical env override should apply");

		assert_eq!(
			config.path,
			PathBuf::from("/tmp/canonical-control-plane.db")
		);
	}
}
