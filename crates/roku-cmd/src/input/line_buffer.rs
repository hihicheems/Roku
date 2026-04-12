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

//! Single-line text buffer with cursor tracking.

/// A mutable text buffer that tracks cursor position as a byte offset.
pub(crate) struct LineBuffer {
	content: String,
	/// Byte offset into `content`. Always sits on a char boundary.
	cursor: usize,
}

impl LineBuffer {
	pub fn new() -> Self {
		Self {
			content: String::new(),
			cursor: 0,
		}
	}

	pub fn content(&self) -> &str {
		&self.content
	}

	/// Number of display columns to the left of the cursor (char count for ASCII).
	pub fn cursor_display_col(&self) -> usize {
		self.content[..self.cursor].chars().count()
	}

	pub fn is_empty(&self) -> bool {
		self.content.is_empty()
	}

	/// Replace the entire buffer and move cursor to end.
	pub fn set(&mut self, text: &str) {
		self.content = text.to_string();
		self.cursor = self.content.len();
	}

	/// Insert a character at the cursor position.
	pub fn insert(&mut self, ch: char) {
		self.content.insert(self.cursor, ch);
		self.cursor += ch.len_utf8();
	}

	/// Delete the character before the cursor (backspace). Returns `true` if
	/// something was deleted.
	pub fn backspace(&mut self) -> bool {
		if self.cursor == 0 {
			return false;
		}
		let prev = self.prev_char_boundary();
		self.content.drain(prev..self.cursor);
		self.cursor = prev;
		true
	}

	/// Delete the character at the cursor (forward delete).
	pub fn delete(&mut self) -> bool {
		if self.cursor >= self.content.len() {
			return false;
		}
		let next = self.next_char_boundary();
		self.content.drain(self.cursor..next);
		true
	}

	pub fn move_left(&mut self) -> bool {
		if self.cursor == 0 {
			return false;
		}
		self.cursor = self.prev_char_boundary();
		true
	}

	pub fn move_right(&mut self) -> bool {
		if self.cursor >= self.content.len() {
			return false;
		}
		self.cursor = self.next_char_boundary();
		true
	}

	pub fn move_home(&mut self) {
		self.cursor = 0;
	}

	pub fn move_end(&mut self) {
		self.cursor = self.content.len();
	}

	/// Ctrl-U: delete from cursor to start of line.
	pub fn delete_to_start(&mut self) {
		self.content.drain(..self.cursor);
		self.cursor = 0;
	}

	/// Ctrl-K: delete from cursor to end of line.
	pub fn delete_to_end(&mut self) {
		self.content.truncate(self.cursor);
	}

	/// Ctrl-W: delete the word before the cursor.
	pub fn delete_word_back(&mut self) {
		if self.cursor == 0 {
			return;
		}
		let before = &self.content[..self.cursor];
		// Skip trailing whitespace, then skip non-whitespace.
		let end = before.len();
		let trimmed = before.trim_end();
		let word_start = trimmed
			.rfind(char::is_whitespace)
			.map(|i| {
				// `i` is byte offset of the whitespace char; step past it.
				i + trimmed[i..].chars().next().unwrap().len_utf8()
			})
			.unwrap_or(0);
		self.content.drain(word_start..end);
		self.cursor = word_start;
	}

	// --- helpers ---

	fn prev_char_boundary(&self) -> usize {
		let mut pos = self.cursor - 1;
		while !self.content.is_char_boundary(pos) {
			pos -= 1;
		}
		pos
	}

	fn next_char_boundary(&self) -> usize {
		let mut pos = self.cursor + 1;
		while pos < self.content.len() && !self.content.is_char_boundary(pos) {
			pos += 1;
		}
		pos
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn insert_and_cursor() {
		let mut buf = LineBuffer::new();
		buf.insert('h');
		buf.insert('i');
		assert_eq!(buf.content(), "hi");
		assert_eq!(buf.cursor_display_col(), 2);
	}

	#[test]
	fn backspace_and_delete() {
		let mut buf = LineBuffer::new();
		buf.set("abc");
		buf.cursor = 1; // after 'a'
		buf.backspace();
		assert_eq!(buf.content(), "bc");
		buf.delete();
		assert_eq!(buf.content(), "c");
	}

	#[test]
	fn move_and_home_end() {
		let mut buf = LineBuffer::new();
		buf.set("hello");
		buf.move_home();
		assert_eq!(buf.cursor_display_col(), 0);
		buf.move_right();
		assert_eq!(buf.cursor_display_col(), 1);
		buf.move_end();
		assert_eq!(buf.cursor_display_col(), 5);
		buf.move_left();
		assert_eq!(buf.cursor_display_col(), 4);
	}

	#[test]
	fn delete_word_back() {
		let mut buf = LineBuffer::new();
		buf.set("hello world");
		buf.delete_word_back();
		assert_eq!(buf.content(), "hello ");
		buf.delete_word_back();
		assert_eq!(buf.content(), "");
	}

	#[test]
	fn delete_to_start_and_end() {
		let mut buf = LineBuffer::new();
		buf.set("abcdef");
		buf.cursor = 3;
		buf.delete_to_end();
		assert_eq!(buf.content(), "abc");
		buf.delete_to_start();
		assert_eq!(buf.content(), "");
	}
}
