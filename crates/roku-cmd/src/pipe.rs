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

//! Pipe mode: stdin-driven turns with JSON/JSONL output.

use std::io::{self, BufRead, Write};

use roku_agent_runtime::LoopEvent;
use roku_common_types::ConversationTurn;

use crate::CommandError;
use crate::runtime::{build_live_runtime_service_from_env, pipe_approval_gate};
use crate::session_store::SessionStore;
use crate::storage::LocalStorageLayout;
use crate::turn::{TurnResult, dispatch_and_record};

/// A typed JSONL envelope wrapping an event or result line.
#[derive(serde::Serialize)]
pub(crate) struct JsonLine<T: serde::Serialize> {
	#[serde(rename = "type")]
	pub(crate) kind: &'static str,
	pub(crate) data: T,
}

/// Write a typed JSONL line to stdout.
pub(crate) fn write_jsonl_event(event: &LoopEvent) {
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

pub(crate) fn write_jsonl_result(resp: &PipeResponse) {
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

/// Standard JSON response written to stdout in pipe mode.
#[derive(serde::Serialize)]
pub(crate) struct PipeResponse {
	pub(crate) ok: bool,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub(crate) request_id: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub(crate) session_id: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub(crate) status: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub(crate) message: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub(crate) error: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub(crate) suggestion: Option<String>,
	/// Model that served this request (populated from TokenUsage event).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub(crate) model: Option<String>,
}

pub(crate) fn write_json_stdout(resp: &PipeResponse) {
	if let Ok(json) = serde_json::to_string(resp) {
		let stdout = io::stdout();
		let mut handle = stdout.lock();
		let _ = writeln!(handle, "{json}");
		let _ = handle.flush();
	}
}

fn session_store() -> SessionStore {
	let layout = LocalStorageLayout::from_env();
	SessionStore::new(layout.session_history_dir)
}

pub(crate) fn run_pipe(
	rt: &tokio::runtime::Runtime,
	options: crate::chat::ChatOptions,
) -> Result<(), CommandError> {
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
