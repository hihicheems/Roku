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

//! Shared conversation-history utilities used by both the CLI chat REPL and the Telegram handler.

use std::time::{SystemTime, UNIX_EPOCH};

use roku_common_types::{ConversationRole, ConversationTurn};

/// Compact result returned after compacting a conversation history.
pub(crate) struct CompactResult {
	/// Number of turns that were summarized and removed.
	pub discarded: usize,
	/// Number of turns that remain after compaction (including the new summary turn).
	pub retained: usize,
}

/// Deterministic compaction: keep the last few turns, summarize older ones into a single
/// system turn.
///
/// Returns [`None`] when the history is already short enough that nothing was discarded,
/// or [`Some(CompactResult)`] describing what happened.
pub(crate) fn compact_conversation_history(
	history: &mut Vec<ConversationTurn>,
) -> Option<CompactResult> {
	const RETAIN_TAIL: usize = 6; // Keep last 3 user-assistant pairs

	if history.len() <= RETAIN_TAIL {
		return None;
	}

	let split_at = history.len() - RETAIN_TAIL;
	let discarded: Vec<_> = history.drain(..split_at).collect();

	// Build a deterministic one-line-per-turn summary.
	let mut summary_lines = Vec::new();
	for turn in &discarded {
		let role = match turn.role {
			ConversationRole::User => "User",
			ConversationRole::Assistant => "Assistant",
			ConversationRole::System => "System",
		};
		// Truncate long content for the summary.
		let preview: String = turn.content.chars().take(120).collect();
		let ellipsis = if turn.content.chars().count() > 120 {
			"..."
		} else {
			""
		};
		summary_lines.push(format!("- {role}: {preview}{ellipsis}"));
	}

	let summary = format!(
		"[Compacted {} earlier turns]\n{}",
		discarded.len(),
		summary_lines.join("\n")
	);

	// Insert the summary as a System turn at position 0.
	history.insert(
		0,
		ConversationTurn {
			role: ConversationRole::System,
			content: summary,
			created_at_unix_ms: now_unix_ms(),
		},
	);

	let discarded_count = discarded.len();
	let retained = history.len();
	Some(CompactResult {
		discarded: discarded_count,
		retained,
	})
}

fn now_unix_ms() -> u64 {
	SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.map(|d| d.as_millis().min(u64::MAX as u128) as u64)
		.unwrap_or(0)
}

#[cfg(test)]
mod tests {
	use roku_common_types::ConversationRole;

	use super::*;

	fn user_turn(content: &str) -> ConversationTurn {
		ConversationTurn {
			role: ConversationRole::User,
			content: content.to_string(),
			created_at_unix_ms: 1,
		}
	}

	fn assistant_turn(content: &str) -> ConversationTurn {
		ConversationTurn {
			role: ConversationRole::Assistant,
			content: content.to_string(),
			created_at_unix_ms: 2,
		}
	}

	#[test]
	fn compact_returns_none_when_history_is_short() {
		let mut history = vec![user_turn("hi"), assistant_turn("hello")];
		let result = compact_conversation_history(&mut history);
		assert!(result.is_none());
		assert_eq!(history.len(), 2);
	}

	#[test]
	fn compact_returns_none_when_history_is_exactly_retain_tail() {
		let mut history = (0..6)
			.map(|i| user_turn(&format!("msg {i}")))
			.collect::<Vec<_>>();
		let result = compact_conversation_history(&mut history);
		assert!(result.is_none());
		assert_eq!(history.len(), 6);
	}

	#[test]
	fn compact_discards_older_turns_and_inserts_summary() {
		let mut history = (0..10)
			.map(|i| user_turn(&format!("msg {i}")))
			.collect::<Vec<_>>();
		let result = compact_conversation_history(&mut history).expect("should compact");
		// 10 - 6 = 4 discarded, then 1 summary + 6 tail = 7 retained
		assert_eq!(result.discarded, 4);
		assert_eq!(result.retained, 7);
		assert_eq!(history.len(), 7);
		assert_eq!(history[0].role, ConversationRole::System);
		assert!(history[0].content.contains("[Compacted 4 earlier turns]"));
		// Last 6 original turns preserved after the summary
		assert_eq!(history[1].content, "msg 4");
		assert_eq!(history[6].content, "msg 9");
	}

	#[test]
	fn compact_truncates_long_turn_content_in_summary() {
		let long_content = "x".repeat(200);
		let mut history: Vec<ConversationTurn> = (0..7).map(|_| user_turn(&long_content)).collect();
		let result = compact_conversation_history(&mut history).expect("should compact");
		assert_eq!(result.discarded, 1);
		assert!(history[0].content.contains("..."));
	}
}
