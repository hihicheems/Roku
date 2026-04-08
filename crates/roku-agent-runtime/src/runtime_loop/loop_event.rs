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

/// Runtime-layer events emitted by `execute_tool_loop` via an optional event sender.
///
/// All variants are `Send + 'static` so the sender can cross async task boundaries.
#[derive(Debug, Clone)]
pub enum LoopEvent {
	/// A tool invocation is about to begin.
	ToolStart {
		/// Step index (1-based, matches `StepRecord::step_index`).
		step: u32,
		tool_name: String,
	},
	/// A tool invocation completed (successfully or with an error).
	ToolEnd {
		step: u32,
		tool_name: String,
		/// Wall-clock duration in milliseconds, if available.
		elapsed_ms: Option<u64>,
	},
	/// Context compaction was triggered after this step.
	CompactTriggered {
		step: u32,
		/// Estimated token count that caused the trigger.
		estimated_tokens: u64,
	},
	/// Incremental text from the LLM during the decision phase.
	LlmTextDelta { step: u32, text: String },
	/// The LLM finished producing its decision for this step.
	LlmDecisionComplete { step: u32 },
	/// One full loop iteration (decide + optional tool execution) is complete.
	StepComplete { step: u32 },
}

/// Convenience alias for the sending half of a `LoopEvent` channel.
pub type LoopEventSender = tokio::sync::mpsc::UnboundedSender<LoopEvent>;
