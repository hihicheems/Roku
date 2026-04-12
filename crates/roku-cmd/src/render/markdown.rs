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

//! Markdown rendering for the interactive REPL.
//!
//! Two entry points:
//! - [`render_markdown`]: batch render a complete markdown string to styled terminal output.
//! - [`StreamRenderer`]: streaming state machine that handles incremental text deltas
//!   (normal passthrough vs code-block buffered highlighting).

use crossterm::style::{Attribute, Color, Stylize};
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;

use super::style::no_color;

// ---------------------------------------------------------------------------
// Lazy-loaded syntect resources
// ---------------------------------------------------------------------------

fn syntax_set() -> &'static SyntaxSet {
	use std::sync::LazyLock;
	static SS: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);
	&SS
}

fn theme_set() -> &'static ThemeSet {
	use std::sync::LazyLock;
	static TS: LazyLock<ThemeSet> = LazyLock::new(ThemeSet::load_defaults);
	&TS
}

// ---------------------------------------------------------------------------
// Batch markdown rendering
// ---------------------------------------------------------------------------

/// Render a complete markdown string to styled terminal text.
pub(crate) fn render_markdown(input: &str) -> String {
	if no_color() {
		return input.to_string();
	}

	let mut opts = Options::empty();
	opts.insert(Options::ENABLE_STRIKETHROUGH);
	let parser = Parser::new_ext(input, opts);

	let mut output = String::new();
	let mut in_code_block = false;
	let mut code_lang = String::new();
	let mut code_buf = String::new();
	let mut list_depth: usize = 0;
	let mut in_heading = false;

	for event in parser {
		match event {
			Event::Start(Tag::CodeBlock(kind)) => {
				in_code_block = true;
				code_buf.clear();
				code_lang = match kind {
					pulldown_cmark::CodeBlockKind::Fenced(lang) => lang.to_string(),
					pulldown_cmark::CodeBlockKind::Indented => String::new(),
				};
			}
			Event::End(TagEnd::CodeBlock) => {
				in_code_block = false;
				output.push_str(&highlight_code(&code_buf, &code_lang));
				code_buf.clear();
			}
			Event::Start(Tag::Heading { level, .. }) => {
				in_heading = true;
				// Add heading marker.
				let marker = match level {
					pulldown_cmark::HeadingLevel::H1 => "# ",
					pulldown_cmark::HeadingLevel::H2 => "## ",
					pulldown_cmark::HeadingLevel::H3 => "### ",
					_ => "#### ",
				};
				output.push_str(&marker.bold().to_string());
			}
			Event::End(TagEnd::Heading(_)) => {
				in_heading = false;
				output.push('\n');
			}
			Event::Start(Tag::List(_)) => {
				list_depth += 1;
			}
			Event::End(TagEnd::List(_)) => {
				list_depth = list_depth.saturating_sub(1);
			}
			Event::Start(Tag::Item) => {
				let indent = "  ".repeat(list_depth.saturating_sub(1));
				output.push_str(&format!("{indent}• "));
			}
			Event::End(TagEnd::Item) => {
				output.push('\n');
			}
			Event::Start(Tag::BlockQuote(_)) => {
				output.push_str(&"│ ".with(Color::DarkGrey).to_string());
			}
			Event::Start(Tag::Emphasis) => {
				output.push_str(&format!("{}", Attribute::Italic));
			}
			Event::End(TagEnd::Emphasis) => {
				output.push_str(&format!("{}", Attribute::NoItalic));
			}
			Event::Start(Tag::Strong) => {
				output.push_str(&format!("{}", Attribute::Bold));
			}
			Event::End(TagEnd::Strong) => {
				output.push_str(&format!("{}", Attribute::NoBold));
			}
			Event::Code(code) => {
				// Inline code: dim background-like styling.
				output.push_str(&format!("`{}`", code.with(Color::DarkYellow)));
			}
			Event::Text(text) => {
				if in_code_block {
					code_buf.push_str(&text);
				} else if in_heading {
					output.push_str(&text.bold().to_string());
				} else {
					output.push_str(&text);
				}
			}
			Event::SoftBreak | Event::HardBreak => {
				if !in_code_block {
					output.push('\n');
				}
			}
			Event::Start(Tag::Paragraph) => {}
			Event::End(TagEnd::Paragraph) => {
				output.push_str("\n\n");
			}
			Event::Start(Tag::Link { dest_url, .. }) => {
				// OSC 8 hyperlink start.
				output.push_str(&format!("\x1b]8;;{dest_url}\x1b\\"));
			}
			Event::End(TagEnd::Link) => {
				output.push_str("\x1b]8;;\x1b\\");
			}
			Event::Start(Tag::Strikethrough) => {
				output.push_str(&format!("{}", Attribute::CrossedOut));
			}
			Event::End(TagEnd::Strikethrough) => {
				output.push_str(&format!("{}", Attribute::NotCrossedOut));
			}
			_ => {}
		}
	}

	// Trim trailing double-newline from final paragraph.
	if output.ends_with("\n\n") {
		output.truncate(output.len() - 1);
	}

	output
}

/// Syntax-highlight a code block, returning styled terminal text.
fn highlight_code(code: &str, lang: &str) -> String {
	let ss = syntax_set();
	let ts = theme_set();

	// Map common language aliases.
	let lang_key = match lang {
		"js" | "javascript" => "JavaScript",
		"ts" | "typescript" => "TypeScript",
		"py" | "python" => "Python",
		"rs" | "rust" => "Rust",
		"sh" | "bash" | "shell" | "zsh" => "Bourne Again Shell (bash)",
		"toml" => "TOML",
		"json" | "jsonc" => "JSON",
		"yaml" | "yml" => "YAML",
		"md" | "markdown" => "Markdown",
		"go" | "golang" => "Go",
		"rb" | "ruby" => "Ruby",
		"sql" => "SQL",
		"css" => "CSS",
		"html" => "HTML",
		"xml" => "XML",
		"c" => "C",
		"cpp" | "c++" => "C++",
		"java" => "Java",
		other => other,
	};

	let syntax = ss
		.find_syntax_by_name(lang_key)
		.or_else(|| ss.find_syntax_by_extension(lang))
		.unwrap_or_else(|| ss.find_syntax_plain_text());

	let theme = &ts.themes["base16-ocean.dark"];

	let highlighted = highlight_multiline(code, syntax, theme, ss);

	let mut output = String::new();
	output.push_str(&"  ┌".with(Color::DarkGrey).to_string());
	if !lang.is_empty() {
		output.push_str(&format!(" {}", lang.with(Color::DarkGrey)));
	}
	output.push('\n');

	for line in highlighted.lines() {
		output.push_str(&"  │ ".with(Color::DarkGrey).to_string());
		output.push_str(line);
		output.push('\n');
	}

	output.push_str(&"  └".with(Color::DarkGrey).to_string());
	output.push('\n');
	output
}

/// Highlight multi-line code using syntect line-by-line.
fn highlight_multiline(
	code: &str,
	syntax: &syntect::parsing::SyntaxReference,
	theme: &syntect::highlighting::Theme,
	ss: &SyntaxSet,
) -> String {
	use syntect::easy::HighlightLines;
	use syntect::util::as_24_bit_terminal_escaped;

	let mut h = HighlightLines::new(syntax, theme);
	let mut output = String::new();

	for line in code.lines() {
		match h.highlight_line(line, ss) {
			Ok(ranges) => {
				output.push_str(&as_24_bit_terminal_escaped(&ranges, false));
				output.push_str("\x1b[0m"); // Reset after each line.
			}
			Err(_) => {
				output.push_str(line);
			}
		}
		output.push('\n');
	}

	// Remove trailing newline to avoid double-newline in code block.
	if output.ends_with('\n') {
		output.truncate(output.len() - 1);
	}

	output
}

// ---------------------------------------------------------------------------
// Streaming renderer
// ---------------------------------------------------------------------------

/// State machine for rendering streaming LLM text deltas.
///
/// Operates in two modes:
/// - **Normal**: text is passed through directly (no per-token markdown parsing).
/// - **InCodeBlock**: text is buffered until the closing fence, then highlighted.
pub(crate) struct StreamRenderer {
	state: StreamState,
	code_buf: String,
	code_lang: String,
	/// Accumulates the line currently being received (for fence detection).
	line_buf: String,
	/// How many bytes of `line_buf` have already been emitted (for partial lines).
	emitted_len: usize,
}

enum StreamState {
	Normal,
	InCodeBlock,
}

impl StreamRenderer {
	pub fn new() -> Self {
		Self {
			state: StreamState::Normal,
			code_buf: String::new(),
			code_lang: String::new(),
			line_buf: String::new(),
			emitted_len: 0,
		}
	}

	/// Process an incremental text delta, returning text to emit immediately.
	///
	/// In normal mode, text passes through directly (partial lines emitted
	/// incrementally without duplication). In code-block mode, text is
	/// buffered until the closing fence, then emitted with gutter.
	pub fn push(&mut self, delta: &str) -> String {
		if no_color() {
			return delta.to_string();
		}

		let mut output = String::new();

		for ch in delta.chars() {
			self.line_buf.push(ch);

			if ch == '\n' {
				let prev_emitted = self.emitted_len;
				let line = std::mem::take(&mut self.line_buf);
				self.emitted_len = 0;
				match self.state {
					StreamState::Normal => {
						if line.trim_start().starts_with("```") {
							// Opening fence detected.
							self.state = StreamState::InCodeBlock;
							self.code_lang = line
								.trim_start()
								.strip_prefix("```")
								.unwrap_or("")
								.trim()
								.to_string();
							self.code_buf.clear();
							// Emit code block header.
							output.push_str(&"  ┌".with(Color::DarkGrey).to_string());
							if !self.code_lang.is_empty() {
								output.push_str(&format!(
									" {}",
									self.code_lang.as_str().with(Color::DarkGrey)
								));
							}
							output.push('\n');
						} else {
							// Emit only the portion not yet emitted (the newline itself).
							output.push_str(&line[prev_emitted..]);
						}
					}
					StreamState::InCodeBlock => {
						if line.trim_start().starts_with("```") {
							// Closing fence — flush highlighted code.
							output.push_str(&flush_code_block(&self.code_buf, &self.code_lang));
							output.push_str(&"  └".with(Color::DarkGrey).to_string());
							output.push('\n');
							self.code_buf.clear();
							self.state = StreamState::Normal;
						} else {
							// Buffer code line; emit with gutter for immediate feedback.
							output.push_str(&"  │ ".with(Color::DarkGrey).to_string());
							output.push_str(&line);
							self.code_buf.push_str(&line);
						}
					}
				}
			}
		}

		// If there's a partial line remaining (no newline yet), emit it in normal mode only.
		// Exception: if the partial line looks like it might be a code fence opening
		// (starts with `), buffer it until newline so fence detection can work.
		if self.line_buf.len() > self.emitted_len {
			match self.state {
				StreamState::Normal => {
					let pending = self.line_buf.trim_start();
					if pending.starts_with('`') {
						// Potential fence — hold until newline confirms.
					} else {
						output.push_str(&self.line_buf[self.emitted_len..]);
						self.emitted_len = self.line_buf.len();
					}
				}
				StreamState::InCodeBlock => {
					// Don't emit partial code lines — wait for newline.
				}
			}
		}

		output
	}

	/// Flush any remaining buffered state (call at end of streaming).
	pub fn flush(&mut self) -> String {
		let mut output = String::new();
		match self.state {
			StreamState::InCodeBlock => {
				if !self.line_buf.is_empty() {
					self.code_buf.push_str(&self.line_buf);
					output.push_str(&"  │ ".with(Color::DarkGrey).to_string());
					output.push_str(&self.line_buf);
					output.push('\n');
					self.line_buf.clear();
					self.emitted_len = 0;
				}
				output.push_str(&"  └".with(Color::DarkGrey).to_string());
				output.push('\n');
				self.state = StreamState::Normal;
			}
			StreamState::Normal => {
				// Any remaining partial line was already emitted incrementally.
				self.line_buf.clear();
				self.emitted_len = 0;
			}
		}
		output
	}
}

/// Highlight buffered code and format with gutter prefix.
fn flush_code_block(_code: &str, _lang: &str) -> String {
	// During streaming, code lines were already emitted with gutter.
	// The final highlighted version is only used in batch render_markdown().
	String::new()
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn render_markdown_heading() {
		// SAFETY: test-only env manipulation, single-threaded test.
		unsafe { std::env::set_var("NO_COLOR", "1") };
		let result = render_markdown("# Hello\n\nWorld");
		assert!(result.contains("# Hello"));
		assert!(result.contains("World"));
		unsafe { std::env::remove_var("NO_COLOR") };
	}

	#[test]
	fn render_markdown_code_block_no_color() {
		unsafe { std::env::set_var("NO_COLOR", "1") };
		let input = "```rust\nfn main() {}\n```\n";
		let result = render_markdown(input);
		assert!(result.contains("fn main()"));
		unsafe { std::env::remove_var("NO_COLOR") };
	}

	#[test]
	fn stream_renderer_normal_passthrough() {
		unsafe { std::env::set_var("NO_COLOR", "1") };
		let mut sr = StreamRenderer::new();
		let out = sr.push("hello world\n");
		assert_eq!(out, "hello world\n");
		unsafe { std::env::remove_var("NO_COLOR") };
	}
}
