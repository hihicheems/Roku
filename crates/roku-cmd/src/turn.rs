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

//! Turn lifecycle: dispatch, execution, and result types.

use std::time::{SystemTime, UNIX_EPOCH};

use roku_agent_runtime::{LoopEvent, RuntimeService};
use roku_common_types::{ConversationRole, ConversationTurn, RequestEnvelope, RequestId};

use crate::CommandError;
use crate::input::{InputReader, ReadlineResult};
use crate::pipe::{PipeResponse, write_jsonl_event, write_jsonl_result};
use crate::render;
use crate::runtime::next_cli_request_sequence;
use crate::session_store::SessionStore;

/// Result of a single turn execution.
pub(crate) enum TurnResult {
	/// Agent completed. Contains the response message.
	Completed(String),
	/// Agent is awaiting user input. Contains the question message.
	AwaitingUser(String),
	/// Execution was cancelled by Ctrl+C.
	Cancelled,
}

/// Per-turn token counts captured from the LoopEvent stream.
#[derive(Default, Clone)]
pub(crate) struct TurnTokens {
	pub(crate) prompt: u64,
	pub(crate) output: u64,
	/// Model that served the request (from the last TokenUsage event).
	pub(crate) model_id: Option<String>,
}

/// Returns the current time as Unix milliseconds.
pub(crate) fn now_unix_ms() -> u64 {
	SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.map(|d| d.as_millis().min(u64::MAX as u128) as u64)
		.unwrap_or(0)
}

/// Handle a user turn in interactive mode, including AwaitingUser loops.
#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_turn_interactive(
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

	// Persist token usage to session store.
	if turn_tokens.prompt > 0 || turn_tokens.output > 0 {
		let usage_entry = crate::session_store::SessionEntry::TokenUsage {
			timestamp_ms: now_unix_ms(),
			prompt_tokens: turn_tokens.prompt,
			output_tokens: turn_tokens.output,
			model_id: turn_tokens.model_id.clone(),
			session_prompt_total: *session_prompt_tokens,
			session_output_total: *session_output_tokens,
		};
		if let Err(e) = store.append_entries(session_id, &[usage_entry]) {
			tracing::warn!("failed to write token usage to session store: {e}");
		}
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

/// Execute a turn and record both user message and assistant response in history.
/// Persists new turns to the session store. On cancel/error, history is not modified.
///
/// Returns `(Result<TurnResult>, request_id, TurnTokens, bool)` so callers can track
/// token usage and whether the render task streamed LLM text.
pub(crate) fn dispatch_and_record(
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
pub(crate) fn execute_turn(
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
			// Interactive mode: RenderEngine drives terminal rendering.
			// Raw mode is active (for Esc key detection), so the engine uses
			// explicit \r\n instead of relying on terminal LF→CRLF translation.
			crate::render::engine::RenderEngine::new().spawn(
				rx,
				captured_tokens_task,
				text_streamed_flag,
			)
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
