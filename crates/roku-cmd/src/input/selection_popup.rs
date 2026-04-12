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

//! Modal interactive selection popup.
//!
//! Unlike [`CommandPopup`](super::command_popup::CommandPopup) which integrates
//! into the main event loop and filters as the user types, this popup is a
//! blocking modal: it takes over terminal input, renders options, and returns
//! when the user confirms or cancels.

use std::os::fd::FromRawFd;

use crossterm::cursor::{MoveLeft, Show};
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::execute;
use crossterm::style::{Color, Print, ResetColor, SetBackgroundColor, SetForegroundColor};
use crossterm::terminal::{self, Clear, ClearType};

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::RawModeGuard;
use super::command_popup::MAX_VISIBLE_ROWS;

/// One item in a selection popup.
pub(crate) struct SelectionItem {
	pub label: String,
	pub description: String,
}

/// Show a blocking modal selection popup and return the chosen index.
///
/// Renders `items` below the current cursor position with arrow-key navigation.
/// Returns `Some(index)` on Enter, `None` on Esc or Ctrl-C.
pub(crate) fn run_selection(items: Vec<SelectionItem>, prompt: &str) -> Option<usize> {
	if items.is_empty() {
		return None;
	}

	let (mut tty, tty_is_fd_alias) = open_tty();

	// Print the prompt before entering raw mode so the newline renders normally.
	let _ = execute!(
		tty,
		SetForegroundColor(Color::DarkCyan),
		Print(prompt),
		ResetColor,
		Print("\r\n"),
	);

	let _ = terminal::enable_raw_mode();
	let _guard = RawModeGuard;

	let mut state = SelectionState::new(items.len());

	// Initial render.
	let rows = render(&mut tty, &items, &state);
	state.rendered_rows = rows;

	let result = event_loop(&mut tty, &items, &mut state);

	drop(_guard);

	// Clean up rendered popup rows.
	cleanup(&mut tty, state.rendered_rows);

	if tty_is_fd_alias {
		std::mem::forget(tty);
	}

	result
}

// ---------------------------------------------------------------------------
// Internal state
// ---------------------------------------------------------------------------

struct SelectionState {
	selected: usize,
	scroll_top: usize,
	total: usize,
	rendered_rows: u16,
}

impl SelectionState {
	fn new(total: usize) -> Self {
		Self {
			selected: 0,
			scroll_top: 0,
			total,
			rendered_rows: 0,
		}
	}

	fn move_up(&mut self) {
		self.selected = if self.selected == 0 {
			self.total - 1
		} else {
			self.selected - 1
		};
		self.ensure_visible();
	}

	fn move_down(&mut self) {
		self.selected = if self.selected + 1 >= self.total {
			0
		} else {
			self.selected + 1
		};
		self.ensure_visible();
	}

	fn ensure_visible(&mut self) {
		let visible = self.total.min(MAX_VISIBLE_ROWS);
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

// ---------------------------------------------------------------------------
// Event loop
// ---------------------------------------------------------------------------

fn event_loop(
	tty: &mut std::fs::File,
	items: &[SelectionItem],
	state: &mut SelectionState,
) -> Option<usize> {
	loop {
		let evt = match event::read() {
			Ok(e) => e,
			Err(_) => return None,
		};

		let Event::Key(key) = evt else {
			continue;
		};
		if key.kind == event::KeyEventKind::Release {
			continue;
		}

		let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
		match key.code {
			KeyCode::Enter => return Some(state.selected),
			KeyCode::Esc => return None,
			KeyCode::Char('c') if ctrl => return None,
			KeyCode::Up => state.move_up(),
			KeyCode::Down => state.move_down(),
			_ => continue,
		}

		// Re-render after navigation.
		let old_rows = state.rendered_rows;
		clear_rows(tty, old_rows);
		let rows = render(tty, items, state);
		state.rendered_rows = rows;
	}
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn render(tty: &mut std::fs::File, items: &[SelectionItem], state: &SelectionState) -> u16 {
	let (term_cols, _) = terminal::size().unwrap_or((80, 24));
	let visible = items
		.len()
		.saturating_sub(state.scroll_top)
		.min(MAX_VISIBLE_ROWS);
	let label_width = items.iter().map(|e| e.label.len()).max().unwrap_or(0) + 3;
	let mut lines_down: u16 = 0;

	for i in 0..visible {
		let idx = state.scroll_top + i;
		let item = &items[idx];
		let is_selected = idx == state.selected;

		let _ = execute!(tty, Clear(ClearType::CurrentLine));
		lines_down += 1;

		let label = format!("  {:<width$}", item.label, width = label_width);
		let full = format!("{label}{}", item.description);
		let display: String = full.chars().take(term_cols as usize).collect();

		if is_selected {
			let _ = execute!(
				tty,
				SetBackgroundColor(Color::DarkGrey),
				SetForegroundColor(Color::White),
			);
		} else {
			let _ = execute!(tty, SetForegroundColor(Color::DarkGrey));
		}

		let _ = execute!(tty, Print(&display), ResetColor);

		if is_selected {
			let remaining = (term_cols as usize).saturating_sub(display.chars().count());
			if remaining > 0 {
				let _ = execute!(
					tty,
					SetBackgroundColor(Color::DarkGrey),
					Print(" ".repeat(remaining)),
					ResetColor,
				);
			}
		}

		// Move to next line (except after the last row).
		if i + 1 < visible {
			let _ = execute!(tty, Print("\r\n"));
		}
	}

	// Move cursor back up to the first rendered row.
	if lines_down > 1 {
		let _ = execute!(tty, crossterm::cursor::MoveUp(lines_down - 1));
	}
	let _ = execute!(tty, Print("\r"));

	lines_down
}

fn clear_rows(tty: &mut std::fs::File, rows: u16) {
	for i in 0..rows {
		let _ = execute!(tty, Clear(ClearType::CurrentLine));
		if i + 1 < rows {
			let _ = execute!(tty, Print("\r\n"));
		}
	}
	if rows > 1 {
		let _ = execute!(tty, crossterm::cursor::MoveUp(rows - 1));
	}
	let _ = execute!(tty, Print("\r"));
}

fn cleanup(tty: &mut std::fs::File, rows: u16) {
	// Clear all rendered rows, then move cursor to the line after the popup.
	for i in 0..rows {
		let _ = execute!(tty, Clear(ClearType::CurrentLine));
		if i + 1 < rows {
			let _ = execute!(tty, Print("\r\n"));
		}
	}
	let _ = execute!(tty, Print("\r\n"), Show);
}

// ---------------------------------------------------------------------------
// Text input (crossterm-based, Esc-cancellable)
// ---------------------------------------------------------------------------

/// Read a single line of text with crossterm (Esc to cancel, Enter to submit).
///
/// Returns `Some(text)` on Enter, `None` on Esc or Ctrl-C.
pub(crate) fn read_text_input(prompt: &str) -> Option<String> {
	let (mut tty, tty_is_fd_alias) = open_tty();

	let _ = execute!(
		tty,
		SetForegroundColor(Color::DarkCyan),
		Print(prompt),
		ResetColor,
	);

	let _ = terminal::enable_raw_mode();
	let _guard = RawModeGuard;

	let mut buf = String::new();
	let result = loop {
		let evt = match event::read() {
			Ok(e) => e,
			Err(_) => break None,
		};
		let Event::Key(key) = evt else {
			continue;
		};
		if key.kind == event::KeyEventKind::Release {
			continue;
		}
		let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
		match key.code {
			KeyCode::Enter => break Some(buf.clone()),
			KeyCode::Esc => break None,
			KeyCode::Char('c') if ctrl => break None,
			KeyCode::Backspace => {
				if let Some(ch) = buf.pop() {
					let w = UnicodeWidthChar::width(ch).unwrap_or(1) as u16;
					let _ = execute!(tty, MoveLeft(w), Clear(ClearType::UntilNewLine));
				}
			}
			KeyCode::Char('u') if ctrl => {
				if !buf.is_empty() {
					let cols = UnicodeWidthStr::width(buf.as_str()) as u16;
					buf.clear();
					let _ = execute!(tty, MoveLeft(cols), Clear(ClearType::UntilNewLine));
				}
			}
			KeyCode::Char(ch) if !ctrl => {
				buf.push(ch);
				let _ = execute!(tty, Print(ch));
			}
			_ => {}
		}
	};

	drop(_guard);
	let _ = execute!(tty, Print("\r\n"), Show);

	if tty_is_fd_alias {
		std::mem::forget(tty);
	}

	result
}

// ---------------------------------------------------------------------------
// TTY helper
// ---------------------------------------------------------------------------

fn open_tty() -> (std::fs::File, bool) {
	match std::fs::OpenOptions::new().write(true).open("/dev/tty") {
		Ok(f) => (f, false),
		Err(_) => (unsafe { std::fs::File::from_raw_fd(2) }, true),
	}
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn navigation_wraps() {
		let mut s = SelectionState::new(3);
		assert_eq!(s.selected, 0);
		s.move_up();
		assert_eq!(s.selected, 2);
		s.move_down();
		assert_eq!(s.selected, 0);
		s.move_down();
		assert_eq!(s.selected, 1);
		s.move_down();
		assert_eq!(s.selected, 2);
		s.move_down();
		assert_eq!(s.selected, 0);
	}

	#[test]
	fn ensure_visible_scrolls() {
		let mut s = SelectionState::new(12);
		// Move to item 10 — should scroll.
		for _ in 0..10 {
			s.move_down();
		}
		assert_eq!(s.selected, 10);
		// scroll_top should ensure item 10 is within the visible window.
		assert!(s.scroll_top + MAX_VISIBLE_ROWS > 10);
		assert!(s.scroll_top <= 10);
	}

	#[test]
	fn single_item_state() {
		let mut s = SelectionState::new(1);
		assert_eq!(s.selected, 0);
		s.move_down();
		assert_eq!(s.selected, 0); // wraps back to 0
		s.move_up();
		assert_eq!(s.selected, 0); // wraps back to 0
	}
}
