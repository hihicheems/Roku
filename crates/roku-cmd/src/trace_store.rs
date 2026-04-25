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

//! Trace storage: persists [`LoopEvent`] sequences to disk as JSONL files.
//!
//! Each agent loop execution produces a trace file at `~/.roku/traces/{run_id}.jsonl`
//! containing one [`TimestampedEvent`] per line.

use std::fs;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use roku_agent_runtime::LoopEvent;

use crate::storage::LocalStorageLayout;

/// A LoopEvent with a wall-clock timestamp for trace storage.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct TimestampedEvent {
	pub timestamp_ms: u64,
	#[serde(flatten)]
	pub event: LoopEvent,
}

/// Collects events during a run for later flush to disk.
#[derive(Debug, Clone)]
pub(crate) struct TraceCollector {
	pub run_id: String,
	pub events: Arc<Mutex<Vec<TimestampedEvent>>>,
}

impl TraceCollector {
	pub fn new(run_id: String) -> Self {
		Self {
			run_id,
			events: Arc::new(Mutex::new(Vec::new())),
		}
	}

	/// Record an event with the current wall-clock time.
	pub fn record(&self, event: &LoopEvent) {
		let timestamped = TimestampedEvent {
			timestamp_ms: now_ms(),
			event: event.clone(),
		};
		if let Ok(mut guard) = self.events.lock() {
			guard.push(timestamped);
		}
	}

	/// Flush collected events to a JSONL file in the traces directory.
	pub fn flush(&self) -> Result<(), std::io::Error> {
		let events = self
			.events
			.lock()
			.map_err(|e| std::io::Error::other(format!("lock poisoned: {e}")))?;
		if events.is_empty() {
			return Ok(());
		}
		let layout = LocalStorageLayout::from_env();
		let dir = &layout.traces_dir;
		fs::create_dir_all(dir)?;
		let path = dir.join(format!("{}.jsonl", sanitize_run_id(&self.run_id)));
		let mut file = fs::File::create(&path)?;
		for event in events.iter() {
			let json = serde_json::to_string(event)
				.map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
			writeln!(file, "{json}")?;
		}
		file.flush()?;
		Ok(())
	}
}

/// Summary of a single trace for `/trace` display.
pub(crate) struct TraceSummary {
	pub run_id: String,
	pub step_count: u32,
	pub tool_calls: Vec<String>,
	pub start_time: Option<u64>,
	pub end_time: Option<u64>,
	pub total_prompt_tokens: u64,
	pub total_output_tokens: u64,
}

/// Read trace files from the traces directory.
pub(crate) struct TraceStore {
	root: PathBuf,
}

impl TraceStore {
	pub fn from_env() -> Self {
		let layout = LocalStorageLayout::from_env();
		Self {
			root: layout.traces_dir,
		}
	}

	/// List trace files sorted by modification time (newest first).
	pub fn list_recent(&self, limit: usize) -> Vec<String> {
		let mut entries: Vec<(String, u64)> = Vec::new();
		if let Ok(dir) = fs::read_dir(&self.root) {
			for entry in dir.flatten() {
				let path = entry.path();
				if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
					continue;
				}
				let run_id = path
					.file_stem()
					.and_then(|s| s.to_str())
					.unwrap_or("")
					.to_string();
				if run_id.is_empty() {
					continue;
				}
				let mtime = file_mtime_ms(&path);
				entries.push((run_id, mtime));
			}
		}
		entries.sort_by(|a, b| b.1.cmp(&a.1));
		entries.into_iter().take(limit).map(|(id, _)| id).collect()
	}

	/// Load and summarize a trace by run_id.
	pub fn summarize(&self, run_id: &str) -> Option<TraceSummary> {
		if !is_safe_run_id(run_id) {
			return None;
		}
		let events = self.load_events(run_id)?;
		if events.is_empty() {
			return None;
		}
		let start_time = events.first().map(|e| e.timestamp_ms);
		let end_time = events.last().map(|e| e.timestamp_ms);
		let mut step_count: u32 = 0;
		let mut tool_calls = Vec::new();
		let mut total_prompt_tokens: u64 = 0;
		let mut total_output_tokens: u64 = 0;

		for te in &events {
			match &te.event {
				LoopEvent::StepComplete { step } => {
					step_count = step_count.max(*step);
				}
				LoopEvent::ToolStart { tool_name, .. } => {
					tool_calls.push(tool_name.clone());
				}
				LoopEvent::TokenUsage {
					prompt_tokens,
					output_tokens,
					..
				} => {
					total_prompt_tokens = total_prompt_tokens.saturating_add(*prompt_tokens);
					total_output_tokens = total_output_tokens.saturating_add(*output_tokens);
				}
				_ => {}
			}
		}
		Some(TraceSummary {
			run_id: run_id.to_string(),
			step_count,
			tool_calls,
			start_time,
			end_time,
			total_prompt_tokens,
			total_output_tokens,
		})
	}

	/// Load all timestamped events from a trace file.
	pub fn load_events(&self, run_id: &str) -> Option<Vec<TimestampedEvent>> {
		if !is_safe_run_id(run_id) {
			return None;
		}
		let path = self.root.join(format!("{run_id}.jsonl"));
		if !path.exists() {
			return None;
		}
		let file = fs::File::open(&path).ok()?;
		let reader = std::io::BufReader::new(file);
		let mut events = Vec::new();
		for line in reader.lines() {
			let Ok(line) = line else { continue };
			let trimmed = line.trim();
			if trimmed.is_empty() {
				continue;
			}
			if let Ok(event) = serde_json::from_str::<TimestampedEvent>(trimmed) {
				events.push(event);
			}
		}
		Some(events)
	}
}

/// Render a trace summary for display.
pub(crate) fn render_trace_summary(summary: &TraceSummary) -> String {
	let mut lines = Vec::new();
	lines.push(format!("[trace] run_id: {}", summary.run_id));
	lines.push(format!("[trace] steps: {}", summary.step_count));
	if let (Some(start), Some(end)) = (summary.start_time, summary.end_time) {
		let duration_ms = end.saturating_sub(start);
		let duration_s = duration_ms as f64 / 1000.0;
		lines.push(format!("[trace] duration: {duration_s:.1}s"));
	}
	lines.push(format!(
		"[trace] tokens: {} prompt, {} output",
		summary.total_prompt_tokens, summary.total_output_tokens
	));
	if !summary.tool_calls.is_empty() {
		lines.push(format!("[trace] tools ({}):", summary.tool_calls.len()));
		for tool in &summary.tool_calls {
			lines.push(format!("  - {tool}"));
		}
	}
	lines.join("\n")
}

/// Render detailed events for `/trace {run_id}`.
pub(crate) fn render_trace_detail(events: &[TimestampedEvent]) -> String {
	let mut lines = Vec::new();
	for te in events {
		let ts = te.timestamp_ms;
		let desc = match &te.event {
			LoopEvent::ToolStart {
				step,
				tool_name,
				args_summary,
				..
			} => {
				let args = args_summary
					.as_deref()
					.map(|s| format!(" ({s})"))
					.unwrap_or_default();
				format!("[step {step}] tool_start: {tool_name}{args}")
			}
			LoopEvent::ToolEnd {
				step,
				tool_name,
				elapsed_ms,
				..
			} => {
				let elapsed = elapsed_ms
					.map(|ms| format!(" ({ms}ms)"))
					.unwrap_or_default();
				format!("[step {step}] tool_end: {tool_name}{elapsed}")
			}
			LoopEvent::CompactTriggered {
				step,
				estimated_tokens,
			} => format!("[step {step}] compact_triggered: {estimated_tokens} tokens"),
			LoopEvent::CompactComplete {
				step, elapsed_ms, ..
			} => format!("[step {step}] compact_complete: {elapsed_ms}ms"),
			LoopEvent::LlmTextDelta { step, .. } => format!("[step {step}] llm_text_delta"),
			LoopEvent::LlmTextReplace { step, text } => format!(
				"[step {step}] llm_text_replace: chars={}",
				text.chars().count()
			),
			LoopEvent::LlmDecisionComplete { step } => {
				format!("[step {step}] llm_decision_complete")
			}
			LoopEvent::StepComplete { step } => format!("[step {step}] step_complete"),
			LoopEvent::TokenUsage {
				step,
				prompt_tokens,
				output_tokens,
				estimated_cost_usd,
				uncached_input_cost_usd,
				cache_write_cost_usd,
				cache_read_cost_usd,
				output_cost_usd,
				..
			} => format!(
				"[step {step}] tokens: {prompt_tokens}/{output_tokens} \
				${estimated_cost_usd:.4} \
				(in: ${uncached_input_cost_usd:.4}, \
				cache_w: ${cache_write_cost_usd:.4}, \
				cache_r: ${cache_read_cost_usd:.4}, \
				out: ${output_cost_usd:.4})"
			),
			LoopEvent::ReactiveCompactTriggered { step, detail } => {
				format!("[step {step}] reactive_compact_triggered: {detail}")
			}
			LoopEvent::MidCompactLayer2Ran {
				step,
				messages_replaced,
			} => format!(
				"[step {step}] mid_compact_layer2_ran: messages_replaced={messages_replaced}"
			),
			LoopEvent::MidCompactLayer1Ran {
				step,
				messages_collapsed,
			} => format!(
				"[step {step}] mid_compact_layer1_ran: messages_collapsed={messages_collapsed}"
			),
			LoopEvent::AutoCompactSummarizerCalled {
				step,
				prompt_tokens,
				output_tokens,
				succeeded,
				drop_oldest_retries,
			} => format!(
				"[step {step}] auto_compact_summarizer: succeeded={succeeded} tokens={prompt_tokens}/{output_tokens} retries={drop_oldest_retries}"
			),
			LoopEvent::AutoCompactCircuitBreakerTripped {
				step,
				consecutive_failures,
			} => format!(
				"[step {step}] auto_compact_breaker_tripped: consecutive_failures={consecutive_failures}"
			),
			LoopEvent::EstimatorCalibrated {
				step,
				estimated_prompt_tokens,
				prompt_tokens,
				scale,
			} => format!(
				"[step {step}] estimator_calibrated: estimated={estimated_prompt_tokens} real={prompt_tokens} scale={scale:.3}"
			),
			LoopEvent::ToolBudgetCheck {
				step,
				per_turn_tool_tokens,
				exceeded,
			} => {
				format!(
					"[step {}] tool_budget_check: per_turn_tool_tokens={} exceeded={}",
					step, per_turn_tool_tokens, exceeded
				)
			}
			LoopEvent::CacheBreakDetected {
				step,
				tokens_lost,
				component_changed,
				..
			} => format!(
				"[step {step}] cache_break_detected: tokens_lost={tokens_lost} changed=[{}]",
				component_changed.join(", ")
			),
			LoopEvent::ToolSchemaFrozen {
				step,
				hash,
				rebuilt,
			} => format!("[step {step}] tool_schema_frozen: hash={hash:x} rebuilt={rebuilt}"),
			LoopEvent::ReasoningContentStripped {
				step,
				messages_stripped,
			} => format!(
				"[step {step}] reasoning_content_stripped: messages_stripped={messages_stripped}"
			),
			LoopEvent::OutputSlotEscalated {
				step,
				initial_max_tokens,
				escalated_max_tokens,
				model_id,
			} => format!(
				"[step {step}] output_slot_escalated: {initial_max_tokens}->{escalated_max_tokens} model={model_id}"
			),
			LoopEvent::OutputSlotEscalationUnsupported {
				step,
				model_id,
				provider,
			} => format!(
				"[step {step}] output_slot_escalation_unsupported: model={model_id} provider={provider}"
			),
			LoopEvent::WebSocketDelta {
				step,
				reuse_count,
				has_previous_response_id,
			} => format!(
				"[step {step}] ws_delta: reuse_count={reuse_count} has_previous_response_id={has_previous_response_id}"
			),
		};
		lines.push(format!("{ts} {desc}"));
	}
	lines.join("\n")
}

/// Reject run IDs that could escape the traces directory.
fn is_safe_run_id(run_id: &str) -> bool {
	!run_id.is_empty()
		&& !run_id.contains('/')
		&& !run_id.contains('\\')
		&& !run_id.contains("..")
		&& !run_id.starts_with('.')
}

fn sanitize_run_id(run_id: &str) -> String {
	run_id
		.chars()
		.map(|c| {
			if c.is_alphanumeric() || c == '-' || c == '_' {
				c
			} else {
				'_'
			}
		})
		.collect()
}

fn now_ms() -> u64 {
	SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.map(|d| d.as_millis().min(u64::MAX as u128) as u64)
		.unwrap_or(0)
}

fn file_mtime_ms(path: &Path) -> u64 {
	fs::metadata(path)
		.and_then(|m| m.modified())
		.ok()
		.and_then(|t| t.duration_since(UNIX_EPOCH).ok())
		.map(|d| d.as_millis().min(u64::MAX as u128) as u64)
		.unwrap_or(0)
}
