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

//! Markdown to Telegram HTML converter with message chunking.
//!
//! Telegram's Bot API supports a [limited HTML subset][tg-html]: `<b>`, `<i>`,
//! `<code>`, `<pre>`, and `<a>`. This module converts standard Markdown
//! formatting to that subset.
//!
//! # Supported Conversions
//!
//! | Markdown              | HTML Output                         |
//! |-----------------------|-------------------------------------|
//! | `**bold**`            | `<b>bold</b>`                       |
//! | `__bold__`            | `<b>bold</b>`                       |
//! | `*italic*`            | `<i>italic</i>`                     |
//! | `_italic_`            | `<i>italic</i>`                     |
//! | `` `code` ``          | `<code>code</code>`                 |
//! | ` ```lang\ncode``` `  | `<pre>code</pre>`                   |
//! | `[text](url)`         | `<a href="url">text</a>`            |
//! | `# Heading`           | `<b>Heading</b>`                    |
//!
//! HTML special characters (`&`, `<`, `>`) are escaped before any Markdown
//! processing to prevent injection.
//!
//! # Message Chunking
//!
//! [`chunk_message`] splits long HTML strings into pieces that fit within
//! Telegram's 4096-character message limit. It prefers breaking at newlines,
//! then spaces, and falls back to hard breaks as a last resort.
//!
//! [tg-html]: https://core.telegram.org/bots/api#html-style

use std::sync::LazyLock;

use regex::Regex;

/// Telegram maximum message length in characters.
pub const TELEGRAM_MAX_MESSAGE_LEN: usize = 4096;

// ---------------------------------------------------------------------------
// Markdown → Telegram HTML
// ---------------------------------------------------------------------------

/// Convert Markdown text to Telegram-supported HTML subset.
///
/// HTML special characters (`&`, `<`, `>`) are escaped first, then inline and
/// block-level Markdown syntax is converted to the corresponding HTML tags.
pub fn markdown_to_telegram_html(md: &str) -> String {
	let preprocessed = preprocess_blocks(md);
	let escaped = html_escape(&preprocessed);

	let mut result = String::with_capacity(escaped.len());
	let chars: Vec<char> = escaped.chars().collect();
	let len = chars.len();
	let mut i = 0;

	while i < len {
		// Fenced code blocks: ```...```
		if i + 2 < len && chars[i] == '`' && chars[i + 1] == '`' && chars[i + 2] == '`' {
			i += 3;
			// skip optional language tag (until newline)
			while i < len && chars[i] != '\n' {
				i += 1;
			}
			if i < len {
				i += 1; // skip newline
			}
			let start = i;
			while i + 2 < len {
				if chars[i] == '`' && chars[i + 1] == '`' && chars[i + 2] == '`' {
					break;
				}
				i += 1;
			}
			let code: String = chars[start..i].iter().collect();
			let code = code.trim_end_matches('\n');
			result.push_str("<pre>");
			result.push_str(code);
			result.push_str("</pre>");
			if i + 2 < len {
				i += 3;
			}
			continue;
		}

		// Inline code: `...`
		if chars[i] == '`' {
			i += 1;
			let start = i;
			while i < len && chars[i] != '`' {
				i += 1;
			}
			let code: String = chars[start..i].iter().collect();
			result.push_str("<code>");
			result.push_str(&code);
			result.push_str("</code>");
			if i < len {
				i += 1;
			}
			continue;
		}

		// Links: [text](url)
		if chars[i] == '['
			&& let Some((link_text, url, end_pos)) = try_parse_link(&chars, i)
		{
			use std::fmt::Write;
			let _ = write!(result, "<a href=\"{url}\">{link_text}</a>");
			i = end_pos;
			continue;
		}

		// Bold: **text** or __text__
		if i + 1 < len
			&& chars[i] == '*'
			&& chars[i + 1] == '*'
			&& let Some((content, end_pos)) = try_parse_delimited(&chars, i, "**")
		{
			result.push_str("<b>");
			result.push_str(&content);
			result.push_str("</b>");
			i = end_pos;
			continue;
		}
		if i + 1 < len
			&& chars[i] == '_'
			&& chars[i + 1] == '_'
			&& let Some((content, end_pos)) = try_parse_delimited(&chars, i, "__")
		{
			result.push_str("<b>");
			result.push_str(&content);
			result.push_str("</b>");
			i = end_pos;
			continue;
		}

		// Italic: *text* or _text_
		if chars[i] == '*'
			&& let Some((content, end_pos)) = try_parse_delimited(&chars, i, "*")
		{
			result.push_str("<i>");
			result.push_str(&content);
			result.push_str("</i>");
			i = end_pos;
			continue;
		}
		if chars[i] == '_'
			&& let Some((content, end_pos)) = try_parse_delimited(&chars, i, "_")
		{
			result.push_str("<i>");
			result.push_str(&content);
			result.push_str("</i>");
			i = end_pos;
			continue;
		}

		result.push(chars[i]);
		i += 1;
	}

	result
}

/// Escape HTML special characters.
pub fn html_escape(s: &str) -> String {
	s.replace('&', "&amp;")
		.replace('<', "&lt;")
		.replace('>', "&gt;")
}

// ---------------------------------------------------------------------------
// Message chunking
// ---------------------------------------------------------------------------

/// Split a long HTML message into chunks that respect the Telegram max length.
///
/// Prefers breaking at newlines, then spaces, then hard breaks at UTF-8 char
/// boundaries as a last resort.
pub fn chunk_message(html: &str, max_len: usize) -> Vec<String> {
	if html.len() <= max_len {
		return vec![html.to_owned()];
	}

	let mut chunks = Vec::new();
	let mut remaining = html;

	while !remaining.is_empty() {
		if remaining.len() <= max_len {
			chunks.push(remaining.to_owned());
			break;
		}

		let safe_max = {
			let mut i = max_len.min(remaining.len());
			while i > 0 && !remaining.is_char_boundary(i) {
				i -= 1;
			}
			i
		};
		let search_region = &remaining[..safe_max];

		let break_at = if let Some(pos) = search_region.rfind('\n') {
			pos + 1
		} else if let Some(pos) = search_region.rfind(' ') {
			pos + 1
		} else {
			safe_max
		};

		let (chunk, rest) = remaining.split_at(break_at);
		chunks.push(chunk.to_owned());
		remaining = rest;
	}

	chunks
}

// ---------------------------------------------------------------------------
// Tool-call XML stripping
// ---------------------------------------------------------------------------

/// Regex matching complete tool-call XML blocks (paired or self-closing).
static TOOL_CALL_BLOCK_RE: LazyLock<Regex> = LazyLock::new(|| {
	Regex::new(
		r"(?si)<(?:toolcall|tool_call|tool_use|function=[^>]*)(?:\s[^>]*)?>.*?</(?:toolcall|tool_call|tool_use|function)>|<(?:toolcall|tool_call|tool_use|function=[^>]*)(?:\s[^>]*)?/>",
	)
	.expect("tool call block regex must compile")
});

/// Regex matching orphaned tool-call opening or closing tags.
///
/// These appear when a streaming flush boundary splits the opening and closing
/// tags into different buffers.
static TOOL_CALL_TAG_RE: LazyLock<Regex> = LazyLock::new(|| {
	Regex::new(r"(?i)</?(?:toolcall|tool_call|tool_use|function=[^>]*)(?:\s[^>]*)?>")
		.expect("tool call tag regex must compile")
});

/// Remove leaked tool-call XML from streamed LLM text.
///
/// Uses a two-pass approach:
/// 1. Remove complete `<toolcall>…</toolcall>` blocks and self-closing tags.
/// 2. Remove orphaned opening/closing tags left by streaming flush boundaries.
pub fn strip_tool_call_xml(text: &str) -> String {
	let pass1 = TOOL_CALL_BLOCK_RE.replace_all(text, "");
	let pass2 = TOOL_CALL_TAG_RE.replace_all(&pass1, "");
	pass2.into_owned()
}

// ---------------------------------------------------------------------------
// Block-level preprocessing
// ---------------------------------------------------------------------------

/// Pre-process block-level Markdown into inline equivalents.
///
/// Converts headings to bold, strips horizontal rules, removes blockquote
/// markers, and renders tables as monospace `<pre>` blocks.
fn preprocess_blocks(md: &str) -> String {
	let mut lines: Vec<String> = Vec::new();
	let raw_lines: Vec<&str> = md.lines().collect();
	let mut idx = 0usize;

	while idx < raw_lines.len() {
		let line = raw_lines[idx];

		// Markdown table block
		if is_markdown_table_header(line)
			&& idx + 1 < raw_lines.len()
			&& is_markdown_table_separator(raw_lines[idx + 1])
		{
			let mut table_rows: Vec<Vec<String>> = vec![
				parse_table_cells(line)
					.into_iter()
					.map(str::to_owned)
					.collect(),
			];
			idx += 2;
			while idx < raw_lines.len() && is_markdown_table_row(raw_lines[idx]) {
				table_rows.push(
					parse_table_cells(raw_lines[idx])
						.into_iter()
						.map(str::to_owned)
						.collect(),
				);
				idx += 1;
			}

			let rendered_rows = render_table_rows(&table_rows);
			lines.push("```".to_owned());
			lines.extend(rendered_rows);
			lines.push("```".to_owned());
			continue;
		}

		let trimmed = line.trim();

		if let Some(rest) = strip_heading_prefix(trimmed) {
			lines.push(format!("**{rest}**"));
		} else if is_horizontal_rule(trimmed) {
			lines.push(String::new());
		} else if let Some(rest) = trimmed.strip_prefix("> ") {
			lines.push(rest.to_string());
		} else if trimmed == ">" {
			lines.push(String::new());
		} else {
			lines.push(line.to_string());
		}

		idx += 1;
	}
	lines.join("\n")
}

// ---------------------------------------------------------------------------
// Table helpers
// ---------------------------------------------------------------------------

fn parse_table_cells(line: &str) -> Vec<&str> {
	let trimmed = line.trim();
	if trimmed.is_empty() || !trimmed.contains('|') {
		return Vec::new();
	}
	let core = trimmed.trim_matches('|').trim();
	if core.is_empty() {
		return Vec::new();
	}
	core.split('|').map(str::trim).collect()
}

fn is_markdown_table_header(line: &str) -> bool {
	let cells = parse_table_cells(line);
	cells.len() >= 2 && !is_markdown_table_separator(line)
}

fn is_markdown_table_separator(line: &str) -> bool {
	let cells = parse_table_cells(line);
	if cells.len() < 2 {
		return false;
	}
	cells.iter().all(|cell| {
		let stripped = cell.trim().trim_matches(':');
		!stripped.is_empty() && stripped.len() >= 3 && stripped.chars().all(|ch| ch == '-')
	})
}

fn is_markdown_table_row(line: &str) -> bool {
	let cells = parse_table_cells(line);
	cells.len() >= 2 && !is_markdown_table_separator(line)
}

fn render_table_rows(rows: &[Vec<String>]) -> Vec<String> {
	if rows.is_empty() {
		return Vec::new();
	}

	let column_count = rows.iter().map(Vec::len).max().unwrap_or(0);
	if column_count == 0 {
		return Vec::new();
	}

	let mut normalized: Vec<Vec<String>> = rows
		.iter()
		.map(|row| {
			let mut r = row.clone();
			while r.len() < column_count {
				r.push(String::new());
			}
			r
		})
		.collect();

	let widths: Vec<usize> = (0..column_count)
		.map(|column| {
			normalized
				.iter()
				.map(|row| row[column].chars().count())
				.max()
				.unwrap_or(0)
		})
		.collect();

	let render_row = |row: &[String]| -> String {
		let mut line = String::new();
		line.push('|');
		for (idx, width) in widths.iter().enumerate() {
			let cell = row.get(idx).map_or("", String::as_str);
			let pad = width.saturating_sub(cell.chars().count());
			line.push(' ');
			line.push_str(cell);
			line.push_str(&" ".repeat(pad));
			line.push(' ');
			line.push('|');
		}
		line
	};

	let mut out = Vec::new();
	out.push(render_row(&normalized[0]));

	let mut separator = String::new();
	separator.push('|');
	for width in &widths {
		separator.push_str(&"-".repeat(*width + 2));
		separator.push('|');
	}
	out.push(separator);

	for row in normalized.drain(1..) {
		out.push(render_row(&row));
	}

	out
}

// ---------------------------------------------------------------------------
// Inline-level helpers
// ---------------------------------------------------------------------------

fn strip_heading_prefix(line: &str) -> Option<&str> {
	let bytes = line.as_bytes();
	let mut level = 0;
	while level < bytes.len() && level < 6 && bytes[level] == b'#' {
		level += 1;
	}
	if level == 0 {
		return None;
	}
	let rest = &line[level..];
	if rest.is_empty() {
		return Some("");
	}
	if let Some(stripped) = rest.strip_prefix(' ') {
		return Some(stripped.trim());
	}
	None
}

fn is_horizontal_rule(line: &str) -> bool {
	let stripped: String = line.chars().filter(|c| !c.is_whitespace()).collect();
	if stripped.len() < 3 {
		return false;
	}
	let ch = stripped.as_bytes()[0];
	matches!(ch, b'-' | b'*' | b'_') && stripped.bytes().all(|b| b == ch)
}

fn try_parse_link(chars: &[char], pos: usize) -> Option<(String, String, usize)> {
	let len = chars.len();
	if pos >= len || chars[pos] != '[' {
		return None;
	}

	let mut i = pos + 1;
	let mut depth = 1;
	while i < len && depth > 0 {
		match chars[i] {
			'[' => depth += 1,
			']' => depth -= 1,
			_ => {}
		}
		if depth > 0 {
			i += 1;
		}
	}
	if depth != 0 || i >= len {
		return None;
	}

	let text: String = chars[pos + 1..i].iter().collect();
	i += 1;

	if i >= len || chars[i] != '(' {
		return None;
	}
	i += 1;

	let url_start = i;
	let mut paren_depth = 1;
	while i < len && paren_depth > 0 {
		match chars[i] {
			'(' => paren_depth += 1,
			')' => paren_depth -= 1,
			_ => {}
		}
		if paren_depth > 0 {
			i += 1;
		}
	}
	if paren_depth != 0 {
		return None;
	}

	let url: String = chars[url_start..i].iter().collect();
	i += 1;

	Some((text, url, i))
}

fn try_parse_delimited(chars: &[char], pos: usize, delim: &str) -> Option<(String, usize)> {
	let delim_chars: Vec<char> = delim.chars().collect();
	let delim_len = delim_chars.len();
	let len = chars.len();

	if pos + delim_len > len {
		return None;
	}

	for (j, dc) in delim_chars.iter().enumerate() {
		if chars[pos + j] != *dc {
			return None;
		}
	}

	let content_start = pos + delim_len;
	let mut i = content_start;

	while i + delim_len <= len {
		let mut matched = true;
		for (j, dc) in delim_chars.iter().enumerate() {
			if chars[i + j] != *dc {
				matched = false;
				break;
			}
		}
		if matched && i > content_start {
			let content: String = chars[content_start..i].iter().collect();
			return Some((content, i + delim_len));
		}
		i += 1;
	}

	None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
	use super::*;

	// -- markdown_to_telegram_html --

	#[test]
	fn bold_with_asterisks() {
		assert_eq!(
			markdown_to_telegram_html("hello **world**"),
			"hello <b>world</b>"
		);
	}

	#[test]
	fn bold_with_underscores() {
		assert_eq!(
			markdown_to_telegram_html("hello __world__"),
			"hello <b>world</b>"
		);
	}

	#[test]
	fn italic_with_asterisk() {
		assert_eq!(
			markdown_to_telegram_html("hello *world*"),
			"hello <i>world</i>"
		);
	}

	#[test]
	fn italic_with_underscore() {
		assert_eq!(
			markdown_to_telegram_html("hello _world_"),
			"hello <i>world</i>"
		);
	}

	#[test]
	fn inline_code() {
		assert_eq!(
			markdown_to_telegram_html("use `cargo build`"),
			"use <code>cargo build</code>"
		);
	}

	#[test]
	fn fenced_code_block() {
		let input = "```rust\nfn main() {}\n```";
		let html = markdown_to_telegram_html(input);
		assert!(html.contains("<pre>fn main() {}</pre>"));
	}

	#[test]
	fn link() {
		assert_eq!(
			markdown_to_telegram_html("[rust](https://rust-lang.org)"),
			"<a href=\"https://rust-lang.org\">rust</a>"
		);
	}

	#[test]
	fn heading_to_bold() {
		assert_eq!(
			markdown_to_telegram_html("# Hello World"),
			"<b>Hello World</b>"
		);
	}

	#[test]
	fn heading_h3() {
		assert_eq!(
			markdown_to_telegram_html("### Sub-heading"),
			"<b>Sub-heading</b>"
		);
	}

	#[test]
	fn horizontal_rule_removed() {
		let input = "above\n---\nbelow";
		let html = markdown_to_telegram_html(input);
		assert!(!html.contains("---"));
		assert!(html.contains("above\n\nbelow"));
	}

	#[test]
	fn blockquote_stripped() {
		assert_eq!(markdown_to_telegram_html("> quoted text"), "quoted text");
	}

	#[test]
	fn html_entities_escaped() {
		assert_eq!(
			markdown_to_telegram_html("a < b & c > d"),
			"a &lt; b &amp; c &gt; d"
		);
	}

	#[test]
	fn mixed_formatting() {
		let input = "**bold** and *italic* and `code`";
		let html = markdown_to_telegram_html(input);
		assert_eq!(html, "<b>bold</b> and <i>italic</i> and <code>code</code>");
	}

	#[test]
	fn plain_text_passthrough() {
		assert_eq!(
			markdown_to_telegram_html("just plain text"),
			"just plain text"
		);
	}

	#[test]
	fn table_rendered_as_pre() {
		let input = "| Name | Score |\n|---|---|\n| Alice | 100 |";
		let html = markdown_to_telegram_html(input);
		assert!(html.contains("<pre>"));
		assert!(html.contains("Alice"));
	}

	#[test]
	fn pipe_without_separator_not_table() {
		let input = "A | B\njust text";
		let html = markdown_to_telegram_html(input);
		assert!(!html.contains("<pre>"));
	}

	// -- html_escape --

	#[test]
	fn escape_all_entities() {
		assert_eq!(html_escape("<b>&</b>"), "&lt;b&gt;&amp;&lt;/b&gt;");
	}

	// -- chunk_message --

	#[test]
	fn short_message_single_chunk() {
		let chunks = chunk_message("hello", 4096);
		assert_eq!(chunks, vec!["hello"]);
	}

	#[test]
	fn long_message_splits_at_newline() {
		let chunk_a = "a".repeat(100);
		let chunk_b = "b".repeat(100);
		let input = format!("{chunk_a}\n{chunk_b}");
		let chunks = chunk_message(&input, 110);
		assert_eq!(chunks.len(), 2);
		assert!(chunks[0].ends_with('\n'));
	}

	#[test]
	fn long_message_splits_at_space() {
		let input = format!("{} {}", "a".repeat(50), "b".repeat(50));
		let chunks = chunk_message(&input, 60);
		assert_eq!(chunks.len(), 2);
	}

	#[test]
	fn hard_break_on_no_space() {
		let input = "a".repeat(200);
		let chunks = chunk_message(&input, 100);
		assert_eq!(chunks.len(), 2);
		assert_eq!(chunks[0].len(), 100);
	}

	#[test]
	fn multibyte_utf8_boundary() {
		// Each CJK character is 3 bytes in UTF-8
		let input = "你".repeat(50); // 150 bytes
		let chunks = chunk_message(&input, 100);
		assert!(chunks.len() >= 2);
		// Each chunk must be valid UTF-8 (guaranteed by String type)
		for chunk in &chunks {
			assert!(chunk.len() <= 100);
		}
	}

	// -- strip_tool_call_xml --

	#[test]
	fn strip_complete_block() {
		let input = "hello <toolcall>some tool content</toolcall> world";
		assert_eq!(strip_tool_call_xml(input), "hello  world");
	}

	#[test]
	fn strip_self_closing_tag() {
		let input = "hello <tool_call name=\"search\"/> world";
		assert_eq!(strip_tool_call_xml(input), "hello  world");
	}

	#[test]
	fn strip_orphaned_tags() {
		let input = "hello </toolcall> world";
		assert_eq!(strip_tool_call_xml(input), "hello  world");
	}

	#[test]
	fn no_xml_passthrough() {
		let input = "just normal text";
		assert_eq!(strip_tool_call_xml(input), "just normal text");
	}

	#[test]
	fn strip_tool_use_variant() {
		let input = "before <tool_use>call</tool_use> after";
		assert_eq!(strip_tool_call_xml(input), "before  after");
	}
}
