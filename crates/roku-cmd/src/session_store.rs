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

//! File-backed conversation history store using JSONL format.
//!
//! Each session is stored as a JSONL file (one `ConversationTurn` per line) under
//! `{session_history_dir}/{session_id}.jsonl`. Append-only writes make the format
//! streaming-friendly and crash-resilient.
//!
//! In addition to `ConversationTurn` entries, the JSONL file may contain metadata
//! entries serialized as `SessionEntry`. These use an internally-tagged `"type"` field
//! so they are skipped by the existing `load()` parser without breaking backward
//! compatibility.

use std::fs;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use roku_common_types::ConversationTurn;

use crate::storage::LocalStorageLayout;

/// A session store entry — either a conversation turn or a metadata entry.
///
/// Uses internally-tagged serde so all variants coexist in the same JSONL stream.
/// Metadata entries appended to the session JSONL alongside raw `ConversationTurn`
/// lines. Uses an internally-tagged `"type"` field so `load()` (which deserializes
/// to `ConversationTurn`) safely skips these lines with a warning.
///
/// **Important**: Do NOT add a `Turn` wrapper variant — serializing it would inject
/// a `"type"` field that `load()` cannot parse, silently dropping conversation data.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub(crate) enum SessionEntry {
	/// Compact boundary marker written after a compaction event.
	#[serde(rename = "compact_boundary")]
	CompactBoundary {
		timestamp_ms: u64,
		summary_turn_index: usize,
		discarded_turns: usize,
		retained_turns: usize,
	},
	/// Token usage metadata for a single turn.
	#[serde(rename = "token_usage")]
	TokenUsage {
		timestamp_ms: u64,
		prompt_tokens: u64,
		output_tokens: u64,
		model_id: Option<String>,
		session_prompt_total: u64,
		session_output_total: u64,
	},
}

/// Metadata about a stored session.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct SessionInfo {
	pub session_id: String,
	pub turn_count: usize,
	pub last_modified: u64,
	/// First user message content (truncated for display).
	pub first_message: Option<String>,
}

/// File-backed conversation history store.
pub(crate) struct SessionStore {
	root: PathBuf,
}

impl SessionStore {
	pub fn new(root: PathBuf) -> Self {
		Self { root }
	}

	fn session_path(&self, session_id: &str) -> Result<PathBuf, std::io::Error> {
		validate_session_id(session_id)?;
		Ok(self.root.join(format!("{session_id}.jsonl")))
	}

	/// Load conversation turns after the last compact boundary.
	///
	/// If the session has a compact boundary marker, only returns turns that
	/// appear after the last boundary. If no boundary exists, returns all turns
	/// (same as `load`).
	///
	/// **Note**: With Roku's current destructive-rewrite compaction model, the
	/// file is already rewritten with only summary + retained turns before the
	/// boundary is appended. Callers that need the full compacted context
	/// (including the summary) should use `load()` instead. This method is
	/// reserved for future append-only chain support where pre-boundary content
	/// genuinely represents discarded history.
	#[allow(dead_code)]
	pub fn load_after_boundary(
		&self,
		session_id: &str,
	) -> Result<Vec<ConversationTurn>, std::io::Error> {
		let path = self.session_path(session_id)?;
		if !path.exists() {
			return Ok(Vec::new());
		}

		let file = fs::File::open(&path)?;
		let reader = std::io::BufReader::new(file);
		let mut all_turns = Vec::new();
		let mut last_boundary_index: Option<usize> = None;
		let mut line_index: usize = 0;

		for line in reader.lines() {
			let line = line?;
			let trimmed = line.trim();
			if trimmed.is_empty() {
				continue;
			}
			if trimmed.contains(r#""type":"#) {
				if trimmed.contains(r#""compact_boundary""#) {
					last_boundary_index = Some(line_index);
				}
				line_index += 1;
				continue;
			}
			match serde_json::from_str::<ConversationTurn>(trimmed) {
				Ok(turn) => all_turns.push((line_index, turn)),
				Err(e) => {
					eprintln!("[warn] skipping malformed line in session {session_id}: {e}");
				}
			}
			line_index += 1;
		}

		match last_boundary_index {
			Some(boundary) => Ok(all_turns
				.into_iter()
				.filter(|(idx, _)| *idx > boundary)
				.map(|(_, turn)| turn)
				.collect()),
			None => Ok(all_turns.into_iter().map(|(_, turn)| turn).collect()),
		}
	}

	/// Load all conversation turns for a session.  Returns an empty vec if the
	/// session file does not exist.
	pub fn load(&self, session_id: &str) -> Result<Vec<ConversationTurn>, std::io::Error> {
		let path = self.session_path(session_id)?;
		if !path.exists() {
			return Ok(Vec::new());
		}

		let file = fs::File::open(&path)?;
		let reader = std::io::BufReader::new(file);
		let mut turns = Vec::new();

		for line in reader.lines() {
			let line = line?;
			let trimmed = line.trim();
			if trimmed.is_empty() {
				continue;
			}
			// Silently skip metadata entries (compact_boundary, token_usage)
			// which carry a "type" field that ConversationTurn doesn't expect.
			if trimmed.contains(r#""type":"#) {
				continue;
			}
			match serde_json::from_str::<ConversationTurn>(trimmed) {
				Ok(turn) => turns.push(turn),
				Err(e) => {
					eprintln!("[warn] skipping malformed line in session {session_id}: {e}");
				}
			}
		}

		Ok(turns)
	}

	/// Append one or more turns to a session file.
	pub fn append(
		&self,
		session_id: &str,
		turns: &[ConversationTurn],
	) -> Result<(), std::io::Error> {
		if turns.is_empty() {
			return Ok(());
		}
		fs::create_dir_all(&self.root)?;
		let path = self.session_path(session_id)?;
		let mut file = fs::OpenOptions::new()
			.create(true)
			.append(true)
			.open(&path)?;
		for turn in turns {
			let json = serde_json::to_string(turn)
				.map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
			writeln!(file, "{json}")?;
		}
		Ok(())
	}

	/// Append raw session entries (metadata, boundaries, etc.) to a session file.
	pub fn append_entries(
		&self,
		session_id: &str,
		entries: &[SessionEntry],
	) -> Result<(), std::io::Error> {
		if entries.is_empty() {
			return Ok(());
		}
		fs::create_dir_all(&self.root)?;
		let path = self.session_path(session_id)?;
		let mut file = fs::OpenOptions::new()
			.create(true)
			.append(true)
			.open(&path)?;
		for entry in entries {
			let json = serde_json::to_string(entry)
				.map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
			writeln!(file, "{json}")?;
		}
		Ok(())
	}

	/// List all sessions with metadata.
	pub fn list(&self) -> Result<Vec<SessionInfo>, std::io::Error> {
		if !self.root.exists() {
			return Ok(Vec::new());
		}

		let mut sessions = Vec::new();
		for entry in fs::read_dir(&self.root)? {
			let entry = entry?;
			let path = entry.path();
			if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
				continue;
			}
			let session_id = path
				.file_stem()
				.and_then(|s| s.to_str())
				.unwrap_or("")
				.to_string();
			if session_id.is_empty() {
				continue;
			}
			let turn_count = count_lines(&path).unwrap_or(0);
			let last_modified = file_mtime_unix_ms(&path);
			let first_message = first_turn_summary(&path);
			sessions.push(SessionInfo {
				session_id,
				turn_count,
				last_modified,
				first_message,
			});
		}
		sessions.sort_by_key(|session| std::cmp::Reverse(session.last_modified));
		Ok(sessions)
	}

	/// Delete a session's history file.
	pub fn delete(&self, session_id: &str) -> Result<bool, std::io::Error> {
		let path = self.session_path(session_id)?;
		if path.exists() {
			fs::remove_file(&path)?;
			Ok(true)
		} else {
			Ok(false)
		}
	}

	/// Clear a session's history (truncate the file).
	pub fn clear(&self, session_id: &str) -> Result<(), std::io::Error> {
		let path = self.session_path(session_id)?;
		if path.exists() {
			fs::remove_file(&path)?;
		}
		Ok(())
	}
}

/// Reject session IDs that could escape the store directory.
fn validate_session_id(session_id: &str) -> Result<(), std::io::Error> {
	if session_id.is_empty()
		|| session_id.contains('/')
		|| session_id.contains('\\')
		|| session_id.contains("..")
		|| session_id.starts_with('.')
	{
		return Err(std::io::Error::new(
			std::io::ErrorKind::InvalidInput,
			format!("invalid session id: {session_id:?}"),
		));
	}
	Ok(())
}

/// Atomically rewrite the session file with the given history (used after compaction).
///
/// Writes to a temporary sibling file first, then renames over the original, so a
/// crash mid-write does not destroy the existing file.
pub(crate) fn rewrite_history(
	store: &SessionStore,
	session_id: &str,
	history: &[ConversationTurn],
) -> Result<(), std::io::Error> {
	let layout = LocalStorageLayout::from_env();
	let dir = &layout.session_history_dir;
	std::fs::create_dir_all(dir)?;

	let target = dir.join(format!("{session_id}.jsonl"));
	let tmp = dir.join(format!("{session_id}.jsonl.tmp"));

	// Write to temp file.
	{
		let mut file = std::fs::File::create(&tmp)?;
		for turn in history {
			let json = serde_json::to_string(turn)
				.map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
			writeln!(file, "{json}")?;
		}
		file.flush()?;
	}

	// Atomic rename over original.
	std::fs::rename(&tmp, &target)?;

	// Suppress unused-variable warning — store is still needed for other operations,
	// but rewrite bypasses it for atomicity.
	let _ = store;

	Ok(())
}

/// Count only conversation turn lines, skipping metadata entries.
fn count_lines(path: &Path) -> Result<usize, std::io::Error> {
	let file = fs::File::open(path)?;
	let reader = std::io::BufReader::new(file);
	let mut count = 0;
	for line in reader.lines() {
		let line = line?;
		let trimmed = line.trim();
		if trimmed.is_empty() || trimmed.contains(r#""type":"#) {
			continue;
		}
		count += 1;
	}
	Ok(count)
}

/// Extract a truncated summary of the first conversation turn in a session file.
fn first_turn_summary(path: &Path) -> Option<String> {
	let file = fs::File::open(path).ok()?;
	let reader = std::io::BufReader::new(file);
	for line in reader.lines() {
		let line = line.ok()?;
		let trimmed = line.trim();
		if trimmed.is_empty() || trimmed.contains(r#""type":"#) {
			continue;
		}
		if let Ok(turn) = serde_json::from_str::<ConversationTurn>(trimmed) {
			let content = turn.content.trim();
			if content.is_empty() {
				continue;
			}
			// Truncate to ~60 chars for display.
			let summary: String = content.chars().take(60).collect();
			if content.chars().count() > 60 {
				return Some(format!("{summary}…"));
			}
			return Some(summary);
		}
	}
	None
}

fn file_mtime_unix_ms(path: &Path) -> u64 {
	fs::metadata(path)
		.and_then(|m| m.modified())
		.ok()
		.and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
		.map(|d| d.as_millis().min(u64::MAX as u128) as u64)
		.unwrap_or(0)
}

#[cfg(test)]
mod tests {
	use super::*;
	use roku_common_types::ConversationRole;

	fn make_turn(role: ConversationRole, content: &str) -> ConversationTurn {
		ConversationTurn {
			role,
			content: content.to_string(),
			created_at_unix_ms: 1000,
		}
	}

	#[test]
	fn roundtrip_append_and_load() {
		let dir = tempfile::tempdir().unwrap();
		let store = SessionStore::new(dir.path().to_path_buf());

		let turns = vec![
			make_turn(ConversationRole::User, "hello"),
			make_turn(ConversationRole::Assistant, "hi there"),
		];
		store.append("test-session", &turns).unwrap();

		let loaded = store.load("test-session").unwrap();
		assert_eq!(loaded.len(), 2);
		assert_eq!(loaded[0].content, "hello");
		assert_eq!(loaded[1].content, "hi there");
	}

	#[test]
	fn append_is_incremental() {
		let dir = tempfile::tempdir().unwrap();
		let store = SessionStore::new(dir.path().to_path_buf());

		store
			.append("s1", &[make_turn(ConversationRole::User, "first")])
			.unwrap();
		store
			.append("s1", &[make_turn(ConversationRole::Assistant, "second")])
			.unwrap();

		let loaded = store.load("s1").unwrap();
		assert_eq!(loaded.len(), 2);
		assert_eq!(loaded[0].content, "first");
		assert_eq!(loaded[1].content, "second");
	}

	#[test]
	fn load_nonexistent_returns_empty() {
		let dir = tempfile::tempdir().unwrap();
		let store = SessionStore::new(dir.path().to_path_buf());
		let loaded = store.load("nonexistent").unwrap();
		assert!(loaded.is_empty());
	}

	#[test]
	fn list_sessions() {
		let dir = tempfile::tempdir().unwrap();
		let store = SessionStore::new(dir.path().to_path_buf());

		store
			.append("a", &[make_turn(ConversationRole::User, "hello")])
			.unwrap();
		store
			.append(
				"b",
				&[
					make_turn(ConversationRole::User, "one"),
					make_turn(ConversationRole::Assistant, "two"),
				],
			)
			.unwrap();

		let sessions = store.list().unwrap();
		assert_eq!(sessions.len(), 2);
		let ids: Vec<&str> = sessions.iter().map(|s| s.session_id.as_str()).collect();
		assert!(ids.contains(&"a"));
		assert!(ids.contains(&"b"));
	}

	#[test]
	fn delete_session() {
		let dir = tempfile::tempdir().unwrap();
		let store = SessionStore::new(dir.path().to_path_buf());

		store
			.append("del", &[make_turn(ConversationRole::User, "bye")])
			.unwrap();
		assert!(store.delete("del").unwrap());
		assert!(!store.delete("del").unwrap()); // already gone
		assert!(store.load("del").unwrap().is_empty());
	}

	#[test]
	fn rejects_path_traversal_session_ids() {
		let dir = tempfile::tempdir().unwrap();
		let store = SessionStore::new(dir.path().to_path_buf());

		let bad_ids = [
			"../../etc/passwd",
			"../escape",
			"foo/bar",
			"foo\\bar",
			".hidden",
			"",
		];
		for bad_id in &bad_ids {
			assert!(
				store.load(bad_id).is_err(),
				"should reject session id: {bad_id:?}"
			);
			assert!(
				store
					.append(bad_id, &[make_turn(ConversationRole::User, "x")])
					.is_err(),
				"should reject session id: {bad_id:?}"
			);
			assert!(
				store.delete(bad_id).is_err(),
				"should reject session id: {bad_id:?}"
			);
		}
	}

	#[test]
	fn session_isolation() {
		let dir = tempfile::tempdir().unwrap();
		let store = SessionStore::new(dir.path().to_path_buf());

		store
			.append("s1", &[make_turn(ConversationRole::User, "session one")])
			.unwrap();
		store
			.append("s2", &[make_turn(ConversationRole::User, "session two")])
			.unwrap();

		let s1 = store.load("s1").unwrap();
		let s2 = store.load("s2").unwrap();
		assert_eq!(s1.len(), 1);
		assert_eq!(s2.len(), 1);
		assert_eq!(s1[0].content, "session one");
		assert_eq!(s2[0].content, "session two");
	}
}
