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

//! Per-provider byte→token counting abstraction.
//!
//! A single whole-history byte heuristic cannot fit prose, structured JSON,
//! and tool schemas simultaneously while leaving room for a calibration EMA
//! to finish the job. Live runs on the OpenAI Responses link hold a steady
//! +30% bias because the shared estimator is insensitive to which provider
//! is about to handle the request.
//!
//! This module introduces a [`TokenCounter`] trait that providers own. The
//! default implementation [`ByteHeuristicCounter`] uses a uniform
//! `bytes / N` rule (default `N = 4`), which tracks the well-known
//! rule-of-thumb for ASCII English and is what the most widely-deployed
//! agent CLIs use as their pre-flight approximation. Providers that have
//! access to a tokenizer or a `/count_tokens` endpoint can override this
//! trait with a higher-fidelity implementation without touching the runtime.

use std::sync::Arc;

use crate::types::Message;

/// Byte→token counting contract owned by each [`crate::LlmProvider`].
///
/// Implementations must be deterministic, O(n) over the input byte length,
/// and must not panic. Methods take individual message and tool structures
/// so implementations can inspect structure (e.g. to separate natural-language
/// fields from JSON scaffolding); the default [`ByteHeuristicCounter`] treats
/// the entire serialized byte sequence uniformly and ignores the split.
pub trait TokenCounter: Send + Sync {
	/// Identifier used in trace / diagnostic output. Should be stable across
	/// runs and unique across implementations.
	fn name(&self) -> &'static str;

	/// Count tokens for a free-form text blob (system prompt, tool result
	/// body, user content). Zero-length input must return 0.
	fn count_text(&self, text: &str) -> u64;

	/// Count tokens for a single [`Message`]. Default implementation sums
	/// `count_text` over every text-bearing field in the message.
	fn count_message(&self, msg: &Message) -> u64 {
		match msg {
			Message::User { content } => self.count_text(content),
			Message::Assistant { text, tool_calls } => {
				let mut total = self.count_text(text);
				for tc in tool_calls {
					total = total.saturating_add(self.count_text(&tc.name));
					let args = tc.arguments.to_string();
					total = total.saturating_add(self.count_text(&args));
				}
				total
			}
			Message::ToolResult { content, .. } => self.count_text(content),
		}
	}

	/// Count tokens for the serialized tool-schema block that this
	/// provider will send on the wire. Callers pass the exact byte
	/// sequence produced by
	/// [`crate::LlmProvider::preview_wire_tool_schema_bytes`] so the count
	/// matches what the tokenizer will see.
	///
	/// Default implementation treats the bytes as text and applies
	/// [`Self::count_text`]. Providers whose tool-schema byte/token ratio
	/// differs markedly from their prose ratio (notably OpenAI, whose
	/// `description`-heavy schemas tokenize at ~7 bytes/token) should
	/// override this method to apply a narrower divisor.
	fn count_tool_schema_bytes(&self, bytes: &[u8]) -> u64 {
		let text = std::str::from_utf8(bytes).unwrap_or("");
		self.count_text(text)
	}
}

/// `bytes / N` counter with independent divisors for free-form text and
/// tool-schema bytes.
///
/// `bytes_per_token` (default 4) applies to system prompt, user / assistant
/// messages, and tool-result bodies — matching the widely-used pre-flight
/// approximation for OpenAI-family tokenizers on mixed English and code.
///
/// `tool_schema_bytes_per_token` (default: same as `bytes_per_token`)
/// lets a provider use a different divisor for the serialized tool-schema
/// block. This exists because real OpenAI tool schemas are ~60% natural
/// English (`description` fields) wrapped in JSON scaffolding that
/// tokenizes into single tokens in cl100k / o200k, so the real ratio is
/// ~7-8 bytes/token on representative surfaces. Uniform 4/byte overshoots
/// the tool-schema portion by ~2x; a dedicated `bytes/5` rule lands in
/// the calibration band.
#[derive(Debug, Clone)]
pub struct ByteHeuristicCounter {
	bytes_per_token: u64,
	tool_schema_bytes_per_token: u64,
	name: &'static str,
}

impl ByteHeuristicCounter {
	/// Construct a counter with an explicit divisor for all byte streams.
	/// Values below 1 are clamped to 1 to avoid division by zero; sensible
	/// range is 2-8.
	pub fn with_bytes_per_token(bytes_per_token: u64) -> Self {
		let clamped = bytes_per_token.max(1);
		Self {
			bytes_per_token: clamped,
			tool_schema_bytes_per_token: clamped,
			name: "byte-heuristic",
		}
	}

	/// Override the divisor used specifically for tool-schema bytes. Use
	/// this when a provider's tool-schema surface has a noticeably
	/// different byte/token ratio than prose or code — for example OpenAI,
	/// where prose-heavy `description` fields tokenize at ~7-8 bytes/token.
	pub fn with_tool_schema_bytes_per_token(mut self, divisor: u64) -> Self {
		self.tool_schema_bytes_per_token = divisor.max(1);
		self
	}

	/// Construct a counter with a named identifier for trace/diagnostic
	/// output. Useful when a provider wants to distinguish its override
	/// from the trait default in logs.
	pub fn with_name(mut self, name: &'static str) -> Self {
		self.name = name;
		self
	}

	/// Effective divisor used for messages / system prompt / tool results.
	pub fn bytes_per_token(&self) -> u64 {
		self.bytes_per_token
	}

	/// Effective divisor used for the serialized tool-schema block.
	pub fn tool_schema_bytes_per_token(&self) -> u64 {
		self.tool_schema_bytes_per_token
	}
}

impl Default for ByteHeuristicCounter {
	fn default() -> Self {
		Self::with_bytes_per_token(4)
	}
}

impl TokenCounter for ByteHeuristicCounter {
	fn name(&self) -> &'static str {
		self.name
	}

	fn count_text(&self, text: &str) -> u64 {
		let len = text.len() as u64;
		if len == 0 {
			return 0;
		}
		len.div_ceil(self.bytes_per_token)
	}

	fn count_tool_schema_bytes(&self, bytes: &[u8]) -> u64 {
		let len = bytes.len() as u64;
		if len == 0 {
			return 0;
		}
		len.div_ceil(self.tool_schema_bytes_per_token)
	}
}

/// Convenience constructor that returns the trait default as an
/// `Arc<dyn TokenCounter>`. Providers whose `token_counter()` method
/// wants to return the default without allocating a new `Arc` on every
/// call should cache the value at construction time.
pub fn default_counter() -> Arc<dyn TokenCounter> {
	Arc::new(ByteHeuristicCounter::default())
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::types::ToolCallBlock;
	use serde_json::json;

	#[test]
	fn byte_heuristic_counter_default_uses_4_bytes_per_token_like_codex() {
		let counter = ByteHeuristicCounter::default();
		assert_eq!(counter.bytes_per_token(), 4);
		// Same formula as codex's APPROX_BYTES_PER_TOKEN path:
		// (bytes + 3) / 4 is `div_ceil(4)`.
		assert_eq!(counter.count_text("a"), 1);
		assert_eq!(counter.count_text("abcd"), 1);
		assert_eq!(counter.count_text("abcde"), 2);
		assert_eq!(counter.count_text(""), 0);
	}

	#[test]
	fn byte_heuristic_counter_with_bytes_per_token_clamps_zero_to_one() {
		let counter = ByteHeuristicCounter::with_bytes_per_token(0);
		assert_eq!(counter.bytes_per_token(), 1);
		// With divisor 1 every byte is a token — still non-panicking.
		assert_eq!(counter.count_text("abcd"), 4);
	}

	#[test]
	fn count_message_sums_user_content() {
		let counter = ByteHeuristicCounter::default();
		let msg = Message::User {
			content: "x".repeat(40),
		};
		assert_eq!(counter.count_message(&msg), 10);
	}

	#[test]
	fn count_message_assistant_sums_text_and_tool_calls() {
		let counter = ByteHeuristicCounter::default();
		let msg = Message::Assistant {
			text: "hello world".to_string(),
			tool_calls: vec![ToolCallBlock {
				id: "t1".to_string(),
				name: "read_file".to_string(),
				arguments: json!({"path": "/tmp/foo"}),
			}],
		};
		let text_tokens = counter.count_text("hello world");
		let name_tokens = counter.count_text("read_file");
		let args_tokens = counter.count_text(r#"{"path":"/tmp/foo"}"#);
		assert_eq!(
			counter.count_message(&msg),
			text_tokens + name_tokens + args_tokens
		);
	}

	#[test]
	fn count_message_tool_result_uses_content_only() {
		let counter = ByteHeuristicCounter::default();
		let msg = Message::ToolResult {
			tool_use_id: "call_1".to_string(),
			content: "abcdefgh".to_string(),
			is_error: false,
		};
		assert_eq!(counter.count_message(&msg), 2);
	}

	#[test]
	fn default_counter_returns_arc_dyn() {
		let counter = default_counter();
		assert_eq!(counter.name(), "byte-heuristic");
		assert_eq!(counter.count_text("1234567890"), 3);
	}

	/// Custom counter type that overrides only `count_text` exercises
	/// the default-method path for `count_message` and `count_tool_schema_bytes`.
	struct HalfCounter;
	impl TokenCounter for HalfCounter {
		fn name(&self) -> &'static str {
			"half"
		}
		fn count_text(&self, text: &str) -> u64 {
			(text.len() as u64).div_ceil(2)
		}
	}

	#[test]
	fn provider_override_propagates_through_default_methods() {
		let counter = HalfCounter;
		// count_message calls count_text internally
		let msg = Message::User {
			content: "abcd".to_string(),
		};
		assert_eq!(counter.count_message(&msg), 2);
	}

	#[test]
	fn tool_schema_bytes_per_token_overrides_the_text_divisor() {
		// OpenAI-style tuning: prose / messages stay at 4 bytes/token,
		// tool schema at 5 bytes/token because OpenAI schemas are mostly
		// natural-language `description` fields.
		let counter =
			ByteHeuristicCounter::with_bytes_per_token(4).with_tool_schema_bytes_per_token(5);
		assert_eq!(counter.bytes_per_token(), 4);
		assert_eq!(counter.tool_schema_bytes_per_token(), 5);
		let schema = b"[{\"name\":\"read\",\"description\":\"reads a file\"}]";
		let by_text = counter.count_text(std::str::from_utf8(schema).unwrap());
		let by_schema = counter.count_tool_schema_bytes(schema);
		// Text divisor=4, schema divisor=5 → schema count must be smaller
		// than text count for the same bytes.
		assert!(
			by_schema < by_text,
			"schema divisor 5 should produce fewer tokens than text divisor 4 \
			 (text={by_text}, schema={by_schema})"
		);
	}

	#[test]
	fn count_tool_schema_bytes_default_matches_count_text() {
		// Default `ByteHeuristicCounter` (no explicit schema override) uses
		// the same divisor everywhere — a regression guard so a future
		// constructor change cannot silently diverge the two paths.
		let counter = ByteHeuristicCounter::default();
		let schema = b"[{\"name\":\"echo\",\"description\":\"echo input\"}]";
		let by_text = counter.count_text(std::str::from_utf8(schema).unwrap());
		let by_schema = counter.count_tool_schema_bytes(schema);
		assert_eq!(by_text, by_schema);
	}
}
