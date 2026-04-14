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

use std::io::{self, BufRead, Write};
use std::time::{SystemTime, UNIX_EPOCH};

use roku_agent_runtime::LoopEvent;
use roku_agent_runtime::RuntimeService;
use roku_common_types::{ConversationRole, ConversationTurn, RequestEnvelope, RequestId};

use crate::CommandError;
use crate::auth::{AuthStore, CredentialEntry};
use crate::conversation::compact_conversation_history;
use crate::input::{
	CommandEntry, InputReader, ReadlineResult, SelectionItem, SubCommandEntry, read_text_input,
	run_selection,
};
use crate::render;
use crate::runtime::{
	build_live_runtime_service_from_env, cli_approval_gate, load_oauth_client_id,
	next_cli_request_sequence, pipe_approval_gate,
};
use crate::session_store::SessionStore;
use crate::storage::LocalStorageLayout;

/// Options parsed from the `chat` subcommand arguments.
pub(crate) struct ChatOptions {
	pub session_id: String,
	pub pipe: bool,
	/// When true, all output goes to stdout as typed JSONL lines.
	pub json: bool,
}

/// Result of a single turn execution.
enum TurnResult {
	/// Agent completed. Contains the response message.
	Completed(String),
	/// Agent is awaiting user input. Contains the question message.
	AwaitingUser(String),
	/// Execution was cancelled by Ctrl+C.
	Cancelled,
}

/// Per-turn token counts captured from the LoopEvent stream.
#[derive(Default, Clone)]
struct TurnTokens {
	prompt: u64,
	output: u64,
	/// Model that served the request (from the last TokenUsage event).
	model_id: Option<String>,
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

// ---------------------------------------------------------------------------
// JSONL output helpers
// ---------------------------------------------------------------------------

/// A typed JSONL envelope wrapping an event or result line.
#[derive(serde::Serialize)]
struct JsonLine<T: serde::Serialize> {
	#[serde(rename = "type")]
	kind: &'static str,
	data: T,
}

/// Write a typed JSONL line to stdout.
fn write_jsonl_event(event: &LoopEvent) {
	if let Ok(json) = serde_json::to_string(&JsonLine {
		kind: "event",
		data: event,
	}) {
		let stdout = io::stdout();
		let mut handle = stdout.lock();
		let _ = writeln!(handle, "{json}");
		let _ = handle.flush();
	}
}

fn write_jsonl_result(resp: &PipeResponse) {
	if let Ok(json) = serde_json::to_string(&JsonLine {
		kind: "result",
		data: resp,
	}) {
		let stdout = io::stdout();
		let mut handle = stdout.lock();
		let _ = writeln!(handle, "{json}");
		let _ = handle.flush();
	}
}

// ---------------------------------------------------------------------------
// Interactive REPL mode
// ---------------------------------------------------------------------------

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

/// Handle a user turn in interactive mode, including AwaitingUser loops.
#[allow(clippy::too_many_arguments)]
fn handle_turn_interactive(
	rt: &tokio::runtime::Runtime,
	service: &RuntimeService,
	store: &SessionStore,
	session_id: &str,
	goal: String,
	conversation_history: &mut Vec<ConversationTurn>,
	reader: &mut InputReader,
	json_mode: bool,
	session_prompt_tokens: &mut u64,
	session_output_tokens: &mut u64,
	model_override: Option<&str>,
	thinking_effort: Option<&str>,
) {
	let (result, _request_id, turn_tokens, text_streamed) = dispatch_and_record(
		rt,
		service,
		store,
		session_id,
		goal,
		conversation_history,
		false,
		json_mode,
		model_override,
		thinking_effort,
	);
	// Accumulate session totals and display token usage after this turn.
	*session_prompt_tokens = session_prompt_tokens.saturating_add(turn_tokens.prompt);
	*session_output_tokens = session_output_tokens.saturating_add(turn_tokens.output);
	if !json_mode && (turn_tokens.prompt > 0 || turn_tokens.output > 0) {
		let turn_cost = (turn_tokens.prompt as f64 / 1_000_000.0) * 3.0
			+ (turn_tokens.output as f64 / 1_000_000.0) * 15.0;
		let session_total = session_prompt_tokens.saturating_add(*session_output_tokens);
		eprintln!(
			"{}",
			render::styled_token_info(
				turn_tokens.prompt,
				turn_tokens.output,
				turn_cost,
				session_total,
				turn_tokens.model_id.as_deref(),
			)
		);
	}

	let turn_model = turn_tokens.model_id.clone();
	let emit_response = |msg: &str, status: &str| {
		if json_mode {
			let resp = PipeResponse {
				ok: true,
				request_id: None,
				session_id: Some(session_id.to_string()),
				status: Some(status.to_string()),
				message: Some(msg.to_string()),
				error: None,
				suggestion: None,
				model: turn_model.clone(),
			};
			write_jsonl_result(&resp);
		}
		// In interactive mode, the streaming render task already emitted
		// the response text to stderr in real-time via StreamRenderer.
		// No final println! — that caused duplicate output.
	};
	match result {
		Ok(TurnResult::Completed(response)) => {
			// When the render task didn't stream any text (e.g. the LLM
			// produced only tool calls and no text deltas), print the
			// final response here so it isn't silently lost.
			if !json_mode && !text_streamed && !response.is_empty() {
				eprintln!("{}", render::render_final_response(&response));
			}
			emit_response(&response, "succeeded");
		}
		Ok(TurnResult::AwaitingUser(question)) => {
			if !json_mode && !text_streamed && !question.is_empty() {
				eprintln!("{}", render::render_final_response(&question));
			}
			emit_response(&question, "awaiting_user");

			loop {
				match reader.readline("roku(reply)> ") {
					ReadlineResult::Line(reply_line) => {
						let reply = reply_line.trim();
						if reply.is_empty() {
							continue;
						}
						reader.add_history_entry(reply);

						let (inner_result, _, inner_tokens, inner_streamed) = dispatch_and_record(
							rt,
							service,
							store,
							session_id,
							reply.to_string(),
							conversation_history,
							false,
							json_mode,
							model_override,
							thinking_effort,
						);
						*session_prompt_tokens =
							session_prompt_tokens.saturating_add(inner_tokens.prompt);
						*session_output_tokens =
							session_output_tokens.saturating_add(inner_tokens.output);
						if inner_tokens.prompt > 0 || inner_tokens.output > 0 {
							let cost = (inner_tokens.prompt as f64 / 1_000_000.0) * 3.0
								+ (inner_tokens.output as f64 / 1_000_000.0) * 15.0;
							let session_total =
								session_prompt_tokens.saturating_add(*session_output_tokens);
							eprintln!(
								"[tokens: {}/{}, cost: ~${cost:.4}] [session: {}]",
								inner_tokens.prompt, inner_tokens.output, session_total,
							);
						}
						match inner_result {
							Ok(TurnResult::Completed(response)) => {
								if !json_mode && !inner_streamed && !response.is_empty() {
									eprintln!("{}", render::render_final_response(&response));
								}
								emit_response(&response, "succeeded");
								break;
							}
							Ok(TurnResult::AwaitingUser(q)) => {
								if !json_mode && !inner_streamed && !q.is_empty() {
									eprintln!("{}", render::render_final_response(&q));
								}
								emit_response(&q, "awaiting_user");
							}
							Ok(TurnResult::Cancelled) => {
								eprintln!("[cancelled]");
								break;
							}
							Err(e) => {
								eprintln!("[error] {e}");
								break;
							}
						}
					}
					ReadlineResult::Interrupted | ReadlineResult::Eof => {
						if let Err(e) = service.clear_pending_loop(session_id) {
							eprintln!("[warn] failed to clear pending loop: {e}");
						}
						eprintln!("[cancelled] Discarded pending agent state.");
						break;
					}
				}
			}
		}
		Ok(TurnResult::Cancelled) => {
			eprintln!("[cancelled]");
		}
		Err(e) => {
			eprintln!("[error] {e}");
		}
	}
}

// ---------------------------------------------------------------------------
// Pipe mode
// ---------------------------------------------------------------------------

/// Standard JSON response written to stdout in pipe mode.
#[derive(serde::Serialize)]
struct PipeResponse {
	ok: bool,
	#[serde(skip_serializing_if = "Option::is_none")]
	request_id: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	session_id: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	status: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	message: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	error: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	suggestion: Option<String>,
	/// Model that served this request (populated from TokenUsage event).
	#[serde(skip_serializing_if = "Option::is_none")]
	model: Option<String>,
}

fn run_pipe(rt: &tokio::runtime::Runtime, options: ChatOptions) -> Result<(), CommandError> {
	let json_mode = options.json;
	let emit_resp = move |resp: &PipeResponse| {
		if json_mode {
			write_jsonl_result(resp);
		} else {
			write_json_stdout(resp);
		}
	};

	let service = match tokio::task::block_in_place(build_live_runtime_service_from_env) {
		Ok(s) => s,
		Err(e) => {
			let resp = PipeResponse {
				ok: false,
				request_id: None,
				session_id: Some(options.session_id.clone()),
				status: None,
				message: None,
				error: Some(e.to_string()),
				suggestion: Some(
					"Check OPENROUTER_API_KEY or run 'roku memory health'".to_string(),
				),
				model: None,
			};
			emit_resp(&resp);
			return Err(e);
		}
	};
	// Pipe mode: stdin is consumed by the message stream, so interactive
	// approval prompts are impossible. Install a gate that auto-approves
	// read-only tools and denies write-risk tools.
	let catalog = std::sync::Arc::new(service.resource_catalog().clone());
	let service = service.with_approval_gate(pipe_approval_gate(catalog));
	let store = session_store();

	let mut conversation_history: Vec<ConversationTurn> =
		store.load(&options.session_id).unwrap_or_else(|e| {
			let resp = PipeResponse {
				ok: false,
				request_id: None,
				session_id: Some(options.session_id.clone()),
				status: None,
				message: None,
				error: Some(format!("failed to load session history: {e}")),
				suggestion: None,
				model: None,
			};
			emit_resp(&resp);
			Vec::new()
		});

	let stdin = io::stdin();
	for line in stdin.lock().lines() {
		let line = match line {
			Ok(l) => l,
			Err(e) => {
				let resp = PipeResponse {
					ok: false,
					request_id: None,
					session_id: Some(options.session_id.clone()),
					status: None,
					message: None,
					error: Some(format!("stdin read error: {e}")),
					suggestion: None,
					model: None,
				};
				emit_resp(&resp);
				break;
			}
		};

		let trimmed = line.trim();
		if trimmed.is_empty() {
			let resp = PipeResponse {
				ok: false,
				request_id: None,
				session_id: Some(options.session_id.clone()),
				status: None,
				message: None,
				error: Some("empty input".to_string()),
				suggestion: None,
				model: None,
			};
			emit_resp(&resp);
			continue;
		}

		// Handle session commands in pipe mode.
		match trimmed {
			"/quit" | "/exit" => break,
			"/clear" => {
				conversation_history.clear();
				let _ = store.clear(&options.session_id);
				let _ = service.clear_pending_loop(&options.session_id);
				let resp = PipeResponse {
					ok: true,
					request_id: None,
					session_id: Some(options.session_id.clone()),
					status: Some("cleared".to_string()),
					message: Some("Conversation history cleared.".to_string()),
					error: None,
					suggestion: None,
					model: None,
				};
				emit_resp(&resp);
				continue;
			}
			_ => {}
		}

		let goal = trimmed.to_string();

		let json_mode = options.json;
		let (result, request_id, tokens, _text_streamed) = dispatch_and_record(
			rt,
			&service,
			&store,
			&options.session_id,
			goal,
			&mut conversation_history,
			true,
			json_mode,
			None,
			None,
		);
		let model = tokens.model_id;

		match result {
			Ok(TurnResult::Completed(message)) => {
				let resp = PipeResponse {
					ok: true,
					request_id: Some(request_id),
					session_id: Some(options.session_id.clone()),
					status: Some("succeeded".to_string()),
					message: Some(message),
					error: None,
					suggestion: None,
					model: model.clone(),
				};
				emit_resp(&resp);
			}
			Ok(TurnResult::AwaitingUser(message)) => {
				let resp = PipeResponse {
					ok: true,
					request_id: Some(request_id),
					session_id: Some(options.session_id.clone()),
					status: Some("awaiting_user".to_string()),
					message: Some(message),
					error: None,
					suggestion: None,
					model: model.clone(),
				};
				emit_resp(&resp);
			}
			Ok(TurnResult::Cancelled) => {
				let resp = PipeResponse {
					ok: true,
					request_id: Some(request_id),
					session_id: Some(options.session_id.clone()),
					status: Some("cancelled".to_string()),
					message: None,
					error: None,
					suggestion: None,
					model,
				};
				emit_resp(&resp);
			}
			Err(e) => {
				let resp = PipeResponse {
					ok: false,
					request_id: Some(request_id),
					session_id: Some(options.session_id.clone()),
					status: None,
					message: None,
					error: Some(e.to_string()),
					suggestion: None,
					model,
				};
				emit_resp(&resp);
			}
		}
	}

	Ok(())
}

fn write_json_stdout(resp: &PipeResponse) {
	if let Ok(json) = serde_json::to_string(resp) {
		let stdout = io::stdout();
		let mut handle = stdout.lock();
		let _ = writeln!(handle, "{json}");
		let _ = handle.flush();
	}
}

// ---------------------------------------------------------------------------
// Shared turn execution
// ---------------------------------------------------------------------------

/// Execute a turn and record both user message and assistant response in history.
/// Persists new turns to the session store. On cancel/error, history is not modified.
///
/// Returns `(Result<TurnResult>, request_id, TurnTokens, bool)` so callers can track
/// token usage and whether the render task streamed LLM text.
fn dispatch_and_record(
	rt: &tokio::runtime::Runtime,
	service: &RuntimeService,
	store: &SessionStore,
	session_id: &str,
	goal: String,
	conversation_history: &mut Vec<ConversationTurn>,
	pipe_mode: bool,
	json_mode: bool,
	model_override: Option<&str>,
	thinking_effort: Option<&str>,
) -> (Result<TurnResult, CommandError>, String, TurnTokens, bool) {
	let seq = next_cli_request_sequence();
	let request_id = format!("req-{seq}");

	let (turn_result, tokens, text_streamed) = match execute_turn(
		rt,
		service,
		session_id,
		goal.clone(),
		conversation_history,
		pipe_mode,
		json_mode,
		&request_id,
		model_override,
		thinking_effort,
	) {
		Ok(triple) => triple,
		Err(e) => return (Err(e), request_id, TurnTokens::default(), false),
	};
	let result = turn_result;

	match &result {
		TurnResult::Completed(response) | TurnResult::AwaitingUser(response) => {
			let user_turn = ConversationTurn {
				role: ConversationRole::User,
				content: goal,
				created_at_unix_ms: now_unix_ms(),
			};
			let assistant_turn = ConversationTurn {
				role: ConversationRole::Assistant,
				content: response.clone(),
				created_at_unix_ms: now_unix_ms(),
			};

			// Persist to disk first, then update in-memory.
			if let Err(e) = store.append(session_id, &[user_turn.clone(), assistant_turn.clone()]) {
				eprintln!("[warn] failed to persist turn: {e}");
			}

			conversation_history.push(user_turn);
			conversation_history.push(assistant_turn);
		}
		TurnResult::Cancelled => {}
	}

	(Ok(result), request_id, tokens, text_streamed)
}

/// Execute a single chat turn with Ctrl+C cancellation support.
///
/// Returns `Ok((TurnResult, TurnTokens, bool))` where `TurnTokens` holds the prompt/output
/// token counts and the bool indicates whether the render task streamed any LLM text.
fn execute_turn(
	rt: &tokio::runtime::Runtime,
	service: &RuntimeService,
	session_id: &str,
	goal: String,
	conversation_history: &[ConversationTurn],
	pipe_mode: bool,
	json_mode: bool,
	request_id: &str,
	model_override_arg: Option<&str>,
	thinking_effort_arg: Option<&str>,
) -> Result<(TurnResult, TurnTokens, bool), CommandError> {
	rt.block_on(async {
		let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<LoopEvent>();

		// Shared token accumulator: the render task writes, execute_turn reads after join.
		let captured_tokens = std::sync::Arc::new(std::sync::Mutex::new(TurnTokens::default()));
		let captured_tokens_task = captured_tokens.clone();

		// Tracks whether the render task streamed any LLM text to the terminal.
		// When false, the caller should print the response message directly.
		let text_was_streamed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
		let text_streamed_flag = text_was_streamed.clone();

		let render_task = if json_mode {
			// JSON mode: emit typed JSONL event lines to stdout + capture tokens.
			tokio::spawn(async move {
				while let Some(event) = rx.recv().await {
					if let LoopEvent::TokenUsage {
						prompt_tokens,
						output_tokens,
						model_id,
						..
					} = &event && let Ok(mut guard) = captured_tokens_task.lock()
					{
						guard.prompt = guard.prompt.saturating_add(*prompt_tokens);
						guard.output = guard.output.saturating_add(*output_tokens);
						if let Some(id) = model_id {
							guard.model_id = Some(id.clone());
						}
					}
					write_jsonl_event(&event);
				}
			})
		} else if pipe_mode {
			// Pipe mode (no --json): serialize all events as JSONL to stderr.
			tokio::spawn(async move {
				while let Some(event) = rx.recv().await {
					if let LoopEvent::TokenUsage {
						prompt_tokens,
						output_tokens,
						model_id,
						..
					} = &event && let Ok(mut guard) = captured_tokens_task.lock()
					{
						guard.prompt = guard.prompt.saturating_add(*prompt_tokens);
						guard.output = guard.output.saturating_add(*output_tokens);
						if let Some(id) = model_id {
							guard.model_id = Some(id.clone());
						}
					}
					if let Ok(json) = serde_json::to_string(&event) {
						eprintln!("{json}");
					}
				}
			})
		} else {
			// Interactive mode: styled event display with streaming markdown.
			// Raw mode is active (for Esc key detection), so all output uses
			// explicit \r\n instead of relying on terminal LF→CRLF translation.
			// Suppress stderr logs during execution to avoid raw-mode CRLF issues.
			tokio::spawn(async move {
				let _log_guard = crate::suppress_stderr_logs();
				let mut stream_renderer = crate::render::StreamRenderer::new();
				let start_time = std::time::Instant::now();
				let mut current_step: u32 = 0;
				let mut current_tool: Option<String> = None;
				let mut status_visible = false;
				let mut streaming_active = false;
				// When Some, a ToolStart line has been printed without a trailing
				// newline, waiting for the matching ToolEnd to complete the line.
				// Stores the tool name so we only inline-complete the correct tool.
				let mut pending_tool_name: Option<String> = None;
				// Timestamp of the last tool event. The timer tick suppresses
				// status redraws shortly after tool activity to avoid blank-line
				// artifacts when rapid tool events interleave with the 500ms tick.
				let mut last_tool_event = start_time;
				// Whether any text was printed since the last LlmDecisionComplete.
				// Controls whether a trailing \r\n is needed to separate text from
				// subsequent tool output.
				let mut had_text_output = false;

				// Helper: draw or refresh the status line.
				macro_rules! show_status {
					() => {
						if status_visible {
							eprint!("\r\x1b[K");
						}
						let s = crate::render::styled_working_status(
							current_step,
							current_tool.as_deref(),
							start_time.elapsed(),
						);
						eprint!("{s}");
						let _ = io::stderr().flush();
						status_visible = true;
					};
				}

				// Show initial status line.
				show_status!();

				// Timer-based status refresh: updates elapsed time and tool
				// name independently of events (like Codex's 32ms tick, but
				// at 500ms since we only show text, not a spinner animation).
				let mut tick = tokio::time::interval(std::time::Duration::from_millis(500));
				tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
				// Consume the immediate first tick.
				tick.tick().await;

				loop {
					tokio::select! {
						event = rx.recv() => {
							let Some(event) = event else { break };

							// Clear status before printing event output.
							if status_visible {
								eprint!("\r\x1b[K");
								status_visible = false;
							}

							// Close any pending ToolStart line before printing
							// other event output, unless the matching ToolEnd will
							// complete it inline.
							let is_matching_tool_end = if let LoopEvent::ToolEnd {
								ref tool_name, ..
							} = event
							{
								pending_tool_name.as_deref() == Some(tool_name.as_str())
							} else {
								false
							};
							if pending_tool_name.is_some() && !is_matching_tool_end {
								eprint!("\r\n");
								pending_tool_name = None;
							}

							// All events print their output and continue.
							// Status display is handled exclusively by the timer tick.
							match event {
								LoopEvent::ToolStart { step, tool_name, args_summary } => {
									if pending_tool_name.is_some() {
										eprint!("\r\n");
									}
									current_step = step;
									current_tool = Some(tool_name.clone());
									streaming_active = false;
									last_tool_event = std::time::Instant::now();
									let msg = crate::render::styled_tool_start(
										&tool_name, args_summary.as_deref(),
									);
									eprint!("{msg}");
									let _ = io::stderr().flush();
									pending_tool_name = Some(tool_name);
								}
								LoopEvent::ToolEnd { tool_name, elapsed_ms, result_summary, .. } => {
									current_tool = None;
									last_tool_event = std::time::Instant::now();
									if is_matching_tool_end {
										let suffix = crate::render::styled_tool_end_suffix(
											elapsed_ms, result_summary.as_deref(),
										);
										eprint!("{suffix}\r\n");
										pending_tool_name = None;
									} else {
										let msg = crate::render::styled_tool_end(
											&tool_name, elapsed_ms, result_summary.as_deref(),
										);
										eprint!("{msg}\r\n");
									}
								}
								LoopEvent::CompactTriggered { step, estimated_tokens } => {
									eprint!("[compact] step {step} triggered (~{estimated_tokens} tokens)\r\n");
								}
								LoopEvent::CompactComplete { elapsed_ms, llm_succeeded, .. } => {
									let method = if llm_succeeded { "LLM" } else { "mechanical" };
									eprint!("[compact] completed ({method}, {elapsed_ms}ms)\r\n");
								}
								LoopEvent::LlmTextDelta { text, .. } => {
									streaming_active = true;
									let rendered = stream_renderer.push(&text);
									if !rendered.is_empty() {
										had_text_output = true;
										text_streamed_flag.store(
											true,
											std::sync::atomic::Ordering::Relaxed,
										);
										crate::mark_streaming_output();
										eprint!("{}", rendered.replace('\n', "\r\n"));
										let _ = io::stderr().flush();
									}
								}
								LoopEvent::LlmDecisionComplete { .. } => {
									streaming_active = false;
									let remaining = stream_renderer.flush();
									if !remaining.is_empty() {
										had_text_output = true;
										crate::mark_streaming_output();
										eprint!("{}", remaining.replace('\n', "\r\n"));
									}
									// Only add newline when there was actual text to
									// separate from. Without this guard, every
									// LlmDecisionComplete creates a blank line before
									// the next tool output.
									if had_text_output {
										eprint!("\r\n");
										had_text_output = false;
									}
									let _ = io::stderr().flush();
								}
								LoopEvent::StepComplete { step } => {
									current_step = step;
									current_tool = None;
								}
								LoopEvent::TokenUsage { prompt_tokens, output_tokens, model_id, .. } => {
									if let Ok(mut guard) = captured_tokens_task.lock() {
										guard.prompt = guard.prompt.saturating_add(prompt_tokens);
										guard.output = guard.output.saturating_add(output_tokens);
										if let Some(id) = model_id {
											guard.model_id = Some(id);
										}
									}
								}
							}
						}
						_ = tick.tick() => {
							// Periodic refresh: update elapsed time in the status line.
							// Skip when streaming text is active (cursor is mid-line),
							// when a ToolStart line is pending (would be overwritten),
							// when the approval prompt is visible, or shortly after
							// a tool event (avoids blank-line artifacts from status
							// being shown and immediately cleared by the next tool).
							if !streaming_active
								&& pending_tool_name.is_none()
								&& !crate::is_approval_active()
								&& last_tool_event.elapsed()
									>= std::time::Duration::from_secs(1)
							{
								show_status!();
							}
						}
					}
				}

				// Clear status line on exit.
				if status_visible {
					eprint!("\r\x1b[K");
				}
				let _ = io::stderr().flush();
				drop(_log_guard);
			})
		};

		let request = RequestEnvelope {
			request_id: RequestId(request_id.to_string()),
			session_id: session_id.to_string(),
			goal,
			planning_mode_hint: None,
			conversation_history: conversation_history.to_vec(),
			model_override: model_override_arg.map(str::to_string),
			thinking_effort: thinking_effort_arg.map(str::to_string),
		};

		let execute_fut = async {
			service
				.execute_with_mode(request, roku_agent_runtime::RunMode::Normal, Some(&tx))
				.await
		};

		// Esc-to-cancel: spawn a key-polling task that enters raw mode and
		// watches for Esc. Raw mode is terminal-wide so the render task above
		// uses explicit \r\n for all output.
		let esc_cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
		let stop_polling = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
		let interactive = !pipe_mode && !json_mode;
		let key_poller = if interactive {
			let esc_flag = esc_cancelled.clone();
			let stop_flag = stop_polling.clone();
			Some(tokio::task::spawn_blocking(move || {
				let _ = crossterm::terminal::enable_raw_mode();
				loop {
					if stop_flag.load(std::sync::atomic::Ordering::Relaxed) {
						break;
					}
					// Pause event reading while the approval prompt is active.
					// The approval gate disables raw mode and reads stdin directly;
					// consuming events here would steal keystrokes.
					if crate::is_approval_active() {
						std::thread::sleep(std::time::Duration::from_millis(200));
						continue;
					}
					if crossterm::event::poll(std::time::Duration::from_millis(200))
						.unwrap_or(false) && let Ok(crossterm::event::Event::Key(key)) =
						crossterm::event::read()
						&& key.kind != crossterm::event::KeyEventKind::Release
					{
						// Esc or Ctrl+C both cancel. In raw mode, Ctrl+C arrives
						// as a key event instead of SIGINT.
						let is_esc = key.code == crossterm::event::KeyCode::Esc;
						let is_ctrl_c = key.code == crossterm::event::KeyCode::Char('c')
							&& key
								.modifiers
								.contains(crossterm::event::KeyModifiers::CONTROL);
						if is_esc || is_ctrl_c {
							esc_flag.store(true, std::sync::atomic::Ordering::Relaxed);
							break;
						}
					}
				}
				let _ = crossterm::terminal::disable_raw_mode();
			}))
		} else {
			None
		};

		let esc_flag = esc_cancelled.clone();
		let result = tokio::select! {
			response = execute_fut => {
				let response = response.map_err(CommandError::Runtime)?;
				drop(tx);
				render_task.await.ok();

				let tokens = captured_tokens.lock().map(|g| g.clone()).unwrap_or_default();
				let streamed = text_was_streamed.load(std::sync::atomic::Ordering::Relaxed);
				if let Ok(Some(_pending)) = service.pending_loop(session_id) {
					Ok((TurnResult::AwaitingUser(response.message), tokens, streamed))
				} else {
					Ok((TurnResult::Completed(response.message), tokens, streamed))
				}
			}
			_ = tokio::signal::ctrl_c() => {
				drop(tx);
				render_task.await.ok();
				let _ = service.clear_pending_loop(session_id);
				Ok((TurnResult::Cancelled, TurnTokens::default(), false))
			}
			_ = async {
				loop {
					if esc_flag.load(std::sync::atomic::Ordering::Relaxed) { break; }
					tokio::time::sleep(std::time::Duration::from_millis(100)).await;
				}
			} => {
				drop(tx);
				render_task.await.ok();
				let _ = service.clear_pending_loop(session_id);
				Ok((TurnResult::Cancelled, TurnTokens::default(), false))
			}
		};

		// Stop the key poller and ensure raw mode is disabled.
		stop_polling.store(true, std::sync::atomic::Ordering::Relaxed);
		if let Some(poller) = key_poller {
			let _ = poller.await;
		}

		result
	})
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn session_store() -> SessionStore {
	let layout = LocalStorageLayout::from_env();
	SessionStore::new(layout.session_history_dir)
}

/// Atomically rewrite the session file with the given history (used after compaction).
///
/// Writes to a temporary sibling file first, then renames over the original, so a
/// crash mid-write does not destroy the existing file.
fn rewrite_history(
	store: &SessionStore,
	session_id: &str,
	history: &[ConversationTurn],
) -> Result<(), std::io::Error> {
	use std::io::Write;

	let layout = LocalStorageLayout::from_env();
	let dir = &layout.session_history_dir;
	std::fs::create_dir_all(dir)?;

	let target = dir.join(format!("{session_id}.jsonl"));
	let tmp = dir.join(format!("{session_id}.jsonl.tmp"));

	// Write to temp file.
	{
		let mut file = std::fs::File::create(&tmp)?;
		for turn in history {
			let json = serde_json::to_string(turn)
				.map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
			writeln!(file, "{json}")?;
		}
		file.flush()?;
	}

	// Atomic rename over original.
	std::fs::rename(&tmp, &target)?;

	// Suppress unused-variable warning — store is still needed for other operations,
	// but rewrite bypasses it for atomicity.
	let _ = store;

	Ok(())
}

fn now_unix_ms() -> u64 {
	SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.map(|d| d.as_millis().min(u64::MAX as u128) as u64)
		.unwrap_or(0)
}

fn print_banner(session_id: &str, turn_count: usize) {
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

fn print_help() {
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

// ---------------------------------------------------------------------------
// Auth helpers (Unit 02 + 03)
// ---------------------------------------------------------------------------

/// Check whether the system has no credentials at all (no env vars, no auth.json).
/// Used to distinguish "missing credentials" from other bootstrap failures.
fn has_no_credentials() -> bool {
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
fn print_auth_status() {
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
fn credential_summary(entry: &CredentialEntry) -> String {
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

/// Rebuild `RuntimeService` from environment + auth.json.
fn rebuild_service() -> Result<RuntimeService, CommandError> {
	let s = tokio::task::block_in_place(build_live_runtime_service_from_env)?;
	let catalog = std::sync::Arc::new(s.resource_catalog().clone());
	Ok(s.with_approval_gate(cli_approval_gate(catalog)))
}

/// First-time setup: choose provider and authenticate.
async fn run_first_time_setup() -> Result<(), String> {
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

/// Handle /session subcommands (list, switch, new).
fn handle_session_command(
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
fn interactive_session_switch(
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
fn switch_to_session(
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

/// Format a unix-ms timestamp as a human-readable relative age.
fn format_age(unix_ms: u64) -> String {
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

/// Available slash commands with descriptions for the popup.
fn slash_commands() -> Vec<CommandEntry> {
	vec![
		CommandEntry {
			name: "approve",
			description: "Toggle auto-approve for tool execution",
			sub_commands: None,
		},
		CommandEntry {
			name: "clear",
			description: "Clear conversation history",
			sub_commands: None,
		},
		CommandEntry {
			name: "compact",
			description: "Compact conversation history",
			sub_commands: None,
		},
		CommandEntry {
			name: "debug",
			description: "Toggle debug log output",
			sub_commands: None,
		},
		CommandEntry {
			name: "exit",
			description: "Exit the REPL",
			sub_commands: None,
		},
		CommandEntry {
			name: "help",
			description: "Show available commands",
			sub_commands: None,
		},
		CommandEntry {
			name: "login",
			description: "Sign in to a provider",
			sub_commands: None,
		},
		CommandEntry {
			name: "model",
			description: "Select LLM model",
			sub_commands: None,
		},
		CommandEntry {
			name: "logout",
			description: "Sign out current provider",
			sub_commands: None,
		},
		CommandEntry {
			name: "session",
			description: "Manage chat sessions",
			sub_commands: Some(vec![
				SubCommandEntry {
					name: "list",
					description: "List all sessions",
				},
				SubCommandEntry {
					name: "switch",
					description: "Switch to a different session",
				},
				SubCommandEntry {
					name: "new",
					description: "Create a new session",
				},
			]),
		},
		CommandEntry {
			name: "switch",
			description: "Switch LLM provider",
			sub_commands: None,
		},
		CommandEntry {
			name: "thinking",
			description: "Set thinking/reasoning effort",
			sub_commands: None,
		},
	]
}

/// Resolve the readline history file path.
fn readline_history_path() -> std::path::PathBuf {
	let layout = LocalStorageLayout::from_env();
	let dir = layout.session_history_dir;
	dir.join(".readline_history")
}

/// Handle /switch command — list stored credentials, pick one.
/// Returns `true` if the switch succeeded and the service was rebuilt.
fn handle_switch_command(service: &mut RuntimeService) -> bool {
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
