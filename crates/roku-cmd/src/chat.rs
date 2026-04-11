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
use roku_agent_runtime::RuntimeService;
use roku_common_types::{ConversationRole, ConversationTurn, RequestEnvelope, RequestId};
use rustyline::DefaultEditor;
use rustyline::error::ReadlineError;

use crate::CommandError;
use crate::conversation::compact_conversation_history;
use crate::runtime::{
	build_live_runtime_service_from_env, cli_approval_gate, next_cli_request_sequence,
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
#[derive(Default, Clone, Copy)]
struct TurnTokens {
	prompt: u64,
	output: u64,
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
	let service = tokio::task::block_in_place(build_live_runtime_service_from_env)?
		.with_approval_gate(cli_approval_gate());
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

	// Session-level cumulative token counters.
	let mut session_prompt_tokens: u64 = 0;
	let mut session_output_tokens: u64 = 0;

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
					options.json,
					&mut session_prompt_tokens,
					&mut session_output_tokens,
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
#[allow(clippy::too_many_arguments)]
fn handle_turn_interactive(
	rt: &tokio::runtime::Runtime,
	service: &RuntimeService,
	store: &SessionStore,
	session_id: &str,
	goal: String,
	conversation_history: &mut Vec<ConversationTurn>,
	editor: &mut DefaultEditor,
	json_mode: bool,
	session_prompt_tokens: &mut u64,
	session_output_tokens: &mut u64,
) {
	let (result, _request_id, turn_tokens) = dispatch_and_record(
		rt,
		service,
		store,
		session_id,
		goal,
		conversation_history,
		false,
		json_mode,
	);
	// Accumulate session totals and display token usage after this turn.
	*session_prompt_tokens = session_prompt_tokens.saturating_add(turn_tokens.prompt);
	*session_output_tokens = session_output_tokens.saturating_add(turn_tokens.output);
	if !json_mode && (turn_tokens.prompt > 0 || turn_tokens.output > 0) {
		let turn_cost = (turn_tokens.prompt as f64 / 1_000_000.0) * 3.0
			+ (turn_tokens.output as f64 / 1_000_000.0) * 15.0;
		let session_total = session_prompt_tokens.saturating_add(*session_output_tokens);
		eprintln!(
			"[tokens: {}/{}, cost: ~${turn_cost:.4}] [session: {}]",
			turn_tokens.prompt, turn_tokens.output, session_total,
		);
	}

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
			};
			write_jsonl_result(&resp);
		} else {
			println!("{msg}");
		}
	};
	match result {
		Ok(TurnResult::Completed(response)) => {
			emit_response(&response, "succeeded");
		}
		Ok(TurnResult::AwaitingUser(question)) => {
			if !json_mode {
				eprintln!("[agent is asking for input]");
			}
			emit_response(&question, "awaiting_user");

			loop {
				match editor.readline("roku(reply)> ") {
					Ok(reply_line) => {
						let reply = reply_line.trim();
						if reply.is_empty() {
							continue;
						}
						let _ = editor.add_history_entry(reply);

						let (inner_result, _, inner_tokens) = dispatch_and_record(
							rt,
							service,
							store,
							session_id,
							reply.to_string(),
							conversation_history,
							false,
							json_mode,
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
								emit_response(&response, "succeeded");
								break;
							}
							Ok(TurnResult::AwaitingUser(q)) => {
								if !json_mode {
									eprintln!("[agent is asking for input]");
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
			};
			emit_resp(&resp);
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
				};
				emit_resp(&resp);
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
				emit_resp(&resp);
				continue;
			}
			_ => {}
		}

		let goal = trimmed.to_string();

		let json_mode = options.json;
		let (result, request_id, _tokens) = dispatch_and_record(
			rt,
			&service,
			&store,
			&options.session_id,
			goal,
			&mut conversation_history,
			true,
			json_mode,
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
/// Returns `(Result<TurnResult>, request_id, TurnTokens)` so callers can track
/// token usage and correlate the response with the `RequestEnvelope` sent to the runtime.
fn dispatch_and_record(
	rt: &tokio::runtime::Runtime,
	service: &RuntimeService,
	store: &SessionStore,
	session_id: &str,
	goal: String,
	conversation_history: &mut Vec<ConversationTurn>,
	pipe_mode: bool,
	json_mode: bool,
) -> (Result<TurnResult, CommandError>, String, TurnTokens) {
	let seq = next_cli_request_sequence();
	let request_id = format!("req-{seq}");

	let (turn_result, tokens) = match execute_turn(
		rt,
		service,
		session_id,
		goal.clone(),
		conversation_history,
		pipe_mode,
		json_mode,
		&request_id,
	) {
		Ok(pair) => pair,
		Err(e) => return (Err(e), request_id, TurnTokens::default()),
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

	(Ok(result), request_id, tokens)
}

/// Execute a single chat turn with Ctrl+C cancellation support.
///
/// Returns `Ok((TurnResult, TurnTokens))` where `TurnTokens` holds the prompt/output
/// token counts from the `TokenUsage` LoopEvent emitted by the runtime.
fn execute_turn(
	rt: &tokio::runtime::Runtime,
	service: &RuntimeService,
	session_id: &str,
	goal: String,
	conversation_history: &[ConversationTurn],
	pipe_mode: bool,
	json_mode: bool,
	request_id: &str,
) -> Result<(TurnResult, TurnTokens), CommandError> {
	rt.block_on(async {
		let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<LoopEvent>();

		// Shared token accumulator: the render task writes, execute_turn reads after join.
		let captured_tokens = std::sync::Arc::new(std::sync::Mutex::new(TurnTokens::default()));
		let captured_tokens_task = captured_tokens.clone();

		let render_task = if json_mode {
			// JSON mode: emit typed JSONL event lines to stdout + capture tokens.
			tokio::spawn(async move {
				while let Some(event) = rx.recv().await {
					if let LoopEvent::TokenUsage {
						prompt_tokens,
						output_tokens,
						..
					} = &event
						&& let Ok(mut guard) = captured_tokens_task.lock()
					{
						guard.prompt = guard.prompt.saturating_add(*prompt_tokens);
						guard.output = guard.output.saturating_add(*output_tokens);
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
						..
					} = &event
						&& let Ok(mut guard) = captured_tokens_task.lock()
					{
						guard.prompt = guard.prompt.saturating_add(*prompt_tokens);
						guard.output = guard.output.saturating_add(*output_tokens);
					}
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
						LoopEvent::TokenUsage {
							prompt_tokens,
							output_tokens,
							..
						} => {
							if let Ok(mut guard) = captured_tokens_task.lock() {
								guard.prompt = guard.prompt.saturating_add(prompt_tokens);
								guard.output = guard.output.saturating_add(output_tokens);
							}
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
				.execute_with_mode(request, roku_agent_runtime::RunMode::Normal, Some(&tx))
				.await
		};

		let result = tokio::select! {
			response = execute_fut => {
				let response = response.map_err(CommandError::Runtime)?;
				drop(tx);
				render_task.await.ok();

				let tokens = captured_tokens.lock().map(|g| *g).unwrap_or_default();
				if let Ok(Some(_pending)) = service.pending_loop(session_id) {
					Ok((TurnResult::AwaitingUser(response.message), tokens))
				} else {
					Ok((TurnResult::Completed(response.message), tokens))
				}
			}
			_ = tokio::signal::ctrl_c() => {
				drop(tx);
				render_task.await.ok();
				let _ = service.clear_pending_loop(session_id);
				Ok((TurnResult::Cancelled, TurnTokens::default()))
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
