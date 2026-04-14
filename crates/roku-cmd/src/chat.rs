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

//! Interactive REPL and pipe mode for `roku chat`.
//!
//! Maintains conversation context across turns via `conversation_history` in each
//! `RequestEnvelope`. History is persisted to disk as JSONL per session_id.
//!
//! ## Modes
//!
//! - **Interactive** (default): custom crossterm REPL with command popup, human-friendly
//!   output to stderr, response text to stdout. Session history loads on start and appends
//!   after each turn.
//! - **Pipe** (`--pipe`): stdin lines as user turns, JSON responses to stdout, LoopEvent
//!   JSONL to stderr. No prompt, no banner, no decorative output on stdout.

use std::io;

use roku_common_types::ConversationTurn;

use crate::CommandError;
use crate::auth::AuthStore;
use crate::commands::session::handle_session_command;
use crate::commands::setup::{handle_switch_command, rebuild_service, run_first_time_setup};
use crate::commands::slash_commands;
use crate::conversation::compact_conversation_history;
use crate::display::{has_no_credentials, print_auth_status, print_banner, print_help};
use crate::input::{InputReader, ReadlineResult, SelectionItem, run_selection};
use crate::pipe::run_pipe;
use crate::runtime::build_live_runtime_service_from_env;
use crate::runtime::cli_approval_gate;
use crate::session_store::{SessionStore, rewrite_history};
use crate::storage::LocalStorageLayout;
use crate::turn::handle_turn_interactive;

/// Options parsed from the `chat` subcommand arguments.
pub(crate) struct ChatOptions {
	pub session_id: String,
	pub pipe: bool,
	/// When true, all output goes to stdout as typed JSONL lines.
	pub json: bool,
}

/// Runs the chat in the appropriate mode.
pub(crate) fn run_chat(
	rt: &tokio::runtime::Runtime,
	options: ChatOptions,
) -> Result<(), CommandError> {
	if options.pipe {
		run_pipe(rt, options)
	} else {
		run_interactive(rt, options)
	}
}

fn session_store() -> SessionStore {
	let layout = LocalStorageLayout::from_env();
	SessionStore::new(layout.session_history_dir)
}

/// Resolve the readline history file path.
fn readline_history_path() -> std::path::PathBuf {
	let layout = LocalStorageLayout::from_env();
	let dir = layout.session_history_dir;
	dir.join(".readline_history")
}

fn run_interactive(rt: &tokio::runtime::Runtime, options: ChatOptions) -> Result<(), CommandError> {
	// Check for credentials BEFORE bootstrap. The bootstrap function falls back
	// to deterministic mode (returns Ok) when credentials are missing, so we
	// cannot detect "no credentials" from its return value.
	if has_no_credentials() {
		eprintln!("No API key or OAuth token found. Starting first-time setup...\n");
		if let Err(e) = rt.block_on(run_first_time_setup()) {
			return Err(CommandError::Io(io::Error::other(format!(
				"first-time setup failed: {e}"
			))));
		}
	}

	let mut service = {
		let s = tokio::task::block_in_place(build_live_runtime_service_from_env)?;
		let catalog = std::sync::Arc::new(s.resource_catalog().clone());
		s.with_approval_gate(cli_approval_gate(catalog))
	};
	let store = session_store();

	let history_path = readline_history_path();
	let mut reader = InputReader::new(slash_commands(), history_path);

	// Mutable session ID — updated by /session switch and /session new.
	let mut session_id = options.session_id.clone();

	// Load persisted conversation history.
	let mut conversation_history: Vec<ConversationTurn> =
		store.load(&session_id).unwrap_or_else(|e| {
			eprintln!("[warn] failed to load session history: {e}");
			Vec::new()
		});

	print_banner(&session_id, conversation_history.len());
	print_auth_status();

	// Tracks whether the user has logged out in this session.
	let mut logged_out = false;

	// Session-level model/thinking overrides.
	let mut model_override: Option<String> = None;
	let mut thinking_effort: Option<String> = None;

	// Session-level cumulative token counters.
	let mut session_prompt_tokens: u64 = 0;
	let mut session_output_tokens: u64 = 0;

	while let ReadlineResult::Line(line) = reader.readline("roku> ") {
		let trimmed = line.trim();
		if trimmed.is_empty() {
			continue;
		}

		match trimmed {
			"/quit" | "/exit" => break,
			"/help" => {
				reader.add_history_entry(trimmed);
				print_help();
				continue;
			}
			"/approve" => {
				reader.add_history_entry(trimmed);
				let enabled = crate::toggle_auto_approve();
				if enabled {
					eprintln!(
						"[approve] Auto-approve enabled. Tools will execute without prompting."
					);
				} else {
					eprintln!("[approve] Auto-approve disabled. Tools will prompt for approval.");
				}
				continue;
			}
			"/clear" => {
				reader.add_history_entry(trimmed);
				conversation_history.clear();
				if let Err(e) = store.clear(&session_id) {
					eprintln!("[warn] failed to clear session file: {e}");
				}
				if let Err(e) = service.clear_pending_loop(&session_id) {
					eprintln!("[warn] failed to clear pending loop: {e}");
				}
				eprintln!("[clear] Conversation history and pending state cleared.");
				continue;
			}
			"/debug" => {
				reader.add_history_entry(trimmed);
				let enabled = crate::toggle_debug_logs();
				if enabled {
					eprintln!("[debug] Debug logging enabled.");
				} else {
					eprintln!("[debug] Debug logging disabled.");
				}
				continue;
			}
			"/compact" => {
				reader.add_history_entry(trimmed);
				match compact_conversation_history(&mut conversation_history) {
					Some(result) => {
						if let Err(e) = rewrite_history(&store, &session_id, &conversation_history)
						{
							eprintln!("[warn] failed to persist compacted history: {e}");
						}
						// Write compact boundary marker to session store.
						let boundary = crate::session_store::SessionEntry::CompactBoundary {
							timestamp_ms: crate::turn::now_unix_ms(),
							summary_turn_index: 0,
							discarded_turns: result.discarded,
							retained_turns: result.retained,
						};
						if let Err(e) = store.append_entries(&session_id, &[boundary]) {
							eprintln!("[warn] failed to write compact boundary: {e}");
						}
						eprintln!(
							"[compact] Compacted {} turns into summary. {} turns remain.",
							result.discarded, result.retained
						);
					}
					None => eprintln!(
						"[compact] History has {} turns, nothing to compact.",
						conversation_history.len()
					),
				}
				continue;
			}
			"/login" => {
				reader.add_history_entry(trimmed);
				match rt.block_on(run_first_time_setup()) {
					Ok(()) => match rebuild_service() {
						Ok(s) => {
							service = s;
							logged_out = false;
							eprintln!("[login] Service rebuilt with new credentials.");
							print_auth_status();
						}
						Err(e) => eprintln!("[login] Failed to rebuild service: {e}"),
					},
					Err(e) => eprintln!("[login] {e}"),
				}
				continue;
			}
			"/logout" => {
				reader.add_history_entry(trimmed);
				let auth_store = AuthStore::from_env();
				if let Ok(Some(auth)) = auth_store.load() {
					if let Some(provider) = auth.active_provider.as_deref() {
						if let Err(e) = auth_store.delete_credential(provider) {
							eprintln!("[logout] Failed to clear credentials: {e}");
						} else {
							logged_out = true;
							eprintln!(
								"[logout] Credentials cleared for {provider}. Use /login to sign in again."
							);
						}
					} else if !auth.credentials.is_empty() {
						let providers: Vec<String> = auth.credentials.keys().cloned().collect();
						for p in &providers {
							let _ = auth_store.delete_credential(p);
						}
						logged_out = true;
						eprintln!(
							"[logout] Cleared {} stored credential(s). Use /login to sign in again.",
							providers.len()
						);
					} else {
						eprintln!("[logout] No credentials found.");
					}
				} else {
					eprintln!("[logout] No credentials found.");
				}
				continue;
			}
			"/model" => {
				reader.add_history_entry(trimmed);
				let models = service.available_models();
				if models.is_empty() {
					eprintln!("[model] No models available.");
				} else {
					let mut items: Vec<SelectionItem> = vec![SelectionItem {
						label: "(default)".to_string(),
						description: "Use the configured default model".to_string(),
					}];
					items.extend(models.iter().map(|m| SelectionItem {
						label: m.clone(),
						description: String::new(),
					}));
					if let Some(idx) = run_selection(items, "Select model:") {
						if idx == 0 {
							model_override = None;
							eprintln!("[model] Using default model.");
						} else {
							let model_id = models[idx - 1].clone();
							eprintln!("[model] Switched to {model_id}.");
							model_override = Some(model_id);
						}
					}
				}
				continue;
			}
			"/thinking" => {
				reader.add_history_entry(trimmed);
				let items = vec![
					SelectionItem {
						label: "none".to_string(),
						description: "No reasoning/thinking".to_string(),
					},
					SelectionItem {
						label: "low".to_string(),
						description: "Minimal reasoning".to_string(),
					},
					SelectionItem {
						label: "medium".to_string(),
						description: "Moderate reasoning".to_string(),
					},
					SelectionItem {
						label: "high".to_string(),
						description: "Maximum reasoning depth".to_string(),
					},
				];
				if let Some(idx) = run_selection(items, "Select thinking effort:") {
					let effort = match idx {
						1 => "low",
						2 => "medium",
						3 => "high",
						_ => "none",
					};
					eprintln!("[thinking] Set to {effort}.");
					thinking_effort = Some(effort.to_string());
				}
				continue;
			}
			"/switch" => {
				reader.add_history_entry(trimmed);
				if handle_switch_command(&mut service) {
					logged_out = false;
				}
				continue;
			}
			input if input.starts_with("/session") => {
				reader.add_history_entry(trimmed);
				handle_session_command(input, &store, &mut session_id, &mut conversation_history);
				continue;
			}
			input if input.starts_with('/') => {
				eprintln!("Unknown command: {input}. Type /help for available commands.");
				continue;
			}
			_ => {
				reader.add_history_entry(trimmed);
			}
		}

		if logged_out {
			eprintln!("Not authenticated. Use /login to sign in.");
			continue;
		}

		let goal = trimmed.to_string();
		handle_turn_interactive(
			rt,
			&service,
			&store,
			&session_id,
			goal,
			&mut conversation_history,
			&mut reader,
			options.json,
			&mut session_prompt_tokens,
			&mut session_output_tokens,
			model_override.as_deref(),
			thinking_effort.as_deref(),
		);
	}

	reader.save_history();

	Ok(())
}
