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

//! File-backed command history with up/down navigation.

use std::fs;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;

pub(crate) struct History {
	entries: Vec<String>,
	/// Browsing position. When equal to `entries.len()`, the user is on the
	/// current (unsaved) line.
	position: usize,
	/// Snapshot of the line the user was typing before they started browsing.
	saved_line: Option<String>,
	path: PathBuf,
}

impl History {
	pub fn load(path: PathBuf) -> Self {
		let entries = if path.exists() {
			fs::File::open(&path)
				.map(io::BufReader::new)
				.ok()
				.map(|r| r.lines().map_while(Result::ok).collect())
				.unwrap_or_default()
		} else {
			Vec::new()
		};
		let position = entries.len();
		Self {
			entries,
			position,
			saved_line: None,
			path,
		}
	}

	pub fn save(&self) -> io::Result<()> {
		if let Some(parent) = self.path.parent() {
			fs::create_dir_all(parent)?;
		}
		let mut f = fs::File::create(&self.path)?;
		for entry in &self.entries {
			writeln!(f, "{entry}")?;
		}
		Ok(())
	}

	/// Add an entry. Deduplicates against the last entry.
	pub fn add(&mut self, line: &str) {
		let line = line.to_string();
		if self.entries.last() == Some(&line) {
			self.reset_position();
			return;
		}
		self.entries.push(line);
		self.reset_position();
	}

	/// Move one step back in history. `current_line` is saved on the first
	/// call so the user can return to it.
	pub fn navigate_up(&mut self, current_line: &str) -> Option<&str> {
		if self.entries.is_empty() || self.position == 0 {
			return None;
		}
		if self.position == self.entries.len() {
			self.saved_line = Some(current_line.to_string());
		}
		self.position -= 1;
		Some(&self.entries[self.position])
	}

	/// Move one step forward in history, or restore the saved line.
	pub fn navigate_down(&mut self) -> Option<&str> {
		if self.position >= self.entries.len() {
			return None;
		}
		self.position += 1;
		if self.position == self.entries.len() {
			self.saved_line.as_deref()
		} else {
			Some(&self.entries[self.position])
		}
	}

	pub fn reset_position(&mut self) {
		self.position = self.entries.len();
		self.saved_line = None;
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn navigate_up_and_down() {
		let mut h = History {
			entries: vec!["first".into(), "second".into()],
			position: 2,
			saved_line: None,
			path: PathBuf::from("/dev/null"),
		};
		assert_eq!(h.navigate_up("current"), Some("second"));
		assert_eq!(h.navigate_up("current"), Some("first"));
		assert_eq!(h.navigate_up("current"), None);
		assert_eq!(h.navigate_down(), Some("second"));
		assert_eq!(h.navigate_down(), Some("current"));
		assert_eq!(h.navigate_down(), None);
	}

	#[test]
	fn add_duplicate_resets_position() {
		let mut h = History {
			entries: vec!["hello".into(), "world".into()],
			position: 2,
			saved_line: None,
			path: PathBuf::from("/dev/null"),
		};
		// Browse up to "world", then add a duplicate.
		assert_eq!(h.navigate_up("typing"), Some("world"));
		assert_eq!(h.position, 1);
		h.add("world"); // duplicate — should still reset position
		assert_eq!(h.position, 2); // back to end
		// Navigate up should start from the end, showing "world" first.
		assert_eq!(h.navigate_up(""), Some("world"));
		assert_eq!(h.navigate_up(""), Some("hello"));
	}

	#[test]
	fn add_deduplicates() {
		let mut h = History {
			entries: vec!["hello".into()],
			position: 1,
			saved_line: None,
			path: PathBuf::from("/dev/null"),
		};
		h.add("hello");
		assert_eq!(h.entries.len(), 1);
		h.add("world");
		assert_eq!(h.entries.len(), 2);
	}
}
