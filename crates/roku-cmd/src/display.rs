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
	eprintln!("  /login             Sign in with a new provider or account");
	eprintln!("  /logout            Clear current credentials");
	eprintln!("  /switch            Switch between stored credentials");
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

/// Display the current authentication status.
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
	let detail = auth
		.credentials
		.get(provider)
		.map(credential_summary)
		.unwrap_or_default();
	if detail.is_empty() {
		eprintln!("[auth] provider: {provider}");
	} else {
		eprintln!("[auth] provider: {provider} ({detail})");
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
