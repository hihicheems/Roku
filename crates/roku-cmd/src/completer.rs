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

//! Tab-completion for slash commands in the interactive REPL.
//!
//! Implements the rustyline [`Completer`] trait so that pressing Tab after `/`
//! offers all registered commands.

use rustyline::completion::{Completer, Pair};
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::validate::Validator;
use rustyline::{Context, Helper};

/// All known slash commands in the REPL.
const COMMANDS: &[&str] = &[
	"/clear", "/compact", "/exit", "/help", "/login", "/logout", "/quit", "/session", "/switch",
];

/// Subcommands for `/session`.
const SESSION_SUBS: &[&str] = &["list", "new", "switch"];

/// A rustyline [`Helper`] that provides slash-command completion.
#[derive(Default)]
pub(crate) struct RokuHelper;

impl Completer for RokuHelper {
	type Candidate = Pair;

	fn complete(
		&self,
		line: &str,
		pos: usize,
		_ctx: &Context<'_>,
	) -> rustyline::Result<(usize, Vec<Pair>)> {
		let prefix = &line[..pos];

		// `/session <sub>` completion.
		if let Some(rest) = prefix.strip_prefix("/session ") {
			let sub = rest.trim_start();
			let start = pos - sub.len();
			let matches: Vec<Pair> = SESSION_SUBS
				.iter()
				.filter(|s| s.starts_with(sub))
				.map(|s| Pair {
					display: s.to_string(),
					replacement: s.to_string(),
				})
				.collect();
			return Ok((start, matches));
		}

		// Top-level `/` command completion.
		if prefix.starts_with('/') {
			let matches: Vec<Pair> = COMMANDS
				.iter()
				.filter(|cmd| cmd.starts_with(prefix))
				.map(|cmd| Pair {
					display: cmd.to_string(),
					replacement: cmd.to_string(),
				})
				.collect();
			return Ok((0, matches));
		}

		Ok((pos, Vec::new()))
	}
}

impl Hinter for RokuHelper {
	type Hint = String;
}
impl Highlighter for RokuHelper {}
impl Validator for RokuHelper {}
impl Helper for RokuHelper {}

#[cfg(test)]
mod tests {
	use super::*;

	fn complete_line(input: &str) -> Vec<String> {
		let helper = RokuHelper;
		let (_start, pairs) = helper
			.complete(
				input,
				input.len(),
				&Context::new(&rustyline::history::DefaultHistory::new()),
			)
			.unwrap();
		pairs.into_iter().map(|p| p.replacement).collect()
	}

	#[test]
	fn completes_slash_prefix() {
		let results = complete_line("/he");
		assert_eq!(results, vec!["/help"]);
	}

	#[test]
	fn completes_all_commands_on_slash() {
		let results = complete_line("/");
		assert_eq!(results.len(), COMMANDS.len());
	}

	#[test]
	fn completes_session_subcommands() {
		let results = complete_line("/session s");
		assert_eq!(results, vec!["switch"]);
	}

	#[test]
	fn no_completion_for_plain_text() {
		let results = complete_line("hello");
		assert!(results.is_empty());
	}
}
