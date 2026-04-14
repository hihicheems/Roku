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

//! /login, /logout, /switch, first-time setup handlers.

use roku_agent_runtime::RuntimeService;

use crate::CommandError;
use crate::auth::{AuthStore, CredentialEntry};
use crate::display::{credential_summary, print_auth_status};
use crate::input::{SelectionItem, read_text_input, run_selection};
use crate::runtime::{
	build_live_runtime_service_from_env, cli_approval_gate, load_oauth_client_id,
};

/// Rebuild `RuntimeService` from environment + auth.json.
pub(crate) fn rebuild_service() -> Result<RuntimeService, CommandError> {
	let s = tokio::task::block_in_place(build_live_runtime_service_from_env)?;
	let catalog = std::sync::Arc::new(s.resource_catalog().clone());
	Ok(s.with_approval_gate(cli_approval_gate(catalog)))
}

/// First-time setup: choose provider and authenticate.
pub(crate) async fn run_first_time_setup() -> Result<(), String> {
	let auth_store = AuthStore::from_env();

	let items = vec![
		SelectionItem {
			label: "OpenRouter".into(),
			description: "API key".into(),
		},
		SelectionItem {
			label: "OpenAI".into(),
			description: "OAuth — opens browser".into(),
		},
	];
	let choice = match run_selection(items, "Choose a provider:") {
		Some(idx) => idx,
		None => return Err("setup cancelled".to_string()),
	};

	match choice {
		0 => {
			let key = match read_text_input("Enter your OpenRouter API key: ") {
				Some(k) if !k.trim().is_empty() => k.trim().to_string(),
				Some(_) => return Err("empty API key".to_string()),
				None => return Err("setup cancelled".to_string()),
			};
			let mut auth = auth_store.load().ok().flatten().unwrap_or_default();
			auth.active_provider = Some("openrouter".to_string());
			auth.credentials.insert(
				"openrouter".to_string(),
				CredentialEntry::ApiKey { api_key: key },
			);
			auth_store.save(&auth).map_err(|e| format!("save: {e}"))?;
			eprintln!("[setup] OpenRouter API key saved.");
			Ok(())
		}
		1 => {
			let client_id = load_oauth_client_id().ok_or_else(|| {
				"No oauth_client_id configured. Set OPENAI_OAUTH_CLIENT_ID env var \
                 or uncomment oauth_client_id in config/runtime.toml under [runtime.llm]."
					.to_string()
			})?;
			eprintln!("[setup] Opening browser for OpenAI authorization...");
			let result = crate::auth::run_openai_oauth(&client_id)
				.await
				.map_err(|e| format!("oauth: {e}"))?;
			let now_ms = std::time::SystemTime::now()
				.duration_since(std::time::UNIX_EPOCH)
				.map(|d| d.as_millis() as i64)
				.unwrap_or(0);
			let mut auth = auth_store.load().ok().flatten().unwrap_or_default();
			auth.active_provider = Some("openai".to_string());
			auth.credentials.insert(
				"openai".to_string(),
				CredentialEntry::OAuth {
					access_token: result.api_key,
					refresh_token: result.refresh_token,
					id_token_claims: result.id_token_claims,
					last_refresh_unix_ms: now_ms,
				},
			);
			auth_store.save(&auth).map_err(|e| format!("save: {e}"))?;
			eprintln!("[setup] OpenAI OAuth credentials saved.");
			Ok(())
		}
		other => unreachable!("unexpected provider index: {other}"),
	}
}

/// Handle /switch command — list stored credentials, pick one.
/// Returns `true` if the switch succeeded and the service was rebuilt.
pub(crate) fn handle_switch_command(service: &mut RuntimeService) -> bool {
	let auth_store = AuthStore::from_env();
	let auth = match auth_store.load() {
		Ok(Some(a)) if !a.credentials.is_empty() => a,
		_ => {
			eprintln!("[switch] No stored credentials. Use /login first.");
			return false;
		}
	};

	let mut providers: Vec<&String> = auth.credentials.keys().collect();
	providers.sort();
	if providers.len() < 2 && auth.active_provider.is_some() {
		eprintln!(
			"[switch] Only one credential stored ({}). Use /login to add another.",
			providers.first().map(|s| s.as_str()).unwrap_or("?")
		);
		return false;
	}
	if providers.is_empty() {
		eprintln!("[switch] No stored credentials. Use /login first.");
		return false;
	}

	let items: Vec<SelectionItem> = providers
		.iter()
		.map(|provider| {
			let active = if auth.active_provider.as_deref() == Some(provider.as_str()) {
				" (active)"
			} else {
				""
			};
			let summary = auth
				.credentials
				.get(*provider)
				.map(credential_summary)
				.unwrap_or_default();
			SelectionItem {
				label: format!("{provider}{active}"),
				description: summary,
			}
		})
		.collect();

	let idx = match run_selection(items, "[switch] Available providers:") {
		Some(i) => i,
		None => {
			eprintln!("[switch] Cancelled.");
			return false;
		}
	};

	let target = providers[idx].clone();
	let mut updated = auth.clone();
	updated.active_provider = Some(target.clone());
	if let Err(e) = auth_store.save(&updated) {
		eprintln!("[switch] Failed to update active provider: {e}");
		return false;
	}
	match rebuild_service() {
		Ok(s) => {
			*service = s;
			eprintln!("[switch] Switched to {target}.");
			print_auth_status();
			true
		}
		Err(e) => {
			eprintln!("[switch] Failed to rebuild service: {e}");
			let _ = auth_store.save(&auth);
			false
		}
	}
}
