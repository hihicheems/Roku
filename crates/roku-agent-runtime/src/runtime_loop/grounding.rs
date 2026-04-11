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

//! Deterministic grounding helpers shared by the route classifier and runtime loop.
//!
//! ## Overview
//!
//! This module performs shallow, best-effort grounding from the current natural-language turn
//! into concrete runtime hints such as:
//!
//! - local path candidates
//! - table file hints
//! - explicit shell or Python snippets
//! - skill source URLs
//! - user reply disambiguation against previously offered candidates
//!
//! The helpers here intentionally stay lexical and local. They should be reusable from both
//! [`crate::router::classifier`] and [`crate::runtime_loop::tool_loop`] so that the runtime does
//! not grow multiple drifting copies of the same grounding heuristics.
//!
//! ## Responsibilities
//!
//! This module is allowed to:
//!
//! - extract grounded arguments from the current request text
//! - recognize explicit resource/modality signals such as paths, glob patterns, table files,
//!   inline commands, code blocks, and skill URLs
//! - help resume disambiguation when the runtime has already asked the user to pick one candidate
//!
//! ## Non-Goals
//!
//! This module must **not**:
//!
//! - choose the final route for a request
//! - decide terminal branches such as `ask_user`, `final_answer`, or `fail`
//! - interpret `ToolObservation`
//! - own retry, recovery, or budget policy
//! - encode multi-step follow-up flow or hidden planning state
//!
//! In other words, this module may produce **grounding signals** for explicit resources and
//! argument alignment, but it must not become a hidden semantic router or "static decision
//! center".
//!
//! ## Design Constraints
//!
//! - Keep heuristics current-turn scoped and cheap to evaluate.
//! - Prefer reusable helpers over duplicating similar logic in classifier/loop callers.
//! - When adding a new helper, ensure it describes observable input structure rather than a
//!   hard-coded follow-up workflow.
//! - Prefer explicit-resource extractors over natural-language intent predicates.
//! - If a helper starts deciding "which tool should go first" for a vague request, it belongs in
//!   the classifier or the live loop instead.
//!
use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use roku_plugin_skills::SkillSource;

pub(crate) fn clean_token(token: &str) -> String {
	if matches!(token, "." | "..") {
		return token.to_string();
	}
	token
		.trim_matches(|character: char| {
			matches!(
				character,
				'"' | '\''
					| '`' | '(' | ')'
					| '[' | ']' | '{'
					| '}' | '<' | '>'
					| ',' | ';' | '!'
					| '?'
			)
		})
		.trim_end_matches(':')
		.trim_end_matches('.')
		.to_string()
}

pub(crate) fn extract_path_candidates(goal: &str) -> Vec<String> {
	let workspace_entries = visible_workspace_entries(64);
	let mut paths = Vec::new();
	for token in goal.split_whitespace().map(clean_token) {
		if token.is_empty() || token.starts_with("http://") || token.starts_with("https://") {
			continue;
		}
		if is_standalone_path_candidate(&token) || workspace_entries.contains(&token) {
			paths.push(token.clone());
		}
		for fragment in embedded_path_fragments(&token, &workspace_entries) {
			if !paths.iter().any(|existing| existing == &fragment) {
				paths.push(fragment);
			}
		}
	}
	paths.dedup();
	paths
}

pub(crate) fn extract_explicit_path_candidates(goal: &str) -> Vec<String> {
	let mut paths = Vec::new();
	for token in goal.split_whitespace().map(clean_token) {
		if token.is_empty() || token.starts_with("http://") || token.starts_with("https://") {
			continue;
		}
		if is_standalone_path_candidate(&token) {
			paths.push(token.clone());
		}
		for fragment in embedded_path_fragments(&token, &[]) {
			if !paths.iter().any(|existing| existing == &fragment) {
				paths.push(fragment);
			}
		}
	}
	paths.dedup();
	paths
}

pub(crate) fn extract_concrete_path_candidates(goal: &str) -> Vec<String> {
	extract_explicit_path_candidates(goal)
		.into_iter()
		.filter(|path| is_concrete_path_candidate(path))
		.collect()
}

pub(crate) fn extract_explicit_table_path(goal: &str) -> Option<String> {
	extract_explicit_path_candidates(goal)
		.into_iter()
		.find(|path| {
			let normalized = path.to_ascii_lowercase();
			normalized.ends_with(".csv")
				|| normalized.ends_with(".tsv")
				|| normalized.ends_with(".xlsx")
		})
}

pub(crate) fn extract_concrete_table_path(goal: &str) -> Option<String> {
	extract_concrete_path_candidates(goal)
		.into_iter()
		.find(|path| {
			let normalized = path.to_ascii_lowercase();
			normalized.ends_with(".csv")
				|| normalized.ends_with(".tsv")
				|| normalized.ends_with(".xlsx")
		})
}

pub(crate) fn extract_glob_pattern(goal: &str) -> Option<String> {
	goal.split_whitespace()
		.map(clean_token)
		.find(|token| token.contains('*') || token.contains('?') || token.contains('['))
}

pub(crate) fn goal_requests_web_lookup(goal: &str) -> bool {
	let lower = goal.trim().to_ascii_lowercase();
	[
		"search the web",
		"search web",
		"search online",
		"web search",
		"look up",
		"lookup",
		"google ",
		"bing ",
		"网上搜索",
		"网络搜索",
		"查一下",
	]
	.iter()
	.any(|marker| lower.contains(marker))
}

pub(crate) fn extract_sheet_name(goal: &str) -> Option<String> {
	let lower = goal.to_ascii_lowercase();
	let marker = "sheet ";
	let index = lower.find(marker)?;
	let suffix = goal.get(index + marker.len()..)?.trim();
	let name = suffix
		.trim_matches(|character: char| matches!(character, '"' | '\'' | '`' | ',' | '.' | ';'));
	(!name.is_empty() && !name.contains(' ')).then(|| name.to_string())
}

pub(crate) fn extract_row_limit(goal: &str) -> Option<u64> {
	let digits = goal
		.split_whitespace()
		.find_map(|part| clean_token(part).parse::<u64>().ok())?;
	Some(digits.clamp(1, 50))
}

pub(crate) fn extract_web_query(goal: &str) -> Option<String> {
	if !goal_requests_web_lookup(goal) {
		return None;
	}
	let trimmed = goal.trim();
	let lower = trimmed.to_ascii_lowercase();
	for marker in [
		"search the web",
		"search web",
		"search online",
		"web search",
		"look up",
		"lookup",
		"google",
		"bing",
		"网上搜索",
		"网络搜索",
		"查一下",
	] {
		if let Some(index) = lower.find(marker) {
			let suffix = trimmed
				.get(index + marker.len()..)
				.unwrap_or_default()
				.trim()
				.trim_start_matches([':', '-', ' '])
				.strip_prefix("for ")
				.unwrap_or_else(|| {
					trimmed
						.get(index + marker.len()..)
						.unwrap_or_default()
						.trim()
						.trim_start_matches([':', '-', ' '])
				});
			let query = suffix.trim_matches(|character: char| {
				matches!(character, '"' | '\'' | '`' | '.' | '!' | '?' | ' ')
			});
			if !query.is_empty() {
				return Some(query.to_string());
			}
		}
	}
	None
}

pub(crate) fn extract_explicit_python_code(goal: &str) -> Option<String> {
	if let Some(code) = extract_fenced_python_code(goal) {
		return Some(code);
	}
	if let Some(code) = extract_inline_code(goal).filter(|code| is_probable_python_snippet(code)) {
		return Some(code);
	}
	extract_line_or_block_python_code(goal)
}

pub(crate) fn extract_explicit_shell_command(goal: &str) -> Option<String> {
	if let Some(command) = extract_fenced_shell_command(goal) {
		return Some(command);
	}
	if let Some(command) = extract_inline_code(goal)
		.as_deref()
		.and_then(normalize_shell_command)
	{
		return Some(command);
	}
	if let Some(command) = extract_dollar_prefixed_command(goal) {
		return Some(command);
	}
	extract_command_suffix_after_separator(goal)
}

pub(crate) fn extract_skill_source_url(goal: &str) -> Option<String> {
	goal.split_whitespace().find_map(|token| {
		SkillSource::parse(token.trim_matches(|character: char| {
			matches!(
				character,
				'(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | '"' | '\'' | ',' | ';' | '.'
			)
		}))
		.ok()
		.map(|source| source.original_url().to_string())
	})
}

pub(crate) fn file_name_from_path(path: &str) -> Option<String> {
	PathBuf::from(path)
		.file_name()
		.and_then(|name| name.to_str())
		.map(str::to_string)
}

pub(crate) fn reply_selects_candidate(reply: &str, candidates: &[String]) -> Option<String> {
	let explicit_paths = extract_path_candidates(reply);
	for candidate in candidates {
		if explicit_paths.iter().any(|path| path == candidate) {
			return Some(candidate.clone());
		}
	}
	let tokens = reply
		.split_whitespace()
		.map(clean_token)
		.filter(|token| !token.is_empty())
		.collect::<Vec<_>>();
	let reply_fragments = candidate_reply_fragments(reply, candidates);
	for candidate in candidates {
		let basename = file_name_from_path(candidate)?;
		if tokens
			.iter()
			.chain(reply_fragments.iter())
			.any(|token| token == candidate || token == &basename)
		{
			return Some(candidate.clone());
		}
	}
	let segment_tokens = tokens
		.into_iter()
		.chain(reply_fragments)
		.collect::<Vec<_>>();
	select_unique_candidate_from_segments(&segment_tokens, candidates)
}

fn select_unique_candidate_from_segments(
	tokens: &[String],
	candidates: &[String],
) -> Option<String> {
	for token in tokens {
		let token = token.trim();
		if token.len() < 4 {
			continue;
		}
		let mut matches = candidates
			.iter()
			.filter(|candidate| {
				candidate_path_segments(candidate)
					.iter()
					.any(|segment| segment.contains(token))
			})
			.cloned()
			.collect::<Vec<_>>();
		matches.dedup();
		if matches.len() == 1 {
			return matches.into_iter().next();
		}
	}
	None
}

fn candidate_path_segments(candidate: &str) -> Vec<String> {
	PathBuf::from(candidate)
		.components()
		.filter_map(|component| component.as_os_str().to_str().map(str::to_string))
		.collect()
}

pub(crate) fn visible_workspace_entries(limit: usize) -> Vec<String> {
	let Ok(cwd) = std::env::current_dir() else {
		return Vec::new();
	};
	let Ok(entries) = fs::read_dir(cwd) else {
		return Vec::new();
	};
	let mut names = entries
		.filter_map(Result::ok)
		.filter_map(|entry| entry.file_name().to_str().map(str::to_string))
		.collect::<BTreeSet<_>>()
		.into_iter()
		.collect::<Vec<_>>();
	names.truncate(limit);
	names
}

fn looks_like_path_candidate(token: &str) -> bool {
	if token.is_empty() {
		return false;
	}
	if matches!(token, "." | ".." | "~") || token.starts_with("~/") || token.starts_with("~\\") {
		return true;
	}
	token.contains('/')
		|| token.contains('\\')
		|| token.rsplit_once('.').is_some_and(|(stem, ext)| {
			!ext.is_empty()
				&& ext
					.chars()
					.all(|character| character.is_ascii_alphanumeric())
				&& (looks_like_known_file_extension(ext)
					|| stem.contains('-')
					|| stem.contains('_')
					|| stem.chars().any(|character| character.is_ascii_uppercase())
					|| stem.chars().any(|character| character.is_ascii_digit()))
		})
}

fn looks_like_known_file_extension(extension: &str) -> bool {
	matches!(
		extension.to_ascii_lowercase().as_str(),
		"txt"
			| "md" | "markdown"
			| "rs" | "toml"
			| "json" | "yaml"
			| "yml" | "csv"
			| "tsv" | "xlsx"
			| "xls" | "env"
			| "lock" | "log"
			| "py" | "js"
			| "ts" | "jsx"
			| "tsx" | "html"
			| "css" | "sh"
			| "bash" | "zsh"
			| "sql"
	)
}

fn is_standalone_path_candidate(token: &str) -> bool {
	if matches!(token, "." | ".." | "~") {
		return true;
	}
	!token.is_empty()
		&& token.chars().all(is_path_fragment_char)
		&& looks_like_path_candidate(token)
}

fn is_concrete_path_candidate(token: &str) -> bool {
	matches!(token, "." | ".." | "~")
		|| token.starts_with("~/")
		|| token.starts_with("~\\")
		|| token.contains('/')
		|| token.contains('\\')
		|| candidate_exists_in_current_workspace(token)
}

fn candidate_exists_in_current_workspace(token: &str) -> bool {
	let Ok(cwd) = std::env::current_dir() else {
		return false;
	};
	cwd.join(token).exists()
}

fn embedded_path_fragments(token: &str, workspace_entries: &[String]) -> Vec<String> {
	let mut fragments = Vec::new();
	let mut current = String::new();
	for character in token.chars() {
		if is_path_fragment_char(character) {
			current.push(character);
			continue;
		}
		push_grounded_path_fragment(&mut fragments, &mut current, workspace_entries);
	}
	push_grounded_path_fragment(&mut fragments, &mut current, workspace_entries);
	fragments
}

fn candidate_reply_fragments(reply: &str, candidates: &[String]) -> Vec<String> {
	let vocabulary = candidate_segment_vocabulary(candidates);
	let mut fragments = Vec::new();
	let mut current = String::new();
	for character in reply.chars() {
		if is_path_fragment_char(character) {
			current.push(character);
			continue;
		}
		push_candidate_reply_fragment(&mut fragments, &mut current, &vocabulary);
	}
	push_candidate_reply_fragment(&mut fragments, &mut current, &vocabulary);
	fragments
}

fn candidate_segment_vocabulary(candidates: &[String]) -> Vec<String> {
	let mut segments = candidates
		.iter()
		.flat_map(|candidate| candidate_path_segments(candidate))
		.filter(|segment| !segment.is_empty())
		.collect::<Vec<_>>();
	segments.sort();
	segments.dedup();
	segments
}

fn push_grounded_path_fragment(
	fragments: &mut Vec<String>,
	current: &mut String,
	workspace_entries: &[String],
) {
	if current.is_empty() {
		return;
	}
	let fragment = current.clone();
	current.clear();
	if fragment.starts_with("http://") || fragment.starts_with("https://") {
		return;
	}
	if (looks_like_path_candidate(&fragment) || workspace_entries.contains(&fragment))
		&& !fragments.iter().any(|existing| existing == &fragment)
	{
		fragments.push(fragment);
	}
}

fn push_candidate_reply_fragment(
	fragments: &mut Vec<String>,
	current: &mut String,
	candidate_segments: &[String],
) {
	if current.is_empty() {
		return;
	}
	let fragment = current.clone();
	current.clear();
	if fragment.starts_with("http://") || fragment.starts_with("https://") {
		return;
	}
	if (looks_like_path_candidate(&fragment)
		|| candidate_segments
			.iter()
			.any(|segment| segment == &fragment))
		&& !fragments.iter().any(|existing| existing == &fragment)
	{
		fragments.push(fragment);
	}
}

fn is_path_fragment_char(character: char) -> bool {
	character.is_ascii_alphanumeric() || matches!(character, '.' | '/' | '\\' | '_' | '-' | '~')
}

fn extract_fenced_python_code(goal: &str) -> Option<String> {
	let fenced = goal.find("```")?;
	let rest = goal.get(fenced + 3..)?;
	let rest = rest.strip_prefix("python").unwrap_or(rest);
	let rest = rest.strip_prefix('\n').unwrap_or(rest);
	let end = rest.find("```")?;
	let code = rest.get(..end)?.trim();
	(!code.is_empty()).then(|| code.to_string())
}

fn extract_inline_code(goal: &str) -> Option<String> {
	let start = goal.find('`')?;
	let rest = goal.get(start + 1..)?;
	let end = rest.find('`')?;
	let code = rest.get(..end)?.trim();
	(!code.is_empty()).then(|| code.to_string())
}

fn extract_fenced_shell_command(goal: &str) -> Option<String> {
	for language in ["bash", "sh", "shell", "zsh"] {
		let marker = format!("```{language}\n");
		let Some(start) = goal.find(&marker) else {
			continue;
		};
		let rest = goal.get(start + marker.len()..)?;
		let end = rest.find("```")?;
		let command = rest.get(..end)?.trim();
		if let Some(command) = normalize_shell_command(command) {
			return Some(command);
		}
	}
	None
}

fn extract_line_or_block_python_code(goal: &str) -> Option<String> {
	let trimmed = goal.trim();
	if let Some(suffix) = extract_python_suffix_after_separator(trimmed) {
		return Some(suffix);
	}
	if is_probable_python_snippet(trimmed) {
		return Some(trimmed.to_string());
	}

	let mut lines = trimmed.lines().map(str::trim_end).collect::<Vec<_>>();
	while lines.first().is_some_and(|line| line.trim().is_empty()) {
		lines.remove(0);
	}
	while lines.last().is_some_and(|line| line.trim().is_empty()) {
		lines.pop();
	}
	if lines.len() < 2 {
		return None;
	}
	let block_start = lines
		.iter()
		.position(|line| is_probable_python_snippet(line.trim()))
		.unwrap_or(1);
	let code = lines
		.iter()
		.skip(block_start)
		.copied()
		.collect::<Vec<_>>()
		.join("\n")
		.trim()
		.to_string();
	(!code.is_empty() && is_probable_python_snippet(code.lines().next().unwrap_or_default()))
		.then_some(code)
}

fn extract_python_suffix_after_separator(value: &str) -> Option<String> {
	value
		.match_indices([':', '：'])
		.filter_map(|(index, _)| {
			let prefix = value.get(..index)?.trim();
			let suffix = value.get(index + 1..)?.trim();
			(!suffix.is_empty()
				&& is_probable_python_snippet(suffix)
				&& !is_probable_python_snippet(prefix))
			.then(|| suffix.to_string())
		})
		.next_back()
}

fn is_probable_python_snippet(value: &str) -> bool {
	let trimmed = value.trim();
	if trimmed.is_empty() {
		return false;
	}
	if trimmed.lines().count() > 1 {
		return trimmed.lines().all(|line| {
			let line = line.trim();
			line.is_empty() || is_probable_python_statement(line)
		});
	}
	is_probable_python_statement(trimmed)
}

fn is_probable_python_statement(line: &str) -> bool {
	let trimmed = line.trim();
	if trimmed.is_empty() {
		return false;
	}
	let punctuation_score = [
		trimmed.contains('('),
		trimmed.contains(')'),
		trimmed.contains(':'),
		trimmed.contains('='),
		trimmed.contains('['),
		trimmed.contains(']'),
	]
	.into_iter()
	.filter(|flag| *flag)
	.count();
	let keyword_score = [
		trimmed.starts_with("print"),
		trimmed.starts_with("for "),
		trimmed.starts_with("if "),
		trimmed.starts_with("while "),
		trimmed.starts_with("def "),
		trimmed.starts_with("class "),
		trimmed.starts_with("import "),
		trimmed.starts_with("from "),
		trimmed.starts_with("return "),
	]
	.into_iter()
	.filter(|flag| *flag)
	.count();
	(keyword_score > 0 || punctuation_score >= 2)
		&& !trimmed.contains("://")
		&& !trimmed.contains('，')
}

fn extract_dollar_prefixed_command(goal: &str) -> Option<String> {
	goal.lines()
		.map(str::trim)
		.find_map(|line| line.strip_prefix("$ ").and_then(normalize_shell_command))
}

fn extract_command_suffix_after_separator(goal: &str) -> Option<String> {
	goal.match_indices([':', '：'])
		.filter_map(|(index, _)| {
			let prefix = goal.get(..index)?.trim().to_ascii_lowercase();
			let suffix = goal.get(index + 1..)?.trim();
			(prefix.contains("command")
				|| prefix.contains("cmd")
				|| prefix.contains("shell")
				|| prefix.contains("bash")
				|| prefix.ends_with("run this")
				|| prefix.ends_with("execute this"))
			.then_some(suffix)
			.and_then(normalize_shell_command)
		})
		.next_back()
}

fn normalize_shell_command(value: &str) -> Option<String> {
	let trimmed = value.trim().trim_matches('`').trim();
	if trimmed.is_empty() {
		return None;
	}
	let lines = trimmed
		.lines()
		.map(str::trim)
		.filter(|line| !line.is_empty())
		.collect::<Vec<_>>();
	if lines.len() != 1 {
		return None;
	}
	let line = lines[0].strip_prefix("$ ").unwrap_or(lines[0]).trim();
	is_probable_shell_command(line).then(|| line.to_string())
}

fn is_probable_shell_command(value: &str) -> bool {
	let trimmed = value.trim();
	if trimmed.is_empty() || trimmed.contains('\n') || trimmed.contains('\r') {
		return false;
	}
	let Some(first_token) = trimmed.split_whitespace().next() else {
		return false;
	};
	first_token
		.chars()
		.all(|character| character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-'))
}

// EPIC-5: migrate to descriptor-driven grounding — these helpers are intentionally
// minimal and temporary, used only by hardcoded match arms in tool_loop.rs.

/// Extracts a grep-like search pattern from the goal text.
///
/// Looks for patterns inside backticks, quotes, or after common grep-like markers.
pub(crate) fn extract_grep_pattern(goal: &str) -> Option<String> {
	// Try backtick-quoted pattern first
	if let Some(pattern) = extract_backtick_content(goal)
		&& !pattern.is_empty()
	{
		return Some(pattern);
	}
	// Try double-quoted pattern
	if let Some(start) = goal.find('"')
		&& let Some(end) = goal[start + 1..].find('"')
	{
		let candidate = &goal[start + 1..start + 1 + end];
		if !candidate.is_empty() {
			return Some(candidate.to_string());
		}
	}
	// Try single-quoted pattern
	if let Some(start) = goal.find('\'')
		&& let Some(end) = goal[start + 1..].find('\'')
	{
		let candidate = &goal[start + 1..start + 1 + end];
		if !candidate.is_empty() {
			return Some(candidate.to_string());
		}
	}
	None
}

/// Extracts a URL from the goal text for web.fetch grounding.
pub(crate) fn extract_fetch_url(goal: &str) -> Option<String> {
	goal.split_whitespace().find_map(|token| {
		let cleaned = token.trim_matches(|c: char| {
			matches!(
				c,
				'(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | '"' | '\'' | ',' | ';'
			)
		});
		if cleaned.starts_with("http://") || cleaned.starts_with("https://") {
			Some(cleaned.to_string())
		} else {
			None
		}
	})
}

fn extract_backtick_content(text: &str) -> Option<String> {
	let start = text.find('`')?;
	let rest = &text[start + 1..];
	let end = rest.find('`')?;
	let content = rest[..end].trim();
	(!content.is_empty()).then(|| content.to_string())
}

#[cfg(test)]
mod tests {
	use super::{extract_explicit_path_candidates, extract_fetch_url, extract_grep_pattern};

	#[test]
	fn extract_explicit_path_candidates_preserves_home_directory_paths() {
		let paths = extract_explicit_path_candidates("帮我 ll ~/ 看看有啥");
		assert_eq!(paths, vec!["~/".to_string()]);
	}

	#[test]
	fn extract_grep_pattern_from_backticks() {
		assert_eq!(
			extract_grep_pattern("Find all `TODO` comments in the code"),
			Some("TODO".to_string())
		);
	}

	#[test]
	fn extract_grep_pattern_from_double_quotes() {
		assert_eq!(
			extract_grep_pattern("Search for \"fn main\" in Rust files"),
			Some("fn main".to_string())
		);
	}

	#[test]
	fn extract_grep_pattern_returns_none_for_no_pattern() {
		assert_eq!(extract_grep_pattern("list all files"), None);
	}

	#[test]
	fn extract_fetch_url_from_https() {
		assert_eq!(
			extract_fetch_url("Read the docs at https://serde.rs/derive.html please"),
			Some("https://serde.rs/derive.html".to_string())
		);
	}

	#[test]
	fn extract_fetch_url_from_http() {
		assert_eq!(
			extract_fetch_url("Fetch http://example.com/api"),
			Some("http://example.com/api".to_string())
		);
	}

	#[test]
	fn extract_fetch_url_returns_none_for_no_url() {
		assert_eq!(extract_fetch_url("search the web for Rust docs"), None);
	}
}
