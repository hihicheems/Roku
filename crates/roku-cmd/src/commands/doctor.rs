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

//! /doctor command: diagnose environment issues.

use std::process::Command;

use crate::auth::AuthStore;
use crate::display::credential_summary;
use crate::storage::LocalStorageLayout;

/// Run all diagnostic checks and print results to stderr.
pub(crate) fn handle_doctor_command() {
	eprintln!("[doctor] Running diagnostics...\n");

	check_auth();
	check_config();
	check_tools();

	eprintln!("\n[doctor] Done.");
}

fn check_auth() {
	eprintln!("  Auth:");
	let store = AuthStore::from_env();
	match store.load() {
		Ok(Some(auth)) => {
			if !auth.has_any_credential() {
				eprintln!("    ✗ No credentials stored. Use /login to sign in.");
			} else {
				for (provider, entries) in &auth.credentials {
					for entry in entries {
						let summary = credential_summary(entry);
						let is_active = auth.active_provider.as_deref() == Some(provider.as_str())
							&& auth
								.credential_for(provider)
								.is_some_and(|ae| ae.label() == entry.label());
						let active = if is_active { " (active)" } else { "" };
						eprintln!("    ✓ {provider} / {}: {summary}{active}", entry.label());
					}
				}
			}
			if auth.active_provider.is_none() && auth.has_any_credential() {
				eprintln!("    ! No active provider set. Use /provider to select one.");
			}
		}
		Ok(None) => {
			eprintln!("    ✗ No auth.json found. Use /login to sign in.");
		}
		Err(e) => {
			eprintln!("    ✗ Failed to read auth.json: {e}");
		}
	}

	// Check env vars.
	let env_keys = [
		("OPENROUTER_API_KEY", "openrouter"),
		("ROKU_OPENAI_API_KEY", "openai"),
		("ROKU_ANTHROPIC_API_KEY", "anthropic"),
	];
	for (var, provider) in &env_keys {
		if std::env::var(var)
			.ok()
			.is_some_and(|v| !v.trim().is_empty())
		{
			eprintln!("    ✓ {provider} (env: {var})");
		}
	}
}

fn check_config() {
	eprintln!("  Config:");
	let layout = LocalStorageLayout::from_env();

	// runtime.toml
	if layout.runtime_config_path.exists() {
		match std::fs::read_to_string(&layout.runtime_config_path) {
			Ok(contents) => match contents.parse::<toml::Value>() {
				Ok(_) => eprintln!(
					"    ✓ runtime.toml ({})",
					layout.runtime_config_path.display()
				),
				Err(e) => eprintln!("    ✗ runtime.toml parse error: {e}"),
			},
			Err(e) => eprintln!("    ✗ runtime.toml read error: {e}"),
		}
	} else {
		eprintln!(
			"    - runtime.toml not found ({})",
			layout.runtime_config_path.display()
		);
	}

	// tools.toml
	if layout.tool_config_path.exists() {
		eprintln!("    ✓ tools.toml ({})", layout.tool_config_path.display());
	} else {
		eprintln!(
			"    - tools.toml not found ({})",
			layout.tool_config_path.display()
		);
	}

	// Data directories.
	if layout.home_dir.exists() {
		eprintln!("    ✓ ROKU_HOME ({})", layout.home_dir.display());
	} else {
		eprintln!("    ✗ ROKU_HOME missing ({})", layout.home_dir.display());
	}
}

fn check_tools() {
	eprintln!("  Tools:");
	let tools: &[(&str, &[&str])] = &[("git", &["--version"]), ("sh", &["-c", "echo ok"])];
	for (name, args) in tools {
		match Command::new(name).args(*args).output() {
			Ok(output) if output.status.success() => {
				let version = String::from_utf8_lossy(&output.stdout);
				let first_line = version.lines().next().unwrap_or("ok");
				eprintln!("    ✓ {name}: {first_line}");
			}
			Ok(_) => eprintln!("    ✗ {name}: command failed"),
			Err(e) => eprintln!("    ✗ {name}: {e}"),
		}
	}
}
