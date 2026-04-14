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

//! Static display functions: banner, help, auth status.

use crate::auth::{AuthStore, CredentialEntry};
use crate::render;

pub(crate) fn print_banner(session_id: &str, turn_count: usize) {
	eprintln!("{}", render::styled_banner("Roku interactive chat"));
	if turn_count > 0 {
		eprintln!(
			"Resuming session '{}' ({} turns loaded)",
			session_id, turn_count
		);
	} else {
		eprintln!("Session: {session_id}");
	}
	eprintln!("Type a message to start. /help for commands, /quit to exit.");
	eprintln!();
}

pub(crate) fn print_help() {
	eprintln!("Commands:");
	eprintln!("  /help              Show this help message");
	eprintln!("  /clear             Reset conversation history and pending state");
	eprintln!("  /compact           Compress older conversation turns into a summary");
	eprintln!("  /resume [ID]       Resume a previous session (compact-aware)");
	eprintln!("  /login             Sign in with a new provider or account");
	eprintln!("  /logout            Clear current credentials");
	eprintln!("  /switch            Switch between stored credentials");
	eprintln!("  /provider [NAME]   List or switch LLM providers");
	eprintln!("  /doctor            Diagnose environment and connectivity");
	eprintln!("  /trace [RUN_ID]    Show execution trace");
	eprintln!("  /model             Select LLM model");
	eprintln!("  /thinking          Set thinking/reasoning effort");
	eprintln!("  /plan              Enter plan mode (read-only tools)");
	eprintln!("  /plan-execute      Exit plan mode");
	eprintln!("  /session list      List all sessions");
	eprintln!("  /session switch ID Switch to a different session");
	eprintln!("  /session new NAME  Create a new session");
	eprintln!("  /quit              Exit the REPL (also: /exit, Ctrl+C, Ctrl+D)");
	eprintln!();
	eprintln!("Tab completion is available for all commands.");
}

/// Check whether the system has no credentials at all (no env vars, no auth.json).
/// Used to distinguish "missing credentials" from other bootstrap failures.
pub(crate) fn has_no_credentials() -> bool {
	// Check env vars for any provider. Each must be checked independently
	// because an empty var (Ok("")) would short-circuit an or_else chain.
	let env_keys = [
		"OPENROUTER_API_KEY",
		"ROKU_OPENAI_API_KEY",
		"ROKU_ANTHROPIC_API_KEY",
	];
	let has_env_key = env_keys.iter().any(|key| {
		std::env::var(key)
			.ok()
			.is_some_and(|v| !v.trim().is_empty())
	});
	if has_env_key {
		return false;
	}
	// Check auth.json for any credential.
	let store = AuthStore::from_env();
	match store.load() {
		Ok(Some(auth)) => auth.credentials.is_empty(),
		_ => true,
	}
}

/// Display the current authentication status with provider, auth method, and model info.
pub(crate) fn print_auth_status() {
	let store = AuthStore::from_env();
	let auth = match store.load() {
		Ok(Some(a)) => a,
		_ => {
			eprintln!("[auth] Not authenticated. Use /login to sign in.");
			return;
		}
	};
	let provider = auth.active_provider.as_deref().unwrap_or("none");
	let auth_method = auth
		.credentials
		.get(provider)
		.map(auth_method_label)
		.unwrap_or_default();
	let detail = auth
		.credentials
		.get(provider)
		.map(credential_summary)
		.unwrap_or_default();

	let model_id = resolve_display_model();
	let mut parts = Vec::new();
	if !detail.is_empty() {
		parts.push(format!("provider: {provider} ({detail})"));
	} else {
		parts.push(format!("provider: {provider}"));
	}
	if !auth_method.is_empty() {
		parts.push(format!("auth: {auth_method}"));
	}
	if let Some(model) = &model_id {
		parts.push(format!("model: {model}"));
	}
	eprintln!("[auth] {}", parts.join(" | "));
}

/// Resolve the display model from runtime config (best-effort).
fn resolve_display_model() -> Option<String> {
	let layout = crate::storage::LocalStorageLayout::from_env();
	let contents = std::fs::read_to_string(&layout.runtime_config_path).ok()?;
	let config: toml::Value = contents.parse().ok()?;
	config
		.get("llm")
		.and_then(|llm| llm.get("model"))
		.and_then(|m| m.as_str())
		.map(|s| s.to_string())
}

/// Human-readable auth method label.
fn auth_method_label(entry: &CredentialEntry) -> String {
	match entry {
		CredentialEntry::ApiKey { .. } => "api_key".to_string(),
		CredentialEntry::OAuth { .. } => "oauth".to_string(),
	}
}

/// One-line summary of a credential entry (email or key prefix).
pub(crate) fn credential_summary(entry: &CredentialEntry) -> String {
	match entry {
		CredentialEntry::ApiKey { api_key } => {
			// Show only the last 4 characters to minimize key exposure.
			let suffix: String = api_key
				.chars()
				.rev()
				.take(4)
				.collect::<Vec<_>>()
				.into_iter()
				.rev()
				.collect();
			format!("...{suffix}")
		}
		CredentialEntry::OAuth {
			id_token_claims, ..
		} => id_token_claims
			.email
			.as_deref()
			.unwrap_or("oauth")
			.to_string(),
	}
}
