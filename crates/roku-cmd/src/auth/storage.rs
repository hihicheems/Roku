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

//! Credential persistence for OAuth and API-key providers.
//!
//! `AuthStore` owns the on-disk `auth.json` contract. It serialises and
//! deserialises [`AuthFile`] with 0o600 permissions on Unix so that tokens are
//! not world-readable.

use std::collections::HashMap;
use std::env;
use std::fs;
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::AuthError;

// ---------------------------------------------------------------------------
// Schema types
// ---------------------------------------------------------------------------

/// Top-level structure stored in `auth.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuthFile {
	/// The provider that is currently active, e.g. `"openai"`.
	pub active_provider: Option<String>,
	/// Per-provider credential entries keyed by provider name.
	#[serde(default)]
	pub credentials: HashMap<String, CredentialEntry>,
}

/// Claims extracted from an OpenID Connect id_token.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IdTokenClaims {
	pub email: Option<String>,
	pub user_id: Option<String>,
	pub account_id: Option<String>,
}

/// Discriminated union of credential types stored per provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CredentialEntry {
	/// A plain API key (e.g. OpenRouter, Anthropic).
	ApiKey { api_key: String },
	/// An OAuth credential that includes tokens and id-token claims.
	OAuth {
		/// The API key obtained via RFC 8693 token-exchange (the value stored
		/// here IS the usable `sk-*` API key, not the raw OAuth access_token).
		access_token: String,
		refresh_token: String,
		id_token_claims: IdTokenClaims,
		last_refresh_unix_ms: i64,
	},
}

// ---------------------------------------------------------------------------
// Auth store
// ---------------------------------------------------------------------------

/// Manages reading and writing `$ROKU_HOME/auth.json`.
#[derive(Debug, Clone)]
pub struct AuthStore {
	path: PathBuf,
}

impl AuthStore {
	/// Construct an [`AuthStore`] whose path is resolved from the environment.
	///
	/// Uses `$ROKU_HOME` when set; falls back to `~/.roku`.
	pub fn from_env() -> Self {
		Self {
			path: auth_file_path(),
		}
	}

	/// Construct an [`AuthStore`] at an explicit path (useful in tests).
	#[cfg(test)]
	pub fn at(path: impl Into<PathBuf>) -> Self {
		Self { path: path.into() }
	}

	/// Returns the path to `auth.json`.
	#[allow(dead_code)] // Public API for callers that need the resolved path.
	pub fn auth_file_path(&self) -> &PathBuf {
		&self.path
	}

	/// Load [`AuthFile`] from disk.
	///
	/// Returns `Ok(None)` when the file does not exist.
	pub fn load(&self) -> Result<Option<AuthFile>, AuthError> {
		match fs::read_to_string(&self.path) {
			Ok(contents) => {
				let file: AuthFile = serde_json::from_str(&contents)
					.map_err(|e| AuthError::Storage(format!("parse auth.json: {e}")))?;
				Ok(Some(file))
			}
			Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
			Err(e) => Err(AuthError::Storage(format!("read auth.json: {e}"))),
		}
	}

	/// Persist [`AuthFile`] to disk.
	///
	/// Creates parent directories as needed. On Unix the file is written with
	/// mode 0o600 so that credentials are not world-readable.
	pub fn save(&self, auth: &AuthFile) -> Result<(), AuthError> {
		if let Some(parent) = self.path.parent() {
			fs::create_dir_all(parent)
				.map_err(|e| AuthError::Storage(format!("create auth dir: {e}")))?;
		}

		let json = serde_json::to_string_pretty(auth)
			.map_err(|e| AuthError::Storage(format!("serialize auth.json: {e}")))?;

		// Atomic write: write to temp sibling → fsync → rename over target.
		// A crash between truncate and write cannot corrupt the existing file.
		let tmp = self.path.with_extension("json.tmp");
		write_restricted(&tmp, &json)
			.map_err(|e| AuthError::Storage(format!("write auth.json.tmp: {e}")))?;
		fs::rename(&tmp, &self.path)
			.map_err(|e| AuthError::Storage(format!("rename auth.json.tmp: {e}")))?;

		Ok(())
	}

	/// Remove the credential entry for `provider` and persist the result.
	///
	/// If the deleted provider was also the `active_provider`, that field is
	/// cleared. A missing file is treated as a no-op (returns `Ok(())`).
	pub fn delete_credential(&self, provider: &str) -> Result<(), AuthError> {
		let mut auth = self.load()?.unwrap_or_default();
		auth.credentials.remove(provider);
		if auth.active_provider.as_deref() == Some(provider) {
			auth.active_provider = None;
		}
		self.save(&auth)
	}
}

// ---------------------------------------------------------------------------
// Path resolution helpers
// ---------------------------------------------------------------------------

/// Resolve the canonical `auth.json` path from the environment.
fn auth_file_path() -> PathBuf {
	roku_home().join("auth.json")
}

fn roku_home() -> PathBuf {
	env::var("ROKU_HOME")
		.ok()
		.filter(|v| !v.trim().is_empty())
		.map(|v| expand_tilde(v.trim()))
		.unwrap_or_else(default_roku_home)
}

fn default_roku_home() -> PathBuf {
	env::var_os("HOME")
		.map(PathBuf::from)
		.or_else(|| env::var_os("USERPROFILE").map(PathBuf::from))
		.map(|h| h.join(".roku"))
		.unwrap_or_else(|| PathBuf::from(".roku"))
}

fn expand_tilde(value: &str) -> PathBuf {
	if value == "~" {
		return default_roku_home();
	}
	if let Some(suffix) = value.strip_prefix("~/")
		&& let Some(home) = env::var_os("HOME").map(PathBuf::from)
	{
		return home.join(suffix);
	}
	PathBuf::from(value)
}

// ---------------------------------------------------------------------------
// Platform-specific secure write
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn write_restricted(path: &std::path::Path, content: &str) -> io::Result<()> {
	use std::os::unix::fs::OpenOptionsExt;
	use std::os::unix::fs::PermissionsExt;
	let mut opts = fs::OpenOptions::new();
	opts.write(true).create(true).truncate(true).mode(0o600);
	let mut file = opts.open(path)?;
	io::Write::write_all(&mut file, content.as_bytes())?;
	file.sync_all()?; // fsync before rename ensures data is on disk.
	// Enforce 0o600 even if the file already existed with broader permissions.
	file.set_permissions(fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn write_restricted(path: &std::path::Path, content: &str) -> io::Result<()> {
	fs::write(path, content)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn auth_file_round_trip() {
		let dir = tempfile::tempdir().expect("tempdir");
		let path = dir.path().join("auth.json");
		let store = AuthStore::at(&path);

		// Nothing on disk yet.
		assert!(store.load().expect("load").is_none());

		// Build a file with one OAuth entry.
		let mut auth = AuthFile {
			active_provider: Some("openai".to_string()),
			credentials: HashMap::new(),
		};
		auth.credentials.insert(
			"openai".to_string(),
			CredentialEntry::OAuth {
				access_token: "sk-test".to_string(),
				refresh_token: "rt-test".to_string(),
				id_token_claims: IdTokenClaims {
					email: Some("user@example.com".to_string()),
					user_id: Some("uid-1".to_string()),
					account_id: Some("acc-1".to_string()),
				},
				last_refresh_unix_ms: 1_712_880_000_000,
			},
		);

		store.save(&auth).expect("save");

		let loaded = store.load().expect("load").expect("some");
		assert_eq!(loaded.active_provider.as_deref(), Some("openai"));

		let entry = loaded.credentials.get("openai").expect("openai entry");
		let CredentialEntry::OAuth {
			access_token,
			refresh_token,
			id_token_claims,
			last_refresh_unix_ms,
		} = entry
		else {
			panic!("expected OAuth variant");
		};
		assert_eq!(access_token, "sk-test");
		assert_eq!(refresh_token, "rt-test");
		assert_eq!(id_token_claims.email.as_deref(), Some("user@example.com"));
		assert_eq!(*last_refresh_unix_ms, 1_712_880_000_000);
	}

	#[test]
	fn delete_credential_clears_active_provider() {
		let dir = tempfile::tempdir().expect("tempdir");
		let store = AuthStore::at(dir.path().join("auth.json"));

		let mut auth = AuthFile {
			active_provider: Some("openrouter".to_string()),
			credentials: HashMap::new(),
		};
		auth.credentials.insert(
			"openrouter".to_string(),
			CredentialEntry::ApiKey {
				api_key: "sk-or-test".to_string(),
			},
		);
		store.save(&auth).expect("save");

		store.delete_credential("openrouter").expect("delete");

		let loaded = store.load().expect("load").expect("some");
		assert!(loaded.active_provider.is_none());
		assert!(loaded.credentials.is_empty());
	}

	#[test]
	fn missing_file_returns_none() {
		let dir = tempfile::tempdir().expect("tempdir");
		let store = AuthStore::at(dir.path().join("nonexistent.json"));
		assert!(store.load().expect("load").is_none());
	}

	#[cfg(unix)]
	#[test]
	fn file_permissions_are_0o600() {
		use std::os::unix::fs::PermissionsExt;

		let dir = tempfile::tempdir().expect("tempdir");
		let store = AuthStore::at(dir.path().join("auth.json"));
		store.save(&AuthFile::default()).expect("save");

		let meta = fs::metadata(dir.path().join("auth.json")).expect("metadata");
		let mode = meta.permissions().mode() & 0o777;
		assert_eq!(mode, 0o600, "expected 0o600, got 0o{mode:o}");
	}
}
