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
//!
//! ## Schema versioning
//!
//! v1 (legacy): `credentials` maps provider name → single `CredentialEntry`.
//! v2 (current): `credentials` maps provider name → `Vec<CredentialEntry>`,
//! each entry carrying a `label` for multi-account disambiguation. v1 files
//! are transparently upgraded on load via [`CredentialSlot`].

use std::collections::HashMap;
use std::env;
use std::fs;
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Deserializer, Serialize};

use super::AuthError;

// ---------------------------------------------------------------------------
// Schema types
// ---------------------------------------------------------------------------

/// Top-level structure stored in `auth.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuthFile {
	/// The provider that is currently active, e.g. `"openai"`.
	pub active_provider: Option<String>,
	/// Label of the active account within the active provider.
	/// When `None`, the first entry in the provider's credential list is used.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub active_account: Option<String>,
	/// Per-provider credential entries keyed by provider name.
	/// Each provider maps to one or more credentials (multi-account).
	#[serde(default, deserialize_with = "deserialize_credentials")]
	pub credentials: HashMap<String, Vec<CredentialEntry>>,
}

impl AuthFile {
	/// Get the active credential entry for the active provider.
	#[allow(dead_code)] // Public convenience API; callers may use credential_for() directly.
	pub fn active_credential(&self) -> Option<&CredentialEntry> {
		let provider = self.active_provider.as_deref()?;
		self.credential_for(provider)
	}

	/// Get the active credential for a specific provider.
	///
	/// If `active_account` is set and this is the active provider, looks up by
	/// label. Otherwise falls back to the first entry.
	pub fn credential_for(&self, provider: &str) -> Option<&CredentialEntry> {
		let entries = self.credentials.get(provider)?;
		if let Some(label) = &self.active_account
			&& self.active_provider.as_deref() == Some(provider)
			&& let Some(entry) = entries.iter().find(|e| e.label() == label.as_str())
		{
			return Some(entry);
		}
		entries.first()
	}

	/// Upsert a credential: if an entry with the same label exists under the
	/// provider, replace it; otherwise append.
	pub fn upsert_credential(&mut self, provider: &str, entry: CredentialEntry) {
		let entries = self.credentials.entry(provider.to_string()).or_default();
		let label = entry.label().to_string();
		if let Some(existing) = entries.iter_mut().find(|e| e.label() == label) {
			*existing = entry;
		} else {
			entries.push(entry);
		}
	}

	/// Remove a single credential by provider + label. Returns `true` if an
	/// entry was actually removed.
	pub fn remove_credential(&mut self, provider: &str, label: &str) -> bool {
		if let Some(entries) = self.credentials.get_mut(provider) {
			let before = entries.len();
			entries.retain(|e| e.label() != label);
			let removed = entries.len() < before;
			if entries.is_empty() {
				self.credentials.remove(provider);
			}
			removed
		} else {
			false
		}
	}

	/// Check whether any credential is stored (across all providers).
	pub fn has_any_credential(&self) -> bool {
		self.credentials.values().any(|v| !v.is_empty())
	}

	/// Total number of individual credential entries across all providers.
	pub fn credential_count(&self) -> usize {
		self.credentials.values().map(|v| v.len()).sum()
	}
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
	ApiKey {
		#[serde(default = "default_label")]
		label: String,
		api_key: String,
	},
	/// An OAuth credential that includes tokens and id-token claims.
	///
	/// `rename_all = "snake_case"` maps `OAuth` → `"o_auth"` which is the
	/// tag written by v1. We rename to `"oauth"` for v2 clarity but keep
	/// `"o_auth"` as a deserialization alias so existing files still load.
	#[serde(rename = "oauth", alias = "o_auth")]
	OAuth {
		#[serde(default = "default_label")]
		label: String,
		/// The API key obtained via RFC 8693 token-exchange (the value stored
		/// here IS the usable `sk-*` API key, not the raw OAuth access_token).
		access_token: String,
		refresh_token: String,
		id_token_claims: IdTokenClaims,
		last_refresh_unix_ms: i64,
	},
}

fn default_label() -> String {
	"default".to_string()
}

impl CredentialEntry {
	/// Human-readable label for this credential.
	pub fn label(&self) -> &str {
		match self {
			CredentialEntry::ApiKey { label, .. } => label,
			CredentialEntry::OAuth { label, .. } => label,
		}
	}
}

// ---------------------------------------------------------------------------
// v1 → v2 migration support
// ---------------------------------------------------------------------------

fn deserialize_credentials<'de, D>(
	deserializer: D,
) -> Result<HashMap<String, Vec<CredentialEntry>>, D::Error>
where
	D: Deserializer<'de>,
{
	use serde_json::Value;

	// Deserialize as raw JSON values first, then decide per-slot whether
	// the value is an array (v2) or a single object (v1 legacy).
	let raw: HashMap<String, Value> = HashMap::deserialize(deserializer)?;
	let mut result = HashMap::with_capacity(raw.len());
	for (provider, value) in raw {
		let entries: Vec<CredentialEntry> = if value.is_array() {
			serde_json::from_value(value).map_err(serde::de::Error::custom)?
		} else {
			let single: CredentialEntry =
				serde_json::from_value(value).map_err(serde::de::Error::custom)?;
			vec![single]
		};
		result.insert(provider, entries);
	}
	Ok(result)
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

	/// Remove all credential entries for `provider` and persist the result.
	///
	/// If the deleted provider was also the `active_provider`, that field is
	/// cleared. A missing file is treated as a no-op (returns `Ok(())`).
	#[allow(dead_code)] // Provider-level delete; /logout now uses per-account remove_credential.
	pub fn delete_credential(&self, provider: &str) -> Result<(), AuthError> {
		let mut auth = self.load()?.unwrap_or_default();
		auth.credentials.remove(provider);
		if auth.active_provider.as_deref() == Some(provider) {
			auth.active_provider = None;
			auth.active_account = None;
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
			active_account: Some("user@example.com".to_string()),
			credentials: HashMap::new(),
		};
		auth.upsert_credential(
			"openai",
			CredentialEntry::OAuth {
				label: "user@example.com".to_string(),
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
		assert_eq!(loaded.active_account.as_deref(), Some("user@example.com"));

		let entry = loaded.credential_for("openai").expect("openai entry");
		let CredentialEntry::OAuth {
			access_token,
			refresh_token,
			id_token_claims,
			last_refresh_unix_ms,
			..
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
			..Default::default()
		};
		auth.upsert_credential(
			"openrouter",
			CredentialEntry::ApiKey {
				label: "default".to_string(),
				api_key: "sk-or-test".to_string(),
			},
		);
		store.save(&auth).expect("save");

		store.delete_credential("openrouter").expect("delete");

		let loaded = store.load().expect("load").expect("some");
		assert!(loaded.active_provider.is_none());
		assert!(!loaded.has_any_credential());
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

	#[test]
	fn v1_single_entry_migration() {
		let dir = tempfile::tempdir().expect("tempdir");
		let path = dir.path().join("auth.json");

		// Write a v1-format file (single CredentialEntry, not array).
		let v1_json = r#"{
            "active_provider": "openai",
            "credentials": {
                "openai": {
                    "kind": "o_auth",
                    "access_token": "sk-old",
                    "refresh_token": "rt-old",
                    "id_token_claims": { "email": "old@example.com" },
                    "last_refresh_unix_ms": 1000000
                }
            }
        }"#;
		fs::write(&path, v1_json).expect("write v1");

		let store = AuthStore::at(&path);
		let loaded = store.load().expect("load").expect("some");

		// Should have migrated the single entry into a vec.
		let entries = loaded.credentials.get("openai").expect("openai entries");
		assert_eq!(entries.len(), 1);
		assert_eq!(entries[0].label(), "default");

		let entry = loaded.credential_for("openai").expect("openai active");
		let CredentialEntry::OAuth { access_token, .. } = entry else {
			panic!("expected OAuth");
		};
		assert_eq!(access_token, "sk-old");
	}

	#[test]
	fn upsert_replaces_same_label() {
		let mut auth = AuthFile::default();
		auth.upsert_credential(
			"openai",
			CredentialEntry::OAuth {
				label: "alice@example.com".to_string(),
				access_token: "sk-1".to_string(),
				refresh_token: "rt-1".to_string(),
				id_token_claims: IdTokenClaims::default(),
				last_refresh_unix_ms: 100,
			},
		);
		auth.upsert_credential(
			"openai",
			CredentialEntry::OAuth {
				label: "alice@example.com".to_string(),
				access_token: "sk-2".to_string(),
				refresh_token: "rt-2".to_string(),
				id_token_claims: IdTokenClaims::default(),
				last_refresh_unix_ms: 200,
			},
		);
		let entries = auth.credentials.get("openai").unwrap();
		assert_eq!(entries.len(), 1, "same label should not duplicate");
		let CredentialEntry::OAuth { access_token, .. } = &entries[0] else {
			panic!("expected OAuth");
		};
		assert_eq!(access_token, "sk-2");
	}

	#[test]
	fn upsert_appends_different_label() {
		let mut auth = AuthFile::default();
		auth.upsert_credential(
			"openai",
			CredentialEntry::OAuth {
				label: "alice@example.com".to_string(),
				access_token: "sk-a".to_string(),
				refresh_token: "rt-a".to_string(),
				id_token_claims: IdTokenClaims::default(),
				last_refresh_unix_ms: 100,
			},
		);
		auth.upsert_credential(
			"openai",
			CredentialEntry::OAuth {
				label: "bob@example.com".to_string(),
				access_token: "sk-b".to_string(),
				refresh_token: "rt-b".to_string(),
				id_token_claims: IdTokenClaims::default(),
				last_refresh_unix_ms: 200,
			},
		);
		let entries = auth.credentials.get("openai").unwrap();
		assert_eq!(entries.len(), 2);
	}

	#[test]
	fn remove_credential_by_label() {
		let mut auth = AuthFile::default();
		auth.upsert_credential(
			"openai",
			CredentialEntry::OAuth {
				label: "alice@example.com".to_string(),
				access_token: "sk-a".to_string(),
				refresh_token: "rt-a".to_string(),
				id_token_claims: IdTokenClaims::default(),
				last_refresh_unix_ms: 100,
			},
		);
		auth.upsert_credential(
			"openai",
			CredentialEntry::OAuth {
				label: "bob@example.com".to_string(),
				access_token: "sk-b".to_string(),
				refresh_token: "rt-b".to_string(),
				id_token_claims: IdTokenClaims::default(),
				last_refresh_unix_ms: 200,
			},
		);

		assert!(auth.remove_credential("openai", "alice@example.com"));
		let entries = auth.credentials.get("openai").unwrap();
		assert_eq!(entries.len(), 1);
		assert_eq!(entries[0].label(), "bob@example.com");
	}

	#[test]
	fn credential_for_respects_active_account() {
		let mut auth = AuthFile {
			active_provider: Some("openai".to_string()),
			active_account: Some("bob@example.com".to_string()),
			..Default::default()
		};
		auth.upsert_credential(
			"openai",
			CredentialEntry::OAuth {
				label: "alice@example.com".to_string(),
				access_token: "sk-a".to_string(),
				refresh_token: "rt-a".to_string(),
				id_token_claims: IdTokenClaims::default(),
				last_refresh_unix_ms: 100,
			},
		);
		auth.upsert_credential(
			"openai",
			CredentialEntry::OAuth {
				label: "bob@example.com".to_string(),
				access_token: "sk-b".to_string(),
				refresh_token: "rt-b".to_string(),
				id_token_claims: IdTokenClaims::default(),
				last_refresh_unix_ms: 200,
			},
		);

		let entry = auth.credential_for("openai").unwrap();
		assert_eq!(entry.label(), "bob@example.com");
	}
}
