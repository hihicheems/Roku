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

//! RenderEngine — LoopEvent consumer that drives terminal rendering.
//!
//! Absorbs the inline render closure from execute_turn, using RenderState
//! for mutable state and the streaming subsystem for newline-gated output.

use std::io::{self, Write};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use roku_agent_runtime::LoopEvent;

use super::state::RenderState;
use super::streaming::{MarkdownStreamCollector, StreamController};
use crate::turn::TurnTokens;

/// Consumes LoopEvent from a channel and drives terminal rendering.
pub(crate) struct RenderEngine {
	state: RenderState,
	collector: MarkdownStreamCollector,
	controller: StreamController,
}

impl RenderEngine {
	pub(crate) fn new() -> Self {
		Self {
			state: RenderState::new(),
			collector: MarkdownStreamCollector::new(),
			controller: StreamController::new(),
		}
	}

	/// Spawn the render engine as a tokio task.
	///
	/// Consumes events from `rx`, renders to stderr, and tracks tokens.
	pub(crate) fn spawn(
		mut self,
		mut rx: tokio::sync::mpsc::UnboundedReceiver<LoopEvent>,
		captured_tokens: Arc<Mutex<TurnTokens>>,
		text_streamed_flag: Arc<AtomicBool>,
	) -> tokio::task::JoinHandle<()> {
		tokio::spawn(async move {
			// Show initial status line.
			self.show_status();

			// Timer for status refresh + stream controller ticks.
			// 50ms for smoother streaming (Codex uses 32ms), 500ms was too coarse.
			let mut tick = tokio::time::interval(std::time::Duration::from_millis(50));
			tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
			tick.tick().await; // Consume immediate first tick.

			// Counter for status refresh (every 10th tick = 500ms).
			let mut tick_count: u32 = 0;

			loop {
				tokio::select! {
					event = rx.recv() => {
						let Some(event) = event else { break };
						self.handle_event(
							event,
							&captured_tokens,
							&text_streamed_flag,
						);
					}
					_ = tick.tick() => {
						tick_count += 1;

						// Release queued lines from the stream controller.
						if let Some(text) = self.controller.on_tick() {
							self.state.had_text_output = true;
							text_streamed_flag.store(true, Ordering::Relaxed);
							let rendered = self.state.stream_renderer.push(&text);
							if !rendered.is_empty() {
								eprint!("{}", rendered.replace('\n', "\r\n"));
								let _ = io::stderr().flush();
							}
						}

						// Refresh status line every ~500ms (10 × 50ms).
						if tick_count.is_multiple_of(10)
							&& !self.state.streaming_active
							&& self.state.pending_tool_name.is_none()
							&& !crate::is_approval_active()
							&& self.state.last_tool_event.elapsed()
								>= std::time::Duration::from_secs(1)
						{
							self.show_status();
						}
					}
				}
			}

			// Drain any remaining queued lines.
			if let Some(text) = self.controller.drain_all() {
				let rendered = self.state.stream_renderer.push(&text);
				if !rendered.is_empty() {
					eprint!("{}", rendered.replace('\n', "\r\n"));
				}
			}

			// Clear status line on exit.
			if self.state.status_visible {
				eprint!("\r\x1b[K");
			}
			let _ = io::stderr().flush();
		})
	}

	fn handle_event(
		&mut self,
		event: LoopEvent,
		captured_tokens: &Arc<Mutex<TurnTokens>>,
		text_streamed_flag: &Arc<AtomicBool>,
	) {
		let s = &mut self.state;

		// Clear status before printing event output.
		if s.status_visible {
			eprint!("\r\x1b[K");
			s.status_visible = false;
		}

		// Close any pending ToolStart line before other output.
		let is_matching_tool_end = if let LoopEvent::ToolEnd { ref tool_name, .. } = event {
			s.pending_tool_name.as_deref() == Some(tool_name.as_str())
		} else {
			false
		};
		if s.pending_tool_name.is_some() && !is_matching_tool_end {
			eprint!("\r\n");
			s.pending_tool_name = None;
		}

		match event {
			LoopEvent::ToolStart {
				step,
				tool_name,
				args_summary,
				..
			} => {
				if s.pending_tool_name.is_some() {
					eprint!("\r\n");
				}
				s.current_step = step;
				s.current_tool = Some(tool_name.clone());
				s.streaming_active = false;
				s.last_tool_event = std::time::Instant::now();
				let msg = crate::render::styled_tool_start(&tool_name, args_summary.as_deref());
				eprint!("{msg}");
				let _ = io::stderr().flush();
				s.pending_tool_name = Some(tool_name);
			}
			LoopEvent::ToolEnd {
				tool_name,
				elapsed_ms,
				result_summary,
				..
			} => {
				s.current_tool = None;
				s.last_tool_event = std::time::Instant::now();
				if is_matching_tool_end {
					let suffix = crate::render::styled_tool_end_suffix(
						elapsed_ms,
						result_summary.as_deref(),
					);
					eprint!("{suffix}\r\n");
					s.pending_tool_name = None;
				} else {
					let msg = crate::render::styled_tool_end(
						&tool_name,
						elapsed_ms,
						result_summary.as_deref(),
					);
					eprint!("{msg}\r\n");
				}
			}
			LoopEvent::CompactTriggered {
				step,
				estimated_tokens,
			} => {
				eprint!(
					"{}\r\n",
					crate::render::style::styled_compact_notice(&format!(
						"[compact] step {step} triggered (~{estimated_tokens} tokens)"
					))
				);
			}
			LoopEvent::CompactComplete {
				elapsed_ms,
				llm_succeeded,
				..
			} => {
				let method = if llm_succeeded { "LLM" } else { "mechanical" };
				eprint!(
					"{}\r\n",
					crate::render::style::styled_compact_notice(&format!(
						"[compact] completed ({method}, {elapsed_ms}ms)"
					))
				);
			}
			LoopEvent::LlmTextDelta { text, .. } => {
				s.streaming_active = true;
				// Push delta through newline-gated collector
				self.collector.push_delta(&text);
				// Commit complete lines and enqueue for controlled release
				if let Some(committed) = self.collector.commit_complete_lines() {
					self.controller.enqueue(committed);
				}
			}
			LoopEvent::LlmDecisionComplete { .. } => {
				s.streaming_active = false;
				// Finalize: flush collector remainder
				if let Some(remaining) = self.collector.finalize_and_drain() {
					self.controller.enqueue(remaining);
				}
				// Drain all remaining from controller
				if let Some(text) = self.controller.drain_all() {
					let rendered = s.stream_renderer.push(&text);
					if !rendered.is_empty() {
						s.had_text_output = true;
						text_streamed_flag.store(true, Ordering::Relaxed);
						eprint!("{}", rendered.replace('\n', "\r\n"));
					}
				}
				// Flush the stream renderer (may contain only ANSI reset)
				let flush_output = s.stream_renderer.flush();
				if !flush_output.is_empty() {
					eprint!("{}", flush_output.replace('\n', "\r\n"));
				}
				if s.had_text_output {
					eprint!("\r\n");
					s.had_text_output = false;
				}
				let _ = io::stderr().flush();
			}
			LoopEvent::StepComplete { step } => {
				s.current_step = step;
				s.current_tool = None;
			}
			LoopEvent::TokenUsage {
				prompt_tokens,
				output_tokens,
				estimated_cost_usd,
				uncached_input_cost_usd,
				cache_write_cost_usd,
				cache_read_cost_usd,
				output_cost_usd,
				model_id,
				..
			} => {
				if let Ok(mut guard) = captured_tokens.lock() {
					guard.prompt = guard.prompt.saturating_add(prompt_tokens);
					guard.output = guard.output.saturating_add(output_tokens);
					guard.estimated_cost_usd += estimated_cost_usd;
					guard.uncached_input_cost_usd += uncached_input_cost_usd;
					guard.cache_write_cost_usd += cache_write_cost_usd;
					guard.cache_read_cost_usd += cache_read_cost_usd;
					guard.output_cost_usd += output_cost_usd;
					if let Some(id) = model_id {
						guard.model_id = Some(id);
					}
				}
			}
			LoopEvent::ReactiveCompactTriggered { step, detail } => {
				eprint!(
					"{}\r\n",
					crate::render::style::styled_compact_notice(&format!(
						"[compact] step {step} reactive trigger: {detail}"
					))
				);
			}
			LoopEvent::MicrocompactRan { .. } => {
				// Layer 0 microcompact runs on every pre-flight; suppress the
				// per-step line to keep live UX quiet. Trace consumers see the
				// freed_tokens via the LoopEvent stream.
			}
			LoopEvent::MidCompactLayer2Ran { .. } | LoopEvent::MidCompactLayer1Ran { .. } => {
				// Mid-tier compaction events are diagnostic; no user-facing line.
				// Trace consumers see the details via the LoopEvent stream.
			}
			LoopEvent::AutoCompactSummarizerCalled { .. } => {
				// The summarizer call is bracketed by CompactTriggered /
				// CompactComplete; this event only carries diagnostic fields
				// (tokens, retries) for trace consumers.
			}
			LoopEvent::AutoCompactCircuitBreakerTripped {
				step,
				consecutive_failures,
			} => {
				eprint!(
					"{}\r\n",
					crate::render::style::styled_compact_notice(&format!(
						"[compact] step {step} summarizer circuit breaker tripped after {consecutive_failures} consecutive failures; falling back to mechanical"
					))
				);
			}
			LoopEvent::EstimatorCalibrated { .. } => {
				// Calibration samples are diagnostic; no live UX feedback.
			}
			LoopEvent::ToolBudgetCheck {
				step,
				per_turn_tool_tokens,
				exceeded,
			} => {
				if exceeded {
					eprint!(
						"{}\r\n",
						crate::render::style::styled_compact_notice(&format!(
							"[budget] step {step} tool result budget exceeded (~{per_turn_tool_tokens} tokens)"
						))
					);
				}
			}
			LoopEvent::CacheBreakDetected {
				step, tokens_lost, ..
			} => {
				eprint!(
					"{}\r\n",
					crate::render::style::styled_compact_notice(&format!(
						"[cache] step {step} prefix cache break detected (~{tokens_lost} tokens lost)"
					))
				);
			}
			LoopEvent::ReasoningContentStripped { .. } => {
				// Thinking-block stripping is a diagnostic detail for trace consumers;
				// no live UX line emitted.
			}
			LoopEvent::OutputSlotEscalated {
				step,
				initial_max_tokens,
				escalated_max_tokens,
				model_id,
			} => {
				eprint!(
					"{}\r\n",
					crate::render::style::styled_compact_notice(&format!(
						"[output_slot] step {step} escalated {initial_max_tokens}->{escalated_max_tokens} for {model_id}"
					))
				);
			}
			LoopEvent::WebSocketDelta {
				step,
				reuse_count,
				has_previous_response_id,
			} => {
				if has_previous_response_id {
					eprint!(
						"{}\r\n",
						crate::render::style::styled_compact_notice(&format!(
							"[ws_delta] step {step} delta request (reuse_count={reuse_count})"
						))
					);
				}
			}
		}
	}

	fn show_status(&mut self) {
		if self.state.status_visible {
			eprint!("\r\x1b[K");
		}
		let s = crate::render::styled_working_status(
			self.state.current_step,
			self.state.current_tool.as_deref(),
			self.state.start_time.elapsed(),
		);
		eprint!("{s}");
		let _ = io::stderr().flush();
		self.state.status_visible = true;
	}
}
