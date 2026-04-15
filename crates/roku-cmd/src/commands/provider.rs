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

//! /provider command: list and switch LLM providers.

use roku_agent_runtime::RuntimeService;

use crate::auth::{AuthFile, AuthStore};
use crate::commands::setup::rebuild_service;
use crate::display::credential_summary;
use crate::runtime_config::load_runtime_configs;
use crate::storage::LocalStorageLayout;

/// Returns the provider name that `runtime.toml` pins explicitly, if any.
///
/// When `[runtime.llm].provider` is set, `build_live_runtime` ignores
/// `auth.active_provider` (see `crates/roku-cmd/src/runtime.rs` provider
/// selection). `/provider` must surface this to avoid misreporting.
fn runtime_explicit_provider() -> Option<&'static str> {
	let layout = LocalStorageLayout::from_env();
	let configs = load_runtime_configs(&layout).ok()?;
	configs
		.llm_provider_explicit
		.then(|| configs.llm_provider.as_str())
}

/// Handle /provider command.
///
/// - `/provider` → list all providers with credentials
/// - `/provider {name}` → switch to that provider
pub(crate) fn handle_provider_command(
	input: &str,
	service: &mut RuntimeService,
	logged_out: &mut bool,
) {
	let parts: Vec<&str> = input.split_whitespace().collect();
	match parts.get(1) {
		None => list_providers(),
		Some(name) => switch_provider(name, service, logged_out),
	}
}

fn list_providers() {
	let store = AuthStore::from_env();
	// Fall through with a default state when `auth.json` is absent so env-var-only
	// setups still see their providers listed (otherwise the env-probe below
	// never runs and users get a misleading "no credentials" message).
	let auth = match store.load() {
		Ok(Some(a)) => a,
		Ok(None) => AuthFile::default(),
		Err(e) => {
			eprintln!("[provider] Failed to read auth store: {e}");
			return;
		}
	};

	let runtime_override = runtime_explicit_provider();
	let active = runtime_override
		.or(auth.active_provider.as_deref())
		.unwrap_or("none");

	eprintln!("[provider] Available providers:");
	if auth.credentials.is_empty() {
		eprintln!("  (none)");
	}
	for (name, entry) in &auth.credentials {
		let summary = credential_summary(entry);
		let marker = if name == active { " ← active" } else { "" };
		eprintln!("  {name}: {summary}{marker}");
	}

	// Show env var providers.
	let env_providers = [
		("OPENROUTER_API_KEY", "openrouter"),
		("ROKU_OPENAI_API_KEY", "openai"),
		("ROKU_ANTHROPIC_API_KEY", "anthropic"),
	];
	for (var, provider) in &env_providers {
		if std::env::var(var)
			.ok()
			.is_some_and(|v| !v.trim().is_empty())
			&& !auth.credentials.contains_key(*provider)
		{
			let marker = if *provider == active {
				" ← active"
			} else {
				""
			};
			eprintln!("  {provider}: (env: {var}){marker}");
		}
	}

	eprintln!(
		"\n  Selection reason: {}",
		selection_reason(runtime_override.is_some(), active)
	);
	eprintln!("  Use /provider {{name}} to switch.");
}

fn switch_provider(name: &str, service: &mut RuntimeService, logged_out: &mut bool) {
	// `build_live_runtime` ignores `auth.active_provider` when `runtime.toml`
	// sets `[runtime.llm].provider` explicitly. Writing to `auth.json` in that
	// case would be misleading — the live runtime would keep using the
	// runtime.toml provider while `/provider` reports success. Refuse early
	// and point the user at the actual authority.
	if let Some(pinned) = runtime_explicit_provider() {
		eprintln!(
			"[provider] Cannot switch: runtime.toml [runtime.llm].provider is \
			 explicitly set to {pinned}."
		);
		eprintln!("  Edit runtime.toml (or remove the explicit setting) instead.");
		return;
	}

	let store = AuthStore::from_env();
	// If `auth.json` is absent, fall through with a default state so env-var-only
	// setups can switch provider without requiring /login first. The save below
	// will create the file with the new active_provider.
	let mut auth = match store.load() {
		Ok(Some(a)) => a,
		Ok(None) => AuthFile::default(),
		Err(e) => {
			eprintln!("[provider] Failed to read auth store: {e}");
			return;
		}
	};

	// Validate: either stored credential or env var.
	let valid = auth.credentials.contains_key(name)
		|| match name {
			"openrouter" => std::env::var("OPENROUTER_API_KEY")
				.ok()
				.is_some_and(|v| !v.trim().is_empty()),
			"openai" => std::env::var("ROKU_OPENAI_API_KEY")
				.ok()
				.is_some_and(|v| !v.trim().is_empty()),
			"anthropic" => std::env::var("ROKU_ANTHROPIC_API_KEY")
				.ok()
				.is_some_and(|v| !v.trim().is_empty()),
			_ => false,
		};

	if !valid {
		eprintln!("[provider] Unknown or unconfigured provider: {name}");
		eprintln!("  Available: openrouter, openai, anthropic");
		return;
	}

	// Save the previous provider so we can roll back if rebuild fails.
	let previous_provider = auth.active_provider.clone();
	auth.active_provider = Some(name.to_string());
	if let Err(e) = store.save(&auth) {
		eprintln!("[provider] Failed to save auth store: {e}");
		return;
	}

	match rebuild_service() {
		Ok(s) => {
			*service = s;
			*logged_out = false;
			eprintln!("[provider] Switched to {name}. Service rebuilt.");
		}
		Err(e) => {
			eprintln!("[provider] Service rebuild failed: {e}");
			// Roll back auth.json to previous provider.
			auth.active_provider = previous_provider;
			if let Err(rollback_err) = store.save(&auth) {
				eprintln!("[provider] Failed to roll back auth store: {rollback_err}");
			} else {
				eprintln!("[provider] Rolled back to previous provider.");
			}
		}
	}
}

fn selection_reason(runtime_explicit: bool, active: &str) -> &'static str {
	if runtime_explicit {
		"runtime.toml [runtime.llm].provider (overrides auth.json)"
	} else if active == "none" {
		"no active provider set"
	} else {
		"auth.json active_provider"
	}
}
