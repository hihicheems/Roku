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

//! Custom crossterm-based input with interactive command popup.
//!
//! Replaces rustyline to provide a visual slash-command completion popup
//! with arrow-key navigation, filtering, and inline selection.

mod command_popup;
mod history;
mod line_buffer;
mod selection_popup;

pub(crate) use command_popup::{CommandEntry, SubCommandEntry};
pub(crate) use selection_popup::{SelectionItem, read_text_input, run_selection};

use std::os::fd::FromRawFd;
use std::path::PathBuf;

use crossterm::cursor::Show;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::style::{Color, Print, ResetColor, SetForegroundColor};
use crossterm::terminal::{self, Clear, ClearType};

use command_popup::CommandPopup;
use history::History;
use line_buffer::LineBuffer;

/// Result of a single `readline()` call.
pub(crate) enum ReadlineResult {
	/// User submitted a line (may be empty).
	Line(String),
	/// Ctrl-C was pressed.
	Interrupted,
	/// Ctrl-D on an empty line.
	Eof,
}

/// Interactive line reader with command popup support.
pub(crate) struct InputReader {
	history: History,
	popup: CommandPopup,
}

impl InputReader {
	pub fn new(commands: Vec<CommandEntry>, history_path: PathBuf) -> Self {
		let history = History::load(history_path);
		let popup = CommandPopup::new(commands);
		Self { history, popup }
	}

	pub fn add_history_entry(&mut self, line: &str) {
		self.history.add(line);
	}

	pub fn save_history(&self) {
		let _ = self.history.save();
	}

	/// Read one line from the terminal, showing the command popup when the input
	/// starts with `/`.
	pub fn readline(&mut self, prompt: &str) -> ReadlineResult {
		// Use /dev/tty for terminal output so it works regardless of redirections.
		// Track whether we used the fd-2 fallback so we can avoid closing stderr.
		let (mut tty, tty_is_fd_alias) =
			match std::fs::OpenOptions::new().write(true).open("/dev/tty") {
				Ok(f) => (f, false),
				Err(_) => (unsafe { std::fs::File::from_raw_fd(2) }, true),
			};
		let mut buf = LineBuffer::new();
		let mut popup_active = false;

		// Print the prompt.
		let _ = execute!(
			tty,
			SetForegroundColor(Color::DarkCyan),
			Print(prompt),
			ResetColor,
		);
		let prompt_len = prompt.len() as u16;

		// Enter raw mode with an RAII guard so it is always restored, even on panic.
		let _ = terminal::enable_raw_mode();
		let _raw_guard = RawModeGuard;

		let result = self.event_loop(&mut tty, &mut buf, &mut popup_active, prompt_len);

		// Explicitly disable (guard will also disable on drop/panic).
		drop(_raw_guard);

		// Clear popup remnants: go down, clear each line, then come back up.
		let rows = self.popup.last_rendered_rows();
		if rows > 0 {
			for _ in 0..rows {
				let _ = execute!(tty, Print("\r\n"), Clear(ClearType::CurrentLine),);
			}
			let _ = execute!(tty, crossterm::cursor::MoveUp(rows));
		}
		let _ = execute!(tty, Print("\r\n"), Show);

		// Only forget the handle when it aliases fd 2 (stderr fallback);
		// normal /dev/tty handles must be closed to avoid leaking fds.
		if tty_is_fd_alias {
			std::mem::forget(tty);
		}

		result
	}

	fn event_loop(
		&mut self,
		tty: &mut std::fs::File,
		buf: &mut LineBuffer,
		popup_active: &mut bool,
		prompt_len: u16,
	) -> ReadlineResult {
		// When the user presses Esc, we suppress re-opening the popup until
		// the input changes enough to no longer match the trigger condition.
		let mut popup_dismissed = false;

		loop {
			let evt = match event::read() {
				Ok(e) => e,
				Err(_) => return ReadlineResult::Interrupted,
			};

			let Event::Key(key) = evt else {
				continue;
			};

			if key.kind == event::KeyEventKind::Release {
				continue;
			}

			match self.handle_key(key, buf, popup_active, &mut popup_dismissed) {
				KeyAction::Continue => {}
				KeyAction::Submit => {
					// Redraw before returning so the final command text is visible
					// on the prompt line (important when popup filled the buffer).
					self.redraw(tty, buf, false, prompt_len);
					let line = buf.content().to_string();
					return ReadlineResult::Line(line);
				}
				KeyAction::SubmitWithSubCommands(items) => {
					// Show the parent command, then drop raw mode for the modal popup.
					*popup_active = false;
					self.redraw(tty, buf, false, prompt_len);
					let parent = buf.content().to_string();
					// run_selection manages its own raw mode, but we must exit
					// ours first to avoid nesting.
					let _ = terminal::disable_raw_mode();
					let sub = run_selection(items, "");
					let _ = terminal::enable_raw_mode();
					if let Some(idx) = sub {
						// We need the sub-command name. Re-derive from parent's
						// sub_commands. The items vec was consumed, but the popup
						// still has the entry. Reconstruct from index.
						if let Some(entry) = self.popup.selected_entry()
							&& let Some(subs) = &entry.sub_commands
							&& let Some(sub_entry) = subs.get(idx)
						{
							buf.set(&format!("{parent} {}", sub_entry.name));
						}
					}
					self.redraw(tty, buf, false, prompt_len);
					let line = buf.content().to_string();
					return ReadlineResult::Line(line);
				}
				KeyAction::Interrupt => return ReadlineResult::Interrupted,
				KeyAction::Eof => return ReadlineResult::Eof,
			}

			// Sync popup state.
			let content = buf.content();
			let should_popup = content.starts_with('/') && !content.contains(' ');
			if should_popup && !popup_dismissed {
				self.popup.update_filter(content);
				*popup_active = true;
			} else {
				if !should_popup {
					// Input no longer matches trigger — reset the dismissed flag.
					popup_dismissed = false;
				}
				*popup_active = false;
			}

			self.redraw(tty, buf, *popup_active, prompt_len);
		}
	}

	fn handle_key(
		&mut self,
		key: KeyEvent,
		buf: &mut LineBuffer,
		popup_active: &mut bool,
		popup_dismissed: &mut bool,
	) -> KeyAction {
		let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

		match key.code {
			// --- submit ---
			KeyCode::Enter => {
				if *popup_active && let Some(selected) = self.popup.selected_entry() {
					buf.set(&format!("/{}", selected.name));
					if let Some(subs) = &selected.sub_commands {
						let items: Vec<_> = subs
							.iter()
							.map(|s| SelectionItem {
								label: s.name.to_string(),
								description: s.description.to_string(),
							})
							.collect();
						return KeyAction::SubmitWithSubCommands(items);
					}
				}
				return KeyAction::Submit;
			}

			// --- signals ---
			KeyCode::Char('c') if ctrl => return KeyAction::Interrupt,
			KeyCode::Char('d') if ctrl => {
				if buf.is_empty() {
					return KeyAction::Eof;
				}
				buf.delete();
			}

			// --- popup navigation ---
			KeyCode::Up if *popup_active => self.popup.move_up(),
			KeyCode::Down if *popup_active => self.popup.move_down(),
			KeyCode::Tab if *popup_active => {
				if let Some(cmd) = self.popup.selected_command() {
					buf.set(&format!("/{cmd} "));
					*popup_active = false;
				}
			}
			KeyCode::Esc if *popup_active => {
				*popup_active = false;
				*popup_dismissed = true;
			}
			KeyCode::Esc => {
				// Universal cancel: clear input and return to a fresh prompt.
				buf.set("");
				self.history.reset_position();
				return KeyAction::Submit;
			}

			// --- history navigation (when popup is NOT active) ---
			KeyCode::Up => {
				if let Some(line) = self.history.navigate_up(buf.content()) {
					buf.set(line);
				}
			}
			KeyCode::Down => {
				if let Some(line) = self.history.navigate_down() {
					buf.set(line);
				}
			}

			// --- editing ---
			KeyCode::Backspace => {
				buf.backspace();
			}
			KeyCode::Delete => {
				buf.delete();
			}
			KeyCode::Left => {
				buf.move_left();
			}
			KeyCode::Right => {
				buf.move_right();
			}
			KeyCode::Home => buf.move_home(),
			KeyCode::End => buf.move_end(),
			KeyCode::Char('a') if ctrl => buf.move_home(),
			KeyCode::Char('e') if ctrl => buf.move_end(),
			KeyCode::Char('u') if ctrl => buf.delete_to_start(),
			KeyCode::Char('k') if ctrl => buf.delete_to_end(),
			KeyCode::Char('w') if ctrl => buf.delete_word_back(),

			// --- regular character input (ignore unhandled Ctrl combos) ---
			KeyCode::Char(ch) if !ctrl => {
				buf.insert(ch);
				self.history.reset_position();
			}

			_ => {}
		}

		KeyAction::Continue
	}

	fn redraw(
		&mut self,
		tty: &mut std::fs::File,
		buf: &LineBuffer,
		popup_active: bool,
		prompt_len: u16,
	) {
		let (term_cols, _) = terminal::size().unwrap_or((80, 24));

		// Redraw the input text on the prompt line.
		let _ = execute!(
			tty,
			crossterm::cursor::MoveToColumn(prompt_len),
			Clear(ClearType::UntilNewLine),
			Print(buf.content()),
		);

		// Render popup below using relative movement.
		let rows_drawn = if popup_active {
			self.popup.render_relative(tty, term_cols)
		} else {
			self.popup.clear_relative(tty)
		};

		// Move cursor back up to the prompt line.
		if rows_drawn > 0 {
			let _ = execute!(tty, crossterm::cursor::MoveUp(rows_drawn));
		}

		// Position cursor within the input text.
		let cursor_col = prompt_len + buf.cursor_display_col() as u16;
		let _ = execute!(tty, crossterm::cursor::MoveToColumn(cursor_col));
	}
}

enum KeyAction {
	Continue,
	Submit,
	/// Submit a parent command, then show a sub-command selection popup.
	SubmitWithSubCommands(Vec<selection_popup::SelectionItem>),
	Interrupt,
	Eof,
}

impl Drop for InputReader {
	fn drop(&mut self) {
		self.save_history();
	}
}

/// RAII guard that disables raw mode on drop, ensuring terminal state is
/// restored even if the event loop panics.
pub(super) struct RawModeGuard;

impl Drop for RawModeGuard {
	fn drop(&mut self) {
		let _ = terminal::disable_raw_mode();
	}
}
