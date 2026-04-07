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
//! Each turn is independent (no cross-turn conversation memory). The user types a goal,
//! the agent executes with real-time tool status via LoopEvent, and the response is printed.

use roku_agent_runtime::LoopEvent;
use roku_api_gateway::{Gateway, RawRequest};
use roku_runtime_service::RuntimeService;
use rustyline::DefaultEditor;
use rustyline::error::ReadlineError;

use crate::CommandError;
use crate::runtime::{build_live_runtime_service_from_env, next_cli_request_sequence};

/// Options parsed from the `chat` subcommand arguments.
pub(crate) struct ChatOptions {
	pub session_id: String,
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

	let mut editor = DefaultEditor::new().map_err(|e| {
		CommandError::Io(std::io::Error::other(e.to_string()))
	})?;

	print_banner();

	loop {
		match editor.readline("roku> ") {
			Ok(line) => {
				let trimmed = line.trim();
				if trimmed.is_empty() {
					continue;
				}

				// Record input in rustyline history (in-session only).
				let _ = editor.add_history_entry(trimmed);

				match trimmed {
					"/quit" | "/exit" => break,
					"/help" => {
						print_help();
						continue;
					}
					input if input.starts_with('/') => {
						eprintln!("Unknown command: {input}. Type /help for available commands.");
						continue;
					}
					_ => {}
				}

				let goal = trimmed.to_string();
				match execute_turn(rt, &service, &options.session_id, goal) {
					Ok(response) => {
						println!("{response}");
					}
					Err(e) => {
						eprintln!("[error] {e}");
					}
				}
			}
			Err(ReadlineError::Interrupted) => {
				// Ctrl+C at prompt — exit.
				break;
			}
			Err(ReadlineError::Eof) => {
				// Ctrl+D — exit.
				break;
			}
			Err(e) => {
				eprintln!("[error] readline: {e}");
				break;
			}
		}
	}

	Ok(())
}

/// Execute a single chat turn: build request, run agent, stream events, return response.
fn execute_turn(
	rt: &tokio::runtime::Runtime,
	service: &RuntimeService,
	session_id: &str,
	goal: String,
) -> Result<String, CommandError> {
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
					LoopEvent::StepComplete { step } => {
						eprintln!("[step] {step} complete");
					}
				}
			}
		});

		let gateway = Gateway;
		let request = gateway.normalize(
			RawRequest {
				session_id: session_id.to_string(),
				goal,
			},
			next_cli_request_sequence(),
		);

		let runtime_mode = service.runtime_mode_report();
		let response = service
			.execute_with_mode(request, roku_runtime_service::RunMode::Normal, Some(&tx))
			.await
			.map_err(CommandError::Runtime)?;

		drop(tx);
		render_task.await.ok();

		let mode_banner = format!(
			"[runtime requested={} effective={}]",
			runtime_mode.requested.as_str(),
			runtime_mode.effective.as_str()
		);
		Ok(format!("{mode_banner}\n{}", response.message))
	})
}

fn print_banner() {
	eprintln!("Roku interactive chat");
	eprintln!("Type a message to start. /help for commands, /quit to exit.");
	eprintln!();
}

fn print_help() {
	eprintln!("Commands:");
	eprintln!("  /help   Show this help message");
	eprintln!("  /quit   Exit the REPL (also: /exit, Ctrl+C, Ctrl+D)");
}
