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

//! Modular system prompt builder.
//!
//! Composes the system prompt from independent fragments:
//! identity, tool guidance, environment context, and project instructions.
//! Each fragment is a pure function returning a string section.

use super::environment::{EnvironmentSnapshot, format_environment_context};

/// Build the full system prompt from modular fragments.
///
/// `working_directory` is per-step (may change after `cd`).
/// `env_snapshot` is per-process (CLI tools, git context).
/// `project_instruction` is loaded from `.roku.md` / `~/.roku/ROKU.md`.
pub fn build_system_prompt(
	env_snapshot: &EnvironmentSnapshot,
	working_directory: &str,
	project_instruction: Option<&str>,
) -> String {
	let mut sections = Vec::with_capacity(4);

	sections.push(identity_section());
	sections.push(tool_guidance_section());
	sections.push(environment_section(env_snapshot, working_directory));

	if let Some(instruction) = project_instruction {
		let trimmed = instruction.trim();
		if !trimmed.is_empty() {
			sections.push(project_instruction_section(trimmed));
		}
	}

	sections.join("\n\n")
}

/// Identity: who Roku is and how it should behave.
fn identity_section() -> String {
	"\
# Identity

You are Roku, an AI coding assistant. You help users with software engineering \
tasks: writing code, debugging, reading files, running commands, searching the web, \
and explaining code. You are direct, concise, and action-oriented.\
"
	.to_string()
}

/// Tool guidance: when and how to use tools.
fn tool_guidance_section() -> String {
	"\
# Tool Usage

You have access to tools. Use them proactively to accomplish the user's task.

## Core rules

- Act immediately. Do not ask for permission before using tools unless the action \
is destructive or irreversible.
- Do NOT answer from training data when you can use tools to get current, accurate \
information. Prefer tool results over memorized knowledge.
- Use the most specific tool available. Fall back to command.run only when no \
dedicated tool fits.
- When the user provides a URL, fetch it with web.fetch.
- When the user asks about a file, read it with fs.read_text.
- When the user asks about a repository or project, explore it with fs.list_dir, \
fs.glob, fs.read_text, or command.run (e.g. `gh repo view`).
- When the user asks you to run a command, use command.run.
- When the user asks a question that requires current information (weather, news, \
docs, package versions), use web.search or web.fetch.
- Never say \"Let me check...\" or \"I'll look into that...\" — just do it.

## Completing the task

- Call final_answer when the task is complete and you have a response for the user.
- Call ask_user when you need clarification before you can proceed.
- Call fail only when the task is genuinely impossible after attempting it.

## Output style

- Be concise. Lead with the answer, not the reasoning.
- Use code blocks with language tags for code.
- When the user writes in Chinese, respond in Chinese. Match the user's language.\
"
	.to_string()
}

/// Environment context: working directory, git info, available CLI tools.
fn environment_section(snapshot: &EnvironmentSnapshot, working_directory: &str) -> String {
	let env_context = format_environment_context(snapshot, working_directory);
	format!("# Environment\n{env_context}")
}

/// Project instruction: content from .roku.md and/or ~/.roku/ROKU.md.
fn project_instruction_section(content: &str) -> String {
	format!("# Project Instructions\n\n{content}")
}

/// Load project instruction content from `.roku.md` (project-level) and
/// `~/.roku/ROKU.md` (global-level). Project-level takes precedence (appended
/// after global, so it overrides in case of conflict).
pub fn load_project_instructions(working_directory: &str) -> Option<String> {
	let mut parts = Vec::new();

	// Global: ~/.roku/ROKU.md
	if let Ok(home) = std::env::var("HOME") {
		let global_path = std::path::Path::new(&home).join(".roku").join("ROKU.md");
		if let Ok(content) = std::fs::read_to_string(&global_path) {
			let trimmed = content.trim();
			if !trimmed.is_empty() {
				parts.push(trimmed.to_string());
			}
		}
	}

	// Project: {working_directory}/.roku.md
	let project_path = std::path::Path::new(working_directory).join(".roku.md");
	if let Ok(content) = std::fs::read_to_string(&project_path) {
		let trimmed = content.trim();
		if !trimmed.is_empty() {
			parts.push(trimmed.to_string());
		}
	}

	if parts.is_empty() {
		None
	} else {
		Some(parts.join("\n\n---\n\n"))
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::runtime_loop::environment::GitContext;

	fn sample_env() -> EnvironmentSnapshot {
		EnvironmentSnapshot {
			available_tools: vec!["git".into(), "gh".into(), "cargo".into()],
			git_context: Some(GitContext {
				branch: "main".into(),
				remote_url: "https://github.com/user/repo.git".into(),
				repo_name: "user/repo".into(),
			}),
		}
	}

	#[test]
	fn system_prompt_contains_all_sections_without_project_instruction() {
		let prompt = build_system_prompt(&sample_env(), "/home/user/project", None);
		assert!(prompt.contains("# Identity"));
		assert!(prompt.contains("# Tool Usage"));
		assert!(prompt.contains("# Environment"));
		assert!(!prompt.contains("# Project Instructions"));
	}

	#[test]
	fn system_prompt_includes_project_instruction_when_present() {
		let prompt = build_system_prompt(
			&sample_env(),
			"/home/user/project",
			Some("Always use Rust. Never use JavaScript."),
		);
		assert!(prompt.contains("# Project Instructions"));
		assert!(prompt.contains("Always use Rust"));
	}

	#[test]
	fn system_prompt_skips_empty_project_instruction() {
		let prompt = build_system_prompt(&sample_env(), "/home/user/project", Some("   "));
		assert!(!prompt.contains("# Project Instructions"));
	}

	#[test]
	fn identity_section_mentions_roku() {
		let section = identity_section();
		assert!(section.contains("Roku"));
		assert!(section.contains("coding assistant"));
	}

	#[test]
	fn tool_guidance_mentions_key_rules() {
		let section = tool_guidance_section();
		assert!(section.contains("web.fetch"));
		assert!(section.contains("fs.read_text"));
		assert!(section.contains("command.run"));
		assert!(section.contains("final_answer"));
		assert!(section.contains("training data"));
	}

	#[test]
	fn environment_section_includes_working_directory() {
		let section = environment_section(&sample_env(), "/workspace");
		assert!(section.contains("/workspace"));
		assert!(section.contains("git"));
	}

	#[test]
	fn load_project_instructions_returns_none_for_nonexistent() {
		let result = load_project_instructions("/tmp/nonexistent-roku-test-dir-xyz");
		assert!(result.is_none());
	}

	#[test]
	fn load_project_instructions_reads_project_file() {
		let dir = tempfile::tempdir().expect("tempdir");
		let roku_md = dir.path().join(".roku.md");
		std::fs::write(&roku_md, "Use tabs not spaces.").expect("write");
		let result = load_project_instructions(dir.path().to_str().unwrap());
		assert_eq!(result.as_deref(), Some("Use tabs not spaces."));
	}

	#[test]
	fn prompt_token_estimate_within_budget() {
		// Rough token estimate: ~4 chars per token for English text.
		let prompt = build_system_prompt(&sample_env(), "/home/user/project", None);
		let estimated_tokens = prompt.len() / 4;
		// Target: ~1.5-2K tokens without project instructions.
		assert!(
			estimated_tokens < 2500,
			"prompt is {estimated_tokens} estimated tokens, should be under 2500"
		);
		assert!(
			estimated_tokens > 300,
			"prompt is {estimated_tokens} estimated tokens, should be at least 300"
		);
	}
}
