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

//! Streaming subsystem: newline-gated collector and stream controller.
//!
//! The collector buffers text deltas and only commits complete lines (ending in `\n`).
//! The controller manages a queue of committed lines and releases them on tick for
//! smooth output. Implements a two-gear policy: Smooth (1 line/tick) and CatchUp
//! (drain all when queue grows large).

use std::collections::VecDeque;
use std::time::Instant;

/// Collects streaming text deltas and commits only complete lines.
///
/// Partial lines (no trailing `\n`) are buffered until the next delta
/// completes them. This prevents half-rendered markdown from flashing.
pub(crate) struct MarkdownStreamCollector {
	buffer: String,
	/// Number of lines already committed (returned to caller).
	committed_line_count: usize,
}

impl MarkdownStreamCollector {
	pub(crate) fn new() -> Self {
		Self {
			buffer: String::new(),
			committed_line_count: 0,
		}
	}

	/// Append a text delta to the internal buffer.
	pub(crate) fn push_delta(&mut self, delta: &str) {
		self.buffer.push_str(delta);
	}

	/// Return complete lines (those ending with `\n`) that haven't been
	/// committed yet. Updates the committed count.
	///
	/// Returns the committed text as a single String (may contain multiple lines).
	pub(crate) fn commit_complete_lines(&mut self) -> Option<String> {
		// Find the last newline in the buffer.
		let last_nl = self.buffer.rfind('\n')?;
		let complete = &self.buffer[..=last_nl];

		// Count how many lines exist in the complete portion.
		let total_lines = complete.lines().count();
		if total_lines <= self.committed_line_count {
			return None;
		}

		// Extract only the new lines (beyond what we've already committed).
		let new_lines: Vec<&str> = complete.lines().skip(self.committed_line_count).collect();
		self.committed_line_count = total_lines;

		if new_lines.is_empty() {
			None
		} else {
			// Rejoin with newlines (each line gets a trailing \n)
			let mut result = new_lines.join("\n");
			result.push('\n');
			Some(result)
		}
	}

	/// Flush remaining buffer content (including incomplete lines).
	/// Call this when streaming ends (e.g. on LlmDecisionComplete).
	pub(crate) fn finalize_and_drain(&mut self) -> Option<String> {
		let remaining = if !self.buffer.is_empty() {
			// Lines we haven't committed yet
			let all_lines: Vec<&str> = self.buffer.lines().collect();
			let new_lines: Vec<&str> = all_lines
				.into_iter()
				.skip(self.committed_line_count)
				.collect();

			if new_lines.is_empty() {
				None
			} else {
				Some(new_lines.join("\n"))
			}
		} else {
			None
		};

		self.clear();
		remaining
	}

	/// Reset all state.
	fn clear(&mut self) {
		self.buffer.clear();
		self.committed_line_count = 0;
	}
}

/// A line waiting to be released to the terminal.
struct QueuedLine {
	text: String,
	#[allow(dead_code)]
	enqueued_at: Instant,
}

/// Controls the release rate of committed lines to the terminal.
///
/// Two-gear policy:
/// - **Smooth**: release 1 line per tick (default 50ms interval)
/// - **CatchUp**: drain all queued lines when the queue exceeds a threshold
pub(crate) struct StreamController {
	queue: VecDeque<QueuedLine>,
	mode: ChunkingMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChunkingMode {
	Smooth,
	CatchUp,
}

impl StreamController {
	pub(crate) fn new() -> Self {
		Self {
			queue: VecDeque::new(),
			mode: ChunkingMode::Smooth,
		}
	}

	/// Enqueue committed lines for release.
	pub(crate) fn enqueue(&mut self, text: String) {
		let now = Instant::now();
		for line in text.lines() {
			self.queue.push_back(QueuedLine {
				text: format!("{line}\n"),
				enqueued_at: now,
			});
		}
		// Switch to catch-up if queue is too deep
		if self.queue.len() >= 8 {
			self.mode = ChunkingMode::CatchUp;
		}
	}

	/// Called on each tick. Returns lines to render now, or None if idle.
	pub(crate) fn on_tick(&mut self) -> Option<String> {
		if self.queue.is_empty() {
			self.mode = ChunkingMode::Smooth;
			return None;
		}

		let mut output = String::new();
		match self.mode {
			ChunkingMode::Smooth => {
				// Release one line per tick
				if let Some(queued) = self.queue.pop_front() {
					output.push_str(&queued.text);
				}
			}
			ChunkingMode::CatchUp => {
				// Drain everything to catch up
				while let Some(queued) = self.queue.pop_front() {
					output.push_str(&queued.text);
				}
				self.mode = ChunkingMode::Smooth;
			}
		}

		// Switch back to smooth if queue is small enough
		if self.queue.len() <= 2 {
			self.mode = ChunkingMode::Smooth;
		}

		if output.is_empty() {
			None
		} else {
			Some(output)
		}
	}

	/// Drain all remaining lines immediately (call at end of streaming).
	pub(crate) fn drain_all(&mut self) -> Option<String> {
		if self.queue.is_empty() {
			return None;
		}
		let mut output = String::new();
		while let Some(queued) = self.queue.pop_front() {
			output.push_str(&queued.text);
		}
		self.mode = ChunkingMode::Smooth;
		if output.is_empty() {
			None
		} else {
			Some(output)
		}
	}

	/// Whether the queue is empty and no more lines are pending.
	#[allow(dead_code)]
	pub(crate) fn is_idle(&self) -> bool {
		self.queue.is_empty()
	}
}
