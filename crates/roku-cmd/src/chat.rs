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

//! Interactive REPL for `roku chat`.
//!
//! Maintains conversation context across turns via `conversation_history` in each
//! `RequestEnvelope`. Supports AwaitingUser resume, `/clear`, `/compact`, and
//! Ctrl+C cancellation during execution.

use std::time::{SystemTime, UNIX_EPOCH};

use roku_agent_runtime::LoopEvent;
use roku_common_types::{ConversationRole, ConversationTurn, RequestEnvelope, RequestId};
use roku_runtime_service::RuntimeService;
use rustyline::DefaultEditor;
use rustyline::error::ReadlineError;

use crate::CommandError;
use crate::conversation::compact_conversation_history;
use crate::runtime::{build_live_runtime_service_from_env, next_cli_request_sequence};

/// Options parsed from the `chat` subcommand arguments.
pub(crate) struct ChatOptions {
	pub session_id: String,
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

/// Runs the interactive REPL loop.
///
/// The caller provides a pre-built tokio `Runtime` to block on async work.
/// The REPL reads user input via rustyline, dispatches each turn through the
/// RuntimeService, and streams LoopEvent status to stderr.
pub(crate) fn run_chat(
	rt: &tokio::runtime::Runtime,
	options: ChatOptions,
) -> Result<(), CommandError> {
	let service = tokio::task::block_in_place(build_live_runtime_service_from_env)?;

	let mut editor =
		DefaultEditor::new().map_err(|e| CommandError::Io(std::io::Error::other(e.to_string())))?;

	let mut conversation_history: Vec<ConversationTurn> = Vec::new();

	print_banner();

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
						if let Err(e) = service.clear_pending_loop(&options.session_id) {
							eprintln!("[warn] failed to clear pending loop: {e}");
						}
						eprintln!("[clear] Conversation history and pending state cleared.");
						continue;
					}
					"/compact" => {
						match compact_conversation_history(&mut conversation_history) {
							Some(result) => eprintln!(
								"[compact] Compacted {} turns into summary. {} turns remain.",
								result.discarded, result.retained
							),
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
				handle_turn(
					rt,
					&service,
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

/// Handle a user turn including potential AwaitingUser follow-up loops.
fn handle_turn(
	rt: &tokio::runtime::Runtime,
	service: &RuntimeService,
	session_id: &str,
	goal: String,
	conversation_history: &mut Vec<ConversationTurn>,
	editor: &mut DefaultEditor,
) {
	match dispatch_and_record(rt, service, session_id, goal, conversation_history) {
		Ok(TurnResult::Completed(response)) => {
			println!("{response}");
		}
		Ok(TurnResult::AwaitingUser(question)) => {
			eprintln!("[agent is asking for input]");
			println!("{question}");

			// Loop to collect the user's reply and resume.
			loop {
				match editor.readline("roku(reply)> ") {
					Ok(reply_line) => {
						let reply = reply_line.trim();
						if reply.is_empty() {
							continue;
						}
						let _ = editor.add_history_entry(reply);

						// Send the reply as a new goal — the runtime will auto-resume
						// via take_resumable_pending_loop.
						match dispatch_and_record(
							rt,
							service,
							session_id,
							reply.to_string(),
							conversation_history,
						) {
							Ok(TurnResult::Completed(response)) => {
								println!("{response}");
								break;
							}
							Ok(TurnResult::AwaitingUser(q)) => {
								eprintln!("[agent is asking for input]");
								println!("{q}");
								// Continue the reply loop.
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
						// Ctrl+C or Ctrl+D during reply prompt — discard the awaiting state.
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

/// Execute a turn and record both user message and assistant response in history
/// only on success. On cancel/error, history is not modified.
fn dispatch_and_record(
	rt: &tokio::runtime::Runtime,
	service: &RuntimeService,
	session_id: &str,
	goal: String,
	conversation_history: &mut Vec<ConversationTurn>,
) -> Result<TurnResult, CommandError> {
	let result = execute_turn(rt, service, session_id, goal.clone(), conversation_history)?;

	match &result {
		TurnResult::Completed(response) | TurnResult::AwaitingUser(response) => {
			conversation_history.push(ConversationTurn {
				role: ConversationRole::User,
				content: goal,
				created_at_unix_ms: now_unix_ms(),
			});
			conversation_history.push(ConversationTurn {
				role: ConversationRole::Assistant,
				content: response.clone(),
				created_at_unix_ms: now_unix_ms(),
			});
		}
		TurnResult::Cancelled => {
			// Don't record incomplete turns.
		}
	}

	Ok(result)
}

/// Execute a single chat turn with Ctrl+C cancellation support.
///
/// Returns `TurnResult::AwaitingUser` if the agent paused for user input,
/// `TurnResult::Completed` on normal completion, or `TurnResult::Cancelled` on Ctrl+C.
fn execute_turn(
	rt: &tokio::runtime::Runtime,
	service: &RuntimeService,
	session_id: &str,
	goal: String,
	conversation_history: &[ConversationTurn],
) -> Result<TurnResult, CommandError> {
	rt.block_on(async {
		let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<LoopEvent>();

		let render_task = tokio::spawn(async move {
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
						eprintln!("[compact] step {step} triggered (~{estimated_tokens} tokens)");
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
		});

		let seq = next_cli_request_sequence();
		let request = RequestEnvelope {
			request_id: RequestId(format!("req-{seq}")),
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

		// Race execution against Ctrl+C for cancellation.
		// Note: tokio::signal::ctrl_c() uses signal_hook_registry which is additive —
		// it does not replace rustyline's SIGINT handler, so readline still works
		// correctly at the prompt after cancellation.
		let result = tokio::select! {
			response = execute_fut => {
				let response = response.map_err(CommandError::Runtime)?;
				drop(tx);
				render_task.await.ok();

				// Check if the agent is awaiting user input.
				if let Ok(Some(_pending)) = service.pending_loop(session_id) {
					Ok(TurnResult::AwaitingUser(response.message))
				} else {
					Ok(TurnResult::Completed(response.message))
				}
			}
			_ = tokio::signal::ctrl_c() => {
				// Drop the sender to unblock the render task.
				drop(tx);
				render_task.await.ok();
				// Clear any partial pending loop state.
				let _ = service.clear_pending_loop(session_id);
				Ok(TurnResult::Cancelled)
			}
		};

		result
	})
}

fn now_unix_ms() -> u64 {
	SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.map(|d| d.as_millis().min(u64::MAX as u128) as u64)
		.unwrap_or(0)
}

fn print_banner() {
	eprintln!("Roku interactive chat");
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
