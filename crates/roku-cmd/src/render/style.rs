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

//! Shared style utilities for the interactive REPL output.
//!
//! Respects `NO_COLOR` and `TERM=dumb` for environments that don't support
//! ANSI escape codes.

use crossterm::style::{Color, Stylize};

/// Returns true when the environment indicates colors should be suppressed.
pub(crate) fn no_color() -> bool {
	std::env::var("NO_COLOR").is_ok() || std::env::var("TERM").ok().is_some_and(|t| t == "dumb")
}

/// Styled banner header line.
pub(crate) fn styled_banner(text: &str) -> String {
	if no_color() {
		return text.to_string();
	}
	text.bold().to_string()
}

/// Styled tool-start line: `[tool] ▶ ToolName: summary`
pub(crate) fn styled_tool_start(tool_name: &str, args_summary: Option<&str>) -> String {
	if no_color() {
		return match args_summary {
			Some(s) => format!("[tool] {tool_name}: {s}"),
			None => format!("[tool] {tool_name}"),
		};
	}
	let prefix = "[tool]".with(Color::DarkCyan).to_string();
	let arrow = "▶".with(Color::DarkCyan).to_string();
	let name = tool_name.bold().to_string();
	match args_summary {
		Some(s) => format!("{prefix} {arrow} {name}: {}", s.with(Color::Grey)),
		None => format!("{prefix} {arrow} {name}"),
	}
}

/// Styled tool-end line: `[tool] ToolName: result_summary ✓ 12ms`
pub(crate) fn styled_tool_end(
	tool_name: &str,
	elapsed_ms: Option<u64>,
	result_summary: Option<&str>,
) -> String {
	if no_color() {
		let elapsed = elapsed_ms
			.map(|ms| format!(" ({ms}ms)"))
			.unwrap_or_default();
		return match result_summary {
			Some(s) => format!("[tool] {tool_name}: {s}{elapsed}"),
			None => format!("[tool] {tool_name} done{elapsed}"),
		};
	}
	let prefix = "[tool]".with(Color::DarkCyan).to_string();
	let name = tool_name.bold().to_string();
	let check = "✓".with(Color::Green).to_string();
	let elapsed = elapsed_ms
		.map(|ms| format!(" {}", format!("{ms}ms").with(Color::DarkGrey)))
		.unwrap_or_default();
	match result_summary {
		Some(s) => format!("{prefix} {name}: {} {check}{elapsed}", s.with(Color::Grey)),
		None => format!("{prefix} {name} {check}{elapsed}"),
	}
}

/// Styled token/cost info line.
pub(crate) fn styled_token_info(
	prompt: u64,
	output: u64,
	cost: f64,
	session_total: u64,
	model: Option<&str>,
) -> String {
	if no_color() {
		let model_tag = model.map(|m| format!(" model: {m}")).unwrap_or_default();
		return format!(
			"[tokens: {prompt}/{output}, cost: ~${cost:.4}] [session: {session_total}]{model_tag}"
		);
	}
	let tokens_label = "[tokens:".with(Color::DarkGrey).to_string();
	let cost_str = format!("~${cost:.4}").with(Color::DarkYellow).to_string();
	let session_str = format!("[session: {session_total}]")
		.with(Color::DarkGrey)
		.to_string();
	let model_tag = model
		.map(|m| format!(" {}", format!("model: {m}").with(Color::DarkGrey)))
		.unwrap_or_default();
	format!("{tokens_label} {prompt}/{output}, cost: {cost_str}] {session_str}{model_tag}")
}
