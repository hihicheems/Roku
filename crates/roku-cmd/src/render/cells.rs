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

//! HistoryCell trait and concrete cell types for typed message rendering.
//!
//! Cell types are pre-built infrastructure for the transcript system (Task 04+).
//! They are not yet wired into the render flow and will trigger dead_code lint
//! until the full transcript system is built.
#![allow(dead_code)]

use super::style::no_color;
use crossterm::style::{Color, Stylize};

/// The type of content a history cell represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CellType {
	UserMessage,
	AssistantText,
	ToolStart,
	ToolEnd,
	SystemInfo,
	CompactNotice,
}

/// A discrete unit of conversation history that can render itself.
pub(crate) trait HistoryCell: std::fmt::Debug + Send + Sync {
	fn render_lines(&self, width: u16) -> Vec<String>;
	fn cell_type(&self) -> CellType;
}

// --- Concrete cell types ---

/// User's input message.
#[derive(Debug)]
pub(crate) struct UserMessageCell {
	pub(crate) text: String,
}

impl HistoryCell for UserMessageCell {
	fn render_lines(&self, _width: u16) -> Vec<String> {
		if no_color() {
			return vec![format!(">>> {}", self.text)];
		}
		vec![format!("{} {}", ">>>".with(Color::DarkCyan), self.text)]
	}
	fn cell_type(&self) -> CellType {
		CellType::UserMessage
	}
}

/// AI assistant's text response (a block of committed lines).
#[derive(Debug)]
pub(crate) struct AssistantTextCell {
	pub(crate) lines: Vec<String>,
}

impl HistoryCell for AssistantTextCell {
	fn render_lines(&self, _width: u16) -> Vec<String> {
		if no_color() {
			return self.lines.clone();
		}
		// AI text uses white (default terminal foreground) — distinct from tool/system
		// by NOT having a colored prefix. The surrounding tool/system cells provide contrast.
		self.lines.clone()
	}
	fn cell_type(&self) -> CellType {
		CellType::AssistantText
	}
}

/// Tool invocation start.
#[derive(Debug)]
pub(crate) struct ToolStartCell {
	pub(crate) tool_name: String,
	pub(crate) args_summary: Option<String>,
}

impl HistoryCell for ToolStartCell {
	fn render_lines(&self, _width: u16) -> Vec<String> {
		vec![super::styled_tool_start(
			&self.tool_name,
			self.args_summary.as_deref(),
		)]
	}
	fn cell_type(&self) -> CellType {
		CellType::ToolStart
	}
}

/// Tool invocation end.
#[derive(Debug)]
pub(crate) struct ToolEndCell {
	pub(crate) tool_name: String,
	pub(crate) elapsed_ms: Option<u64>,
	pub(crate) result_summary: Option<String>,
}

impl HistoryCell for ToolEndCell {
	fn render_lines(&self, _width: u16) -> Vec<String> {
		vec![super::styled_tool_end(
			&self.tool_name,
			self.elapsed_ms,
			self.result_summary.as_deref(),
		)]
	}
	fn cell_type(&self) -> CellType {
		CellType::ToolEnd
	}
}

/// System info/status message with severity-based coloring.
#[derive(Debug)]
pub(crate) struct SystemInfoCell {
	pub(crate) message: String,
	pub(crate) severity: SystemSeverity,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum SystemSeverity {
	Info,
	Warning,
	Error,
	Dim,
}

impl HistoryCell for SystemInfoCell {
	fn render_lines(&self, _width: u16) -> Vec<String> {
		if no_color() {
			return vec![self.message.clone()];
		}
		let styled = match self.severity {
			SystemSeverity::Info => self.message.clone(),
			SystemSeverity::Warning => self.message.as_str().with(Color::DarkYellow).to_string(),
			SystemSeverity::Error => self.message.as_str().with(Color::Red).to_string(),
			SystemSeverity::Dim => self.message.as_str().with(Color::DarkGrey).to_string(),
		};
		vec![styled]
	}
	fn cell_type(&self) -> CellType {
		CellType::SystemInfo
	}
}

/// Compact/summary notice.
#[derive(Debug)]
pub(crate) struct CompactNoticeCell {
	pub(crate) message: String,
}

impl HistoryCell for CompactNoticeCell {
	fn render_lines(&self, _width: u16) -> Vec<String> {
		if no_color() {
			return vec![self.message.clone()];
		}
		vec![self.message.as_str().with(Color::DarkGrey).to_string()]
	}
	fn cell_type(&self) -> CellType {
		CellType::CompactNotice
	}
}
