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

//! Colored unified diff rendering for the Edit tool.
//!
//! Uses the `similar` crate to produce a unified diff, then colorizes it
//! with crossterm (green additions, red deletions, grey context).

use crossterm::style::{Color, Stylize};
use similar::{ChangeTag, TextDiff};

use super::style::no_color;

/// Render a colored unified diff between `old` and `new` text.
///
/// Returns a formatted string ready for `eprint!`. Includes ±3 lines of context.
/// Returns `None` if the texts are identical.
#[allow(dead_code)] // Scaffolding for Edit tool diff preview (not yet wired).
pub(crate) fn render_unified_diff(
	file_path: &str,
	old: &str,
	new: &str,
	context_lines: usize,
) -> Option<String> {
	if old == new {
		return None;
	}

	let diff = TextDiff::from_lines(old, new);
	let mut output = String::new();

	if no_color() {
		// Plain-text unified diff.
		for hunk in diff
			.unified_diff()
			.context_radius(context_lines)
			.iter_hunks()
		{
			output.push_str(&format!("--- {file_path}\n+++ {file_path}\n"));
			output.push_str(&format!("{hunk}"));
		}
		return if output.is_empty() {
			None
		} else {
			Some(output)
		};
	}

	// Colored diff.
	let header = format!("  {} {file_path}", "diff".with(Color::DarkGrey));
	output.push_str(&header);
	output.push('\n');

	for hunk in diff
		.unified_diff()
		.context_radius(context_lines)
		.iter_hunks()
	{
		// Hunk header (@@...@@).
		let hunk_str = format!("{hunk}");
		for line in hunk_str.lines() {
			if line.starts_with("@@") {
				output.push_str(&format!("  {}\n", line.with(Color::DarkCyan)));
			} else if line.starts_with('+') {
				output.push_str(&format!("  {}\n", line.with(Color::Green)));
			} else if line.starts_with('-') {
				output.push_str(&format!("  {}\n", line.with(Color::Red)));
			} else {
				output.push_str(&format!("  {}\n", line.with(Color::DarkGrey)));
			}
		}
	}

	if output.is_empty() || output.lines().count() <= 1 {
		return None;
	}

	Some(output)
}

/// Render a compact diff summary for streaming display.
/// Returns `(additions, deletions)` line counts.
#[allow(dead_code)] // Scaffolding for Edit tool diff preview (not yet wired).
pub(crate) fn diff_stats(old: &str, new: &str) -> (usize, usize) {
	let diff = TextDiff::from_lines(old, new);
	let mut adds = 0usize;
	let mut dels = 0usize;
	for change in diff.iter_all_changes() {
		match change.tag() {
			ChangeTag::Insert => adds += 1,
			ChangeTag::Delete => dels += 1,
			ChangeTag::Equal => {}
		}
	}
	(adds, dels)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::test_support::with_env_var;

	#[test]
	fn identical_returns_none() {
		assert!(render_unified_diff("f.rs", "hello\n", "hello\n", 3).is_none());
	}

	#[test]
	fn diff_stats_counts() {
		let (adds, dels) = diff_stats("a\nb\nc\n", "a\nx\nc\n");
		assert_eq!(adds, 1);
		assert_eq!(dels, 1);
	}

	#[test]
	fn plain_diff_no_color() {
		with_env_var("NO_COLOR", "1", || {
			let result = render_unified_diff("f.rs", "old\n", "new\n", 3);
			assert!(result.is_some());
			let text = result.unwrap();
			assert!(text.contains("-old"));
			assert!(text.contains("+new"));
		});
	}
}
