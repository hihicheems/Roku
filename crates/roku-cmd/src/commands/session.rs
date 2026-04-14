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

//! /session subcommand handlers.

use std::time::{SystemTime, UNIX_EPOCH};

use roku_common_types::ConversationTurn;

use crate::input::{SelectionItem, run_selection};
use crate::session_store::SessionStore;
use crate::turn::now_unix_ms;

/// Handle /session subcommands (list, switch, new).
pub(crate) fn handle_session_command(
	input: &str,
	store: &SessionStore,
	session_id: &mut String,
	conversation_history: &mut Vec<ConversationTurn>,
) {
	let parts: Vec<&str> = input.split_whitespace().collect();
	let sub = parts.get(1).copied().unwrap_or("list");

	match sub {
		"list" => {
			interactive_session_switch(store, session_id, conversation_history);
		}
		"switch" => {
			if let Some(target) = parts.get(2) {
				switch_to_session(store, target, session_id, conversation_history);
			} else {
				interactive_session_switch(store, session_id, conversation_history);
			}
		}
		"new" => {
			let name = parts
				.get(2..)
				.map(|p| p.join("-"))
				.filter(|s| !s.is_empty());
			let new_id = name.unwrap_or_else(|| {
				format!(
					"session-{}",
					SystemTime::now()
						.duration_since(UNIX_EPOCH)
						.map(|d| d.as_millis())
						.unwrap_or(0)
				)
			});
			// Reject invalid IDs (path traversal, separators) and
			// existing sessions to prevent silent persistence failures.
			match store.load(&new_id) {
				Err(e) => {
					eprintln!("[session] Invalid session ID '{new_id}': {e}");
				}
				Ok(turns) if !turns.is_empty() => {
					eprintln!(
						"[session] Session '{new_id}' already exists. Use /session switch {new_id} instead."
					);
				}
				Ok(_) => {
					conversation_history.clear();
					eprintln!("[session] Created new session '{new_id}'.");
					*session_id = new_id;
				}
			}
		}
		_ => {
			eprintln!("[session] Unknown subcommand: {sub}. Available: list, switch, new");
		}
	}
}

/// Show an interactive session picker and switch to the selected session.
pub(crate) fn interactive_session_switch(
	store: &SessionStore,
	session_id: &mut String,
	conversation_history: &mut Vec<ConversationTurn>,
) {
	let sessions = match store.list() {
		Ok(s) if s.is_empty() => {
			eprintln!("[session] No sessions found.");
			return;
		}
		Ok(s) => s,
		Err(e) => {
			eprintln!("[session] Failed to list sessions: {e}");
			return;
		}
	};
	let items: Vec<SelectionItem> = sessions
		.iter()
		.map(|s| {
			let active = if s.session_id == session_id.as_str() {
				" (active)"
			} else {
				""
			};
			let age = format_age(s.last_modified);
			SelectionItem {
				label: format!("{}{active}", s.session_id),
				description: format!("{} turns, last active {age}", s.turn_count),
			}
		})
		.collect();
	if let Some(idx) = run_selection(items, "[session] Select a session to switch to:") {
		switch_to_session(
			store,
			&sessions[idx].session_id,
			session_id,
			conversation_history,
		);
	}
}

/// Switch to a specific session by ID.
pub(crate) fn switch_to_session(
	store: &SessionStore,
	target: &str,
	session_id: &mut String,
	conversation_history: &mut Vec<ConversationTurn>,
) {
	match store.load(target) {
		Ok(turns) => {
			*conversation_history = turns;
			*session_id = target.to_string();
			eprintln!(
				"[session] Switched to '{}' ({} turns loaded).",
				target,
				conversation_history.len()
			);
		}
		Err(e) => eprintln!("[session] Failed to load '{target}': {e}"),
	}
}

/// Handle /resume command: load a session with compact-boundary awareness.
///
/// - `/resume` (no args) → show recent sessions, select to resume
/// - `/resume {session_id}` → directly resume that session
pub(crate) fn handle_resume_command(
	input: &str,
	store: &SessionStore,
	session_id: &mut String,
	conversation_history: &mut Vec<ConversationTurn>,
) {
	let parts: Vec<&str> = input.split_whitespace().collect();
	match parts.get(1) {
		Some(target) => {
			resume_session(store, target, session_id, conversation_history);
		}
		None => {
			interactive_resume(store, session_id, conversation_history);
		}
	}
}

/// Show an interactive session picker for resuming.
fn interactive_resume(
	store: &SessionStore,
	session_id: &mut String,
	conversation_history: &mut Vec<ConversationTurn>,
) {
	let sessions = match store.list() {
		Ok(s) if s.is_empty() => {
			eprintln!("[resume] No sessions found.");
			return;
		}
		Ok(s) => s,
		Err(e) => {
			eprintln!("[resume] Failed to list sessions: {e}");
			return;
		}
	};
	let items: Vec<SelectionItem> = sessions
		.iter()
		.take(10)
		.map(|s| {
			let active = if s.session_id == session_id.as_str() {
				" (active)"
			} else {
				""
			};
			let age = format_age(s.last_modified);
			let summary = s.first_message.as_deref().unwrap_or("(empty)");
			SelectionItem {
				label: format!("{}{active}", s.session_id),
				description: format!("{} turns, {age} — {summary}", s.turn_count),
			}
		})
		.collect();
	if let Some(idx) = run_selection(items, "[resume] Select a session to resume:") {
		resume_session(
			store,
			&sessions[idx].session_id,
			session_id,
			conversation_history,
		);
	}
}

/// Resume a specific session using compact-boundary-aware loading.
fn resume_session(
	store: &SessionStore,
	target: &str,
	session_id: &mut String,
	conversation_history: &mut Vec<ConversationTurn>,
) {
	match store.load_after_boundary(target) {
		Ok(turns) => {
			let count = turns.len();
			*conversation_history = turns;
			*session_id = target.to_string();
			eprintln!("[resume] Resumed '{target}' ({count} turns loaded).");
		}
		Err(e) => eprintln!("[resume] Failed to load '{target}': {e}"),
	}
}

/// Format a unix-ms timestamp as a human-readable relative age.
pub(crate) fn format_age(unix_ms: u64) -> String {
	let now = now_unix_ms();
	if unix_ms == 0 || now < unix_ms {
		return "unknown".to_string();
	}
	let secs = (now - unix_ms) / 1000;
	if secs < 60 {
		"just now".to_string()
	} else if secs < 3600 {
		format!("{}m ago", secs / 60)
	} else if secs < 86400 {
		format!("{}h ago", secs / 3600)
	} else {
		format!("{}d ago", secs / 86400)
	}
}
