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

//! Interactive slash-command popup rendered below the input line.
//!
//! Adapted from the Codex CLI's `CommandPopup` / `ScrollState` pattern.

use std::io::Write;

use crossterm::execute;
use crossterm::style::{Color, ResetColor, SetBackgroundColor, SetForegroundColor};
use crossterm::terminal::{Clear, ClearType};

/// Maximum number of visible rows in the popup.
pub(super) const MAX_VISIBLE_ROWS: usize = 8;

/// A command that can appear in the popup.
#[derive(Clone)]
pub(crate) struct CommandEntry {
	pub name: &'static str,
	pub description: &'static str,
}

/// State for the interactive command popup.
pub(crate) struct CommandPopup {
	entries: Vec<CommandEntry>,
	filter: String,
	selected: usize,
	scroll_top: usize,
	/// Number of terminal rows the popup currently occupies (for clearing).
	rendered_rows: u16,
}

impl CommandPopup {
	pub fn new(entries: Vec<CommandEntry>) -> Self {
		Self {
			entries,
			filter: String::new(),
			selected: 0,
			scroll_top: 0,
			rendered_rows: 0,
		}
	}

	/// Update the filter string from the current input buffer. Expects the full
	/// input line (e.g. "/he"). Extracts the token after `/`.
	pub fn update_filter(&mut self, input: &str) {
		self.filter = input
			.strip_prefix('/')
			.unwrap_or("")
			.split_whitespace()
			.next()
			.unwrap_or("")
			.to_string();
		let len = self.filtered().len();
		if len == 0 {
			self.selected = 0;
		} else if self.selected >= len {
			self.selected = len - 1;
		}
		self.ensure_visible(len);
	}

	/// Return the filtered list of entries matching the current filter.
	pub fn filtered(&self) -> Vec<&CommandEntry> {
		let filter = self.filter.to_lowercase();
		if filter.is_empty() {
			return self.entries.iter().collect();
		}
		// Exact matches first, then prefix matches.
		let mut exact = Vec::new();
		let mut prefix = Vec::new();
		for entry in &self.entries {
			let name = entry.name.to_lowercase();
			if name == filter {
				exact.push(entry);
			} else if name.starts_with(&filter) {
				prefix.push(entry);
			}
		}
		exact.extend(prefix);
		exact
	}

	pub fn move_up(&mut self) {
		let len = self.filtered().len();
		if len == 0 {
			return;
		}
		self.selected = if self.selected == 0 {
			len - 1
		} else {
			self.selected - 1
		};
		self.ensure_visible(len);
	}

	pub fn move_down(&mut self) {
		let len = self.filtered().len();
		if len == 0 {
			return;
		}
		self.selected = if self.selected + 1 >= len {
			0
		} else {
			self.selected + 1
		};
		self.ensure_visible(len);
	}

	/// Return the currently selected command name (without `/` prefix), if any.
	pub fn selected_command(&self) -> Option<String> {
		let items = self.filtered();
		items.get(self.selected).map(|e| e.name.to_string())
	}

	/// Render the popup using relative cursor movement (no absolute positioning).
	///
	/// The cursor is expected to be on the input line. This method prints popup
	/// rows below using `\r\n`, then the caller uses `MoveUp` to return.
	/// Returns the total number of rows moved down (for the caller's `MoveUp`).
	pub fn render_relative(&mut self, w: &mut impl Write, term_cols: u16) -> u16 {
		let items = self.filtered();
		let total = items.len();
		if total == 0 {
			return self.clear_relative(w);
		}

		// Clamp visible to items actually reachable from scroll_top.
		let visible = total.saturating_sub(self.scroll_top).min(MAX_VISIBLE_ROWS);
		let name_col_width = items.iter().map(|e| e.name.len()).max().unwrap_or(0) + 3;
		let mut lines_down: u16 = 0;

		for i in 0..visible {
			let idx = self.scroll_top + i;
			let entry = &items[idx];
			let is_selected = idx == self.selected;

			let _ = execute!(
				w,
				crossterm::style::Print("\r\n"),
				Clear(ClearType::CurrentLine),
			);
			lines_down += 1;

			let name = format!("  /{:<width$}", entry.name, width = name_col_width);
			let desc = entry.description;
			let full = format!("{name}{desc}");
			let display: String = full.chars().take(term_cols as usize).collect();

			if is_selected {
				let _ = execute!(
					w,
					SetBackgroundColor(Color::DarkGrey),
					SetForegroundColor(Color::White),
				);
			} else {
				let _ = execute!(w, SetForegroundColor(Color::DarkGrey));
			}

			let _ = execute!(w, crossterm::style::Print(&display), ResetColor);

			if is_selected {
				let remaining = (term_cols as usize).saturating_sub(display.chars().count());
				if remaining > 0 {
					let _ = execute!(
						w,
						SetBackgroundColor(Color::DarkGrey),
						crossterm::style::Print(" ".repeat(remaining)),
						ResetColor,
					);
				}
			}
		}

		// Clear leftover rows from a previous taller popup.
		for _ in visible as u16..self.rendered_rows {
			let _ = execute!(
				w,
				crossterm::style::Print("\r\n"),
				Clear(ClearType::CurrentLine),
			);
			lines_down += 1;
		}

		self.rendered_rows = visible as u16;
		lines_down
	}

	/// Clear any previously rendered popup rows (relative positioning).
	/// Returns the number of lines moved down.
	pub fn clear_relative(&mut self, w: &mut impl Write) -> u16 {
		let rows = self.rendered_rows;
		for _ in 0..rows {
			let _ = execute!(
				w,
				crossterm::style::Print("\r\n"),
				Clear(ClearType::CurrentLine),
			);
		}
		self.rendered_rows = 0;
		rows
	}

	/// Number of rows last rendered (for cleanup).
	pub fn last_rendered_rows(&self) -> u16 {
		self.rendered_rows
	}

	fn ensure_visible(&mut self, len: usize) {
		let visible = len.min(MAX_VISIBLE_ROWS);
		if visible == 0 {
			self.scroll_top = 0;
			return;
		}
		if self.selected < self.scroll_top {
			self.scroll_top = self.selected;
		} else if self.selected >= self.scroll_top + visible {
			self.scroll_top = self.selected + 1 - visible;
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn test_entries() -> Vec<CommandEntry> {
		vec![
			CommandEntry {
				name: "clear",
				description: "Clear conversation history",
			},
			CommandEntry {
				name: "compact",
				description: "Compact conversation history",
			},
			CommandEntry {
				name: "exit",
				description: "Exit the REPL",
			},
			CommandEntry {
				name: "help",
				description: "Show available commands",
			},
			CommandEntry {
				name: "login",
				description: "Sign in to a provider",
			},
			CommandEntry {
				name: "session",
				description: "Manage chat sessions",
			},
		]
	}

	#[test]
	fn filter_by_prefix() {
		let mut popup = CommandPopup::new(test_entries());
		popup.update_filter("/cl");
		let items = popup.filtered();
		assert_eq!(items.len(), 1);
		assert_eq!(items[0].name, "clear");
	}

	#[test]
	fn empty_filter_shows_all() {
		let mut popup = CommandPopup::new(test_entries());
		popup.update_filter("/");
		assert_eq!(popup.filtered().len(), 6);
	}

	#[test]
	fn navigation_wraps() {
		let mut popup = CommandPopup::new(test_entries());
		popup.update_filter("/");
		assert_eq!(popup.selected, 0);
		popup.move_up();
		assert_eq!(popup.selected, 5); // wrapped to bottom
		popup.move_down();
		assert_eq!(popup.selected, 0); // wrapped to top
	}

	#[test]
	fn selected_command_returns_name() {
		let mut popup = CommandPopup::new(test_entries());
		popup.update_filter("/he");
		assert_eq!(popup.selected_command(), Some("help".to_string()));
	}

	#[test]
	fn no_match_returns_none() {
		let mut popup = CommandPopup::new(test_entries());
		popup.update_filter("/zzz");
		assert_eq!(popup.selected_command(), None);
	}
}
