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

use std::collections::HashMap;
use std::path::PathBuf;

/// Preview size limit: first 2KB of content shown in context.
const PREVIEW_SIZE: usize = 2_048;

/// Three-state machine for tool result content replacement.
///
/// - `Fresh`: content just produced this turn. Disk path and preview are
///   computed at registration time so `advance_turn` needs no extra context.
/// - `Frozen`: content has been persisted to disk and replaced with preview.
/// - `MustReapply`: on subsequent turns, the preview bytes must be reapplied
///   identically to preserve cache prefix stability.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ContentReplacementState {
	/// Content produced this turn. Preview and disk path already computed.
	Fresh { preview: String, disk_path: PathBuf },
	/// Content persisted to disk, preview in context. Bytes are stable.
	Frozen { preview: String, disk_path: PathBuf },
	/// Reapply the frozen preview bytes on subsequent turns.
	MustReapply { preview: String, disk_path: PathBuf },
}

/// Per-run store that tracks tool result content replacement state.
///
/// Keyed by `tool_use_id`. The store persists full tool content to disk
/// and produces stable preview replacements for the LLM context.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct ToolResultStore {
	entries: HashMap<String, ContentReplacementState>,
}

impl ToolResultStore {
	/// Register a new tool result. If the content exceeds `PREVIEW_SIZE`,
	/// persist to disk and return a preview; otherwise return the original content.
	///
	/// Returns `(content_for_context, is_preview)`.
	pub(crate) fn register(
		&mut self,
		tool_use_id: &str,
		content: &str,
		run_id: &str,
	) -> (String, bool) {
		// Small results don't need disk persistence.
		if content.len() <= PREVIEW_SIZE {
			return (content.to_string(), false);
		}

		// Already frozen/must_reapply — return the stable preview.
		if let Some(state) = self.entries.get(tool_use_id) {
			match state {
				ContentReplacementState::Frozen { preview, .. }
				| ContentReplacementState::MustReapply { preview, .. } => {
					return (preview.clone(), true);
				}
				ContentReplacementState::Fresh { preview, .. } => {
					// Same tool_use_id called twice in one turn — return existing preview.
					return (preview.clone(), true);
				}
			}
		}

		// Persist to disk and create preview.
		let disk_path = tool_result_disk_path(run_id, tool_use_id);
		let preview = build_preview(content, &disk_path);

		// Attempt disk write; on failure, fall back to full content in context.
		if let Err(e) = write_tool_result_to_disk(&disk_path, content) {
			let _ = roku_common_types::emit_global_log(roku_common_types::LogRecord::new(
				"roku-agent-runtime",
				roku_common_types::LogLevel::Warn,
				format!("tool result disk write failed: {e}; keeping full content in context"),
			));
			return (content.to_string(), false);
		}

		self.entries.insert(
			tool_use_id.to_string(),
			ContentReplacementState::Fresh {
				preview: preview.clone(),
				disk_path,
			},
		);

		(preview, true)
	}

	/// Advance all Fresh entries to Frozen, and Frozen to MustReapply.
	/// Called at the end of each turn.
	pub(crate) fn advance_turn(&mut self) {
		let keys: Vec<String> = self.entries.keys().cloned().collect();
		for key in keys {
			if let Some(state) = self.entries.remove(&key) {
				let new_state = match state {
					ContentReplacementState::Fresh { preview, disk_path } => {
						ContentReplacementState::Frozen { preview, disk_path }
					}
					ContentReplacementState::Frozen { preview, disk_path } => {
						ContentReplacementState::MustReapply { preview, disk_path }
					}
					ContentReplacementState::MustReapply { preview, disk_path } => {
						ContentReplacementState::MustReapply { preview, disk_path }
					}
				};
				self.entries.insert(key, new_state);
			}
		}
	}

	/// Get the preview for a tool_use_id if it exists in Frozen/MustReapply state.
	#[allow(dead_code)]
	pub(crate) fn get_preview(&self, tool_use_id: &str) -> Option<&str> {
		self.entries.get(tool_use_id).and_then(|state| match state {
			ContentReplacementState::Frozen { preview, .. }
			| ContentReplacementState::MustReapply { preview, .. } => Some(preview.as_str()),
			ContentReplacementState::Fresh { .. } => None,
		})
	}
}

/// Build the disk path for a tool result file.
fn tool_result_disk_path(run_id: &str, tool_use_id: &str) -> PathBuf {
	let home = std::env::var("HOME")
		.or_else(|_| std::env::var("USERPROFILE"))
		.unwrap_or_else(|_| "/tmp".to_string());
	PathBuf::from(home)
		.join(".roku")
		.join("tool-results")
		.join(run_id)
		.join(format!("{tool_use_id}.txt"))
}

/// Build a preview string from full content + disk path reference.
fn build_preview(content: &str, disk_path: &std::path::Path) -> String {
	let preview_end = content
		.char_indices()
		.nth(PREVIEW_SIZE)
		.map(|(i, _)| i)
		.unwrap_or(content.len());
	let preview_text = &content[..preview_end];
	format!(
		"{preview_text}\n\n[Full content ({} chars) saved to disk: {}]",
		content.len(),
		disk_path.display(),
	)
}

/// Write full content to disk. Creates parent directories if needed.
fn write_tool_result_to_disk(path: &PathBuf, content: &str) -> Result<(), std::io::Error> {
	if let Some(parent) = path.parent() {
		std::fs::create_dir_all(parent)?;
	}
	std::fs::write(path, content)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn small_content_not_persisted() {
		let mut store = ToolResultStore::default();
		let (content, is_preview) = store.register("tool-1", "short content", "run-1");
		assert_eq!(content, "short content");
		assert!(!is_preview);
		assert!(store.entries.is_empty());
	}

	#[test]
	fn large_content_produces_preview() {
		let mut store = ToolResultStore::default();
		let large = "X".repeat(5000);
		let (content, is_preview) = store.register("tool-1", &large, "test-run");
		assert!(is_preview);
		assert!(content.len() < large.len());
		assert!(content.contains("[Full content (5000 chars) saved to disk:"));
		assert!(content.contains("tool-results/test-run/tool-1.txt"));
	}

	#[test]
	fn frozen_preview_is_byte_identical_across_calls() {
		let mut store = ToolResultStore::default();
		let large = "Y".repeat(5000);
		let run_id = "test-run-2";

		// First call — Fresh
		let (preview1, _) = store.register("tool-2", &large, run_id);

		// Manually transition to Frozen state.
		let disk_path = tool_result_disk_path(run_id, "tool-2");
		let preview = build_preview(&large, &disk_path);
		store.entries.insert(
			"tool-2".to_string(),
			ContentReplacementState::Frozen {
				preview: preview.clone(),
				disk_path: disk_path.clone(),
			},
		);

		// Second call — should return identical preview
		let (preview2, _) = store.register("tool-2", &large, run_id);
		assert_eq!(preview1, preview2);
		assert_eq!(preview2, preview);
	}

	#[test]
	fn advance_turn_transitions_states() {
		let mut store = ToolResultStore::default();
		let disk_path = PathBuf::from("/tmp/test.txt");
		let preview = "preview text".to_string();

		store.entries.insert(
			"t1".to_string(),
			ContentReplacementState::Fresh {
				preview: preview.clone(),
				disk_path: disk_path.clone(),
			},
		);
		store.entries.insert(
			"t2".to_string(),
			ContentReplacementState::Frozen {
				preview: preview.clone(),
				disk_path: disk_path.clone(),
			},
		);
		store.entries.insert(
			"t3".to_string(),
			ContentReplacementState::MustReapply {
				preview: preview.clone(),
				disk_path: disk_path.clone(),
			},
		);

		store.advance_turn();

		// Fresh -> Frozen
		assert!(matches!(
			store.entries.get("t1"),
			Some(ContentReplacementState::Frozen { .. })
		));
		// Frozen -> MustReapply
		assert!(matches!(
			store.entries.get("t2"),
			Some(ContentReplacementState::MustReapply { .. })
		));
		// MustReapply -> MustReapply (stable)
		assert!(matches!(
			store.entries.get("t3"),
			Some(ContentReplacementState::MustReapply { .. })
		));
	}

	#[test]
	fn preview_contains_disk_path() {
		let path = PathBuf::from("/home/user/.roku/tool-results/run-1/tool-1.txt");
		let preview = build_preview(&"Z".repeat(3000), &path);
		assert!(preview.contains("/home/user/.roku/tool-results/run-1/tool-1.txt"));
		assert!(preview.contains("[Full content (3000 chars)"));
	}

	#[test]
	fn get_preview_returns_none_for_fresh_state() {
		let mut store = ToolResultStore::default();
		let disk_path = PathBuf::from("/tmp/test.txt");
		store.entries.insert(
			"t1".to_string(),
			ContentReplacementState::Fresh {
				preview: "preview".to_string(),
				disk_path,
			},
		);
		// Fresh state is not returned by get_preview
		assert_eq!(store.get_preview("t1"), None);
		assert_eq!(store.get_preview("nonexistent"), None);
	}

	#[test]
	fn get_preview_returns_some_for_frozen_and_must_reapply() {
		let mut store = ToolResultStore::default();
		let disk_path = PathBuf::from("/tmp/test.txt");

		store.entries.insert(
			"t2".to_string(),
			ContentReplacementState::Frozen {
				preview: "frozen preview".to_string(),
				disk_path: disk_path.clone(),
			},
		);
		store.entries.insert(
			"t3".to_string(),
			ContentReplacementState::MustReapply {
				preview: "reapply preview".to_string(),
				disk_path,
			},
		);

		assert_eq!(store.get_preview("t2"), Some("frozen preview"));
		assert_eq!(store.get_preview("t3"), Some("reapply preview"));
	}
}
