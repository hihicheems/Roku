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
//! - **Interactive** (default): rustyline REPL, human-friendly output to stderr, response
//!   text to stdout. Session history loads on start and appends after each turn.
//! - **Pipe** (`--pipe`): stdin lines as user turns, JSON responses to stdout, LoopEvent
//!   JSONL to stderr. No prompt, no banner, no decorative output on stdout.

use std::io::{self, BufRead, Write};
use std::time::{SystemTime, UNIX_EPOCH};

use roku_agent_runtime::LoopEvent;
use roku_common_types::{ConversationRole, ConversationTurn, RequestEnvelope, RequestId};
use roku_runtime_service::RuntimeService;
use rustyline::DefaultEditor;
use rustyline::error::ReadlineError;

use crate::CommandError;
use crate::conversation::compact_conversation_history;
use crate::runtime::{build_live_runtime_service_from_env, next_cli_request_sequence};
use crate::session_store::SessionStore;
use crate::storage::LocalStorageLayout;

/// Options parsed from the `chat` subcommand arguments.
pub(crate) struct ChatOptions {
	pub session_id: String,
	pub pipe: bool,
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
// Interactive REPL mode
// ---------------------------------------------------------------------------

fn run_interactive(rt: &tokio::runtime::Runtime, options: ChatOptions) -> Result<(), CommandError> {
	let service = tokio::task::block_in_place(build_live_runtime_service_from_env)?;
	let store = session_store();

	let mut editor =
		DefaultEditor::new().map_err(|e| CommandError::Io(io::Error::other(e.to_string())))?;

	// Load persisted history.
	let mut conversation_history: Vec<ConversationTurn> =
		store.load(&options.session_id).unwrap_or_else(|e| {
			eprintln!("[warn] failed to load session history: {e}");
			Vec::new()
		});

	print_banner(&options.session_id, conversation_history.len());

	loop {
		match editor.readline("roku> ") {
			Ok(line) => {
				let trimmed = line.trim();
				if trimmed.is_empty() {
					continue;
				}

				let _ = editor.add_history_entry(trimmed);

				match trimmed {
					"/quit" | "/exit" => break,
					"/help" => {
						print_help();
						continue;
					}
					"/clear" => {
						conversation_history.clear();
						if let Err(e) = store.clear(&options.session_id) {
							eprintln!("[warn] failed to clear session file: {e}");
						}
						if let Err(e) = service.clear_pending_loop(&options.session_id) {
							eprintln!("[warn] failed to clear pending loop: {e}");
						}
						eprintln!("[clear] Conversation history and pending state cleared.");
						continue;
					}
					"/compact" => {
						match compact_conversation_history(&mut conversation_history) {
							Some(result) => {
								// Rewrite file with compacted history.
								if let Err(e) = rewrite_history(
									&store,
									&options.session_id,
									&conversation_history,
								) {
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
					input if input.starts_with('/') => {
						eprintln!("Unknown command: {input}. Type /help for available commands.");
						continue;
					}
					_ => {}
				}

				let goal = trimmed.to_string();
				handle_turn_interactive(
					rt,
					&service,
					&store,
					&options.session_id,
					goal,
					&mut conversation_history,
					&mut editor,
				);
			}
			Err(ReadlineError::Interrupted | ReadlineError::Eof) => break,
			Err(e) => {
				eprintln!("[error] readline: {e}");
				break;
			}
		}
	}

	Ok(())
}

/// Handle a user turn in interactive mode, including AwaitingUser loops.
fn handle_turn_interactive(
	rt: &tokio::runtime::Runtime,
	service: &RuntimeService,
	store: &SessionStore,
	session_id: &str,
	goal: String,
	conversation_history: &mut Vec<ConversationTurn>,
	editor: &mut DefaultEditor,
) {
	let (result, _request_id) = dispatch_and_record(
		rt,
		service,
		store,
		session_id,
		goal,
		conversation_history,
		false,
	);
	match result {
		Ok(TurnResult::Completed(response)) => {
			println!("{response}");
		}
		Ok(TurnResult::AwaitingUser(question)) => {
			eprintln!("[agent is asking for input]");
			println!("{question}");

			loop {
				match editor.readline("roku(reply)> ") {
					Ok(reply_line) => {
						let reply = reply_line.trim();
						if reply.is_empty() {
							continue;
						}
						let _ = editor.add_history_entry(reply);

						let (inner_result, _) = dispatch_and_record(
							rt,
							service,
							store,
							session_id,
							reply.to_string(),
							conversation_history,
							false,
						);
						match inner_result {
							Ok(TurnResult::Completed(response)) => {
								println!("{response}");
								break;
							}
							Ok(TurnResult::AwaitingUser(q)) => {
								eprintln!("[agent is asking for input]");
								println!("{q}");
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
					Err(ReadlineError::Interrupted | ReadlineError::Eof) => {
						if let Err(e) = service.clear_pending_loop(session_id) {
							eprintln!("[warn] failed to clear pending loop: {e}");
						}
						eprintln!("[cancelled] Discarded pending agent state.");
						break;
					}
					Err(e) => {
						eprintln!("[error] readline: {e}");
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
}

fn run_pipe(rt: &tokio::runtime::Runtime, options: ChatOptions) -> Result<(), CommandError> {
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
			};
			write_json_stdout(&resp);
			return Err(e);
		}
	};
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
			};
			write_json_stdout(&resp);
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
				};
				write_json_stdout(&resp);
				break;
			}
		};

		let trimmed = line.trim();
		if trimmed.is_empty() {
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
				};
				write_json_stdout(&resp);
				continue;
			}
			_ => {}
		}

		let goal = trimmed.to_string();

		let (result, request_id) = dispatch_and_record(
			rt,
			&service,
			&store,
			&options.session_id,
			goal,
			&mut conversation_history,
			true,
		);

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
				};
				write_json_stdout(&resp);
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
				};
				write_json_stdout(&resp);
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
				};
				write_json_stdout(&resp);
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
				};
				write_json_stdout(&resp);
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
/// Returns `(Result<TurnResult>, request_id)` so pipe-mode callers can correlate
/// the response with the `RequestEnvelope` sent to the runtime.
fn dispatch_and_record(
	rt: &tokio::runtime::Runtime,
	service: &RuntimeService,
	store: &SessionStore,
	session_id: &str,
	goal: String,
	conversation_history: &mut Vec<ConversationTurn>,
	pipe_mode: bool,
) -> (Result<TurnResult, CommandError>, String) {
	let seq = next_cli_request_sequence();
	let request_id = format!("req-{seq}");

	let result = match execute_turn(
		rt,
		service,
		session_id,
		goal.clone(),
		conversation_history,
		pipe_mode,
		&request_id,
	) {
		Ok(r) => r,
		Err(e) => return (Err(e), request_id),
	};

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

	(Ok(result), request_id)
}

/// Execute a single chat turn with Ctrl+C cancellation support.
fn execute_turn(
	rt: &tokio::runtime::Runtime,
	service: &RuntimeService,
	session_id: &str,
	goal: String,
	conversation_history: &[ConversationTurn],
	pipe_mode: bool,
	request_id: &str,
) -> Result<TurnResult, CommandError> {
	rt.block_on(async {
		let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<LoopEvent>();

		let render_task = if pipe_mode {
			// Pipe mode: serialize events as JSONL to stderr.
			tokio::spawn(async move {
				while let Some(event) = rx.recv().await {
					if let Ok(json) = serde_json::to_string(&event) {
						eprintln!("{json}");
					}
				}
			})
		} else {
			// Interactive mode: human-friendly event display.
			tokio::spawn(async move {
				while let Some(event) = rx.recv().await {
					match event {
						LoopEvent::ToolStart { step, tool_name } => {
							eprintln!("[tool] step {step} starting: {tool_name}");
						}
						LoopEvent::ToolEnd {
							step,
							tool_name,
							elapsed_ms,
						} => {
							if let Some(ms) = elapsed_ms {
								eprintln!("[tool] step {step} done: {tool_name} ({ms}ms)");
							} else {
								eprintln!("[tool] step {step} done: {tool_name}");
							}
						}
						LoopEvent::CompactTriggered {
							step,
							estimated_tokens,
						} => {
							eprintln!(
								"[compact] step {step} triggered (~{estimated_tokens} tokens)"
							);
						}
						LoopEvent::LlmTextDelta { text, .. } => {
							eprint!("{text}");
						}
						LoopEvent::LlmDecisionComplete { .. } => {
							eprintln!();
						}
						LoopEvent::StepComplete { step } => {
							eprintln!("[step] {step} complete");
						}
					}
				}
			})
		};

		let request = RequestEnvelope {
			request_id: RequestId(request_id.to_string()),
			session_id: session_id.to_string(),
			goal,
			planning_mode_hint: None,
			conversation_history: conversation_history.to_vec(),
		};

		let execute_fut = async {
			service
				.execute_with_mode(request, roku_runtime_service::RunMode::Normal, Some(&tx))
				.await
		};

		let result = tokio::select! {
			response = execute_fut => {
				let response = response.map_err(CommandError::Runtime)?;
				drop(tx);
				render_task.await.ok();

				if let Ok(Some(_pending)) = service.pending_loop(session_id) {
					Ok(TurnResult::AwaitingUser(response.message))
				} else {
					Ok(TurnResult::Completed(response.message))
				}
			}
			_ = tokio::signal::ctrl_c() => {
				drop(tx);
				render_task.await.ok();
				let _ = service.clear_pending_loop(session_id);
				Ok(TurnResult::Cancelled)
			}
		};

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
	eprintln!("Roku interactive chat");
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
	eprintln!("  /help     Show this help message");
	eprintln!("  /clear    Reset conversation history and pending state");
	eprintln!("  /compact  Compress older conversation turns into a summary");
	eprintln!("  /quit     Exit the REPL (also: /exit, Ctrl+C, Ctrl+D)");
}
