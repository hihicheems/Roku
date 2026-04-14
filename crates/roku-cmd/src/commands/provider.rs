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

use crate::auth::AuthStore;
use crate::commands::setup::rebuild_service;
use crate::display::credential_summary;

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
	let auth = match store.load() {
		Ok(Some(a)) => a,
		Ok(None) => {
			eprintln!("[provider] No credentials found. Use /login to sign in.");
			return;
		}
		Err(e) => {
			eprintln!("[provider] Failed to read auth store: {e}");
			return;
		}
	};

	let active = auth.active_provider.as_deref().unwrap_or("none");

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
			eprintln!("  {provider}: (env: {var})");
		}
	}

	eprintln!("\n  Selection reason: {}", selection_reason(active));
	eprintln!("  Use /provider {{name}} to switch.");
}

fn switch_provider(name: &str, service: &mut RuntimeService, logged_out: &mut bool) {
	let store = AuthStore::from_env();
	let mut auth = match store.load() {
		Ok(Some(a)) => a,
		Ok(None) => {
			eprintln!("[provider] No auth store. Use /login first.");
			return;
		}
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

fn selection_reason(active: &str) -> &'static str {
	if active == "none" {
		"no active provider set"
	} else {
		"auth.json active_provider"
	}
}
