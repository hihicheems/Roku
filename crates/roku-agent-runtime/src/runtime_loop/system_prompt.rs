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

use roku_plugin_tools::{
	PSEUDO_ASK_USER, PSEUDO_FAIL, PSEUDO_FINAL_ANSWER, TOOL_BASH, TOOL_GLOB, TOOL_LISTDIR,
	TOOL_READ, TOOL_WEB_FETCH, TOOL_WEB_SEARCH,
};

use roku_common_types::RuntimeMemorySections;
use roku_plugin_llm::{SystemPromptBlock, SystemPromptSections};

use super::environment::{EnvironmentSnapshot, format_environment_context};

/// Build the full system prompt, flattened into a single string.
///
/// Convenience wrapper kept for callers that do not need the structured
/// block form. Internally constructs [`SystemPromptSections`] and flattens
/// via [`SystemPromptSections::flatten`]; the resulting byte sequence places
/// every static block before every dynamic block (static prefix + dynamic
/// suffix). All existing content-presence assertions continue to hold; only
/// the intra-string section order differs from the pre-split implementation.
///
/// `working_directory` is per-step (may change after `cd`).
/// `env_snapshot` is per-process (CLI tools, git context).
/// `project_instruction` is loaded from `.roku.md` / `~/.roku/ROKU.md`.
#[cfg_attr(not(test), allow(dead_code))]
pub fn build_system_prompt(
	env_snapshot: &EnvironmentSnapshot,
	working_directory: &str,
	project_instruction: Option<&str>,
	memory_sections: Option<&RuntimeMemorySections>,
	plan_mode: bool,
) -> String {
	build_system_prompt_sections(
		env_snapshot,
		working_directory,
		project_instruction,
		memory_sections,
		plan_mode,
	)
	.flatten()
}

/// Build the system prompt as structured static / dynamic blocks.
///
/// **Static blocks** hold session-stable content (identity, tool guidance,
/// project instruction). Their serialized bytes must remain byte-identical
/// across `cd`, timestamp, and other per-turn runtime-variable changes in
/// the same session. Prompt-cache adapters consume this slice to build a
/// cache-stable prefix.
///
/// **Dynamic blocks** hold per-turn variable content (environment probe +
/// `working_directory`, memory, plan-mode). Their content may change turn to
/// turn. Adapters must never mark these as cacheable.
///
/// Block ordering within each group is stable (insertion order). The flatten
/// view emits every static block before every dynamic block.
pub fn build_system_prompt_sections(
	env_snapshot: &EnvironmentSnapshot,
	working_directory: &str,
	project_instruction: Option<&str>,
	memory_sections: Option<&RuntimeMemorySections>,
	plan_mode: bool,
) -> SystemPromptSections {
	let mut static_blocks = Vec::with_capacity(3);
	let mut dynamic_blocks = Vec::with_capacity(3);

	static_blocks.push(SystemPromptBlock {
		id: "identity".to_string(),
		content: identity_section(),
	});
	static_blocks.push(SystemPromptBlock {
		id: "tool_guidance".to_string(),
		content: tool_guidance_section(),
	});

	if let Some(instruction) = project_instruction {
		let trimmed = instruction.trim();
		if !trimmed.is_empty() {
			static_blocks.push(SystemPromptBlock {
				id: "project_instruction".to_string(),
				content: project_instruction_section(trimmed),
			});
		}
	}

	dynamic_blocks.push(SystemPromptBlock {
		id: "environment".to_string(),
		content: environment_section(env_snapshot, working_directory),
	});

	if let Some(memory) = memory_sections.filter(|m| !m.is_empty()) {
		dynamic_blocks.push(SystemPromptBlock {
			id: "memory".to_string(),
			content: memory_section(memory),
		});
	}

	if plan_mode {
		dynamic_blocks.push(SystemPromptBlock {
			id: "plan_mode".to_string(),
			content: plan_mode_section(),
		});
	}

	SystemPromptSections {
		static_blocks,
		dynamic_blocks,
	}
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
	format!(
		"\
# Tool Usage

You have access to tools. Use them proactively to accomplish the user's task.

## Core rules

- Act immediately. Do not ask for permission before using tools unless the action \
is destructive or irreversible.
- Do NOT answer from training data when you can use tools to get current, accurate \
information. Prefer tool results over memorized knowledge.
- Use the most specific tool available. Fall back to {TOOL_BASH} only when no \
dedicated tool fits.
- When the user provides a URL, fetch it with {TOOL_WEB_FETCH}.
- When the user asks about a file, read it with {TOOL_READ}.
- When the user asks about a repository or project, explore it with {TOOL_LISTDIR}, \
{TOOL_GLOB}, {TOOL_READ}, or {TOOL_BASH} (e.g. `gh repo view`).
- When the user asks you to run a command, use {TOOL_BASH}.
- When the user asks a question that requires current information (weather, news, \
docs, package versions), use {TOOL_WEB_SEARCH} or {TOOL_WEB_FETCH}.
- Never say \"Let me check...\" or \"I'll look into that...\" — just do it.

## Completing the task

- Call {PSEUDO_FINAL_ANSWER} when the task is complete and you have a response for the user.
- Call {PSEUDO_ASK_USER} when you need clarification before you can proceed.
- Call {PSEUDO_FAIL} only when the task is genuinely impossible after attempting it.

## Output style

- Be concise. Lead with the answer, not the reasoning.
- Use code blocks with language tags for code.
- When the user writes in Chinese, respond in Chinese. Match the user's language.\
"
	)
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

/// Plan mode constraint: injected when the loop is in Plan mode.
fn plan_mode_section() -> String {
	"\
# Plan Mode (Active)

You are in **read-only planning mode**. Follow these rules strictly:

1. **DO NOT** write, edit, create, or delete any files.
2. **DO NOT** execute commands that modify state (e.g. `git commit`, `rm`, write operations).
3. **DO** read files, search code, explore the codebase, and gather information.
4. **DO** produce a structured execution plan with clear steps.

Your output should be a plan that describes:
- What changes are needed and why
- Which files need to be modified
- The order of operations
- Any risks or considerations

The user will review your plan and decide whether to execute it.\
"
	.to_string()
}

/// Maximum character count for injected memory content.
/// Prevents unbounded context growth from very large memory stores.
const MAX_MEMORY_CHARS: usize = 40_000;

/// Memory context: short-term continuity, long-term recall, working memory.
fn memory_section(memory: &RuntimeMemorySections) -> String {
	let text = memory.named_sections_text();
	let capped = if text.chars().count() > MAX_MEMORY_CHARS {
		let truncated: String = text.chars().take(MAX_MEMORY_CHARS).collect();
		format!("{truncated}\n\n[Note: memory content truncated at {MAX_MEMORY_CHARS} chars]")
	} else {
		text
	};
	format!("# Memory\n\n{capped}")
}

/// Maximum byte size for a single instruction file. Files exceeding this limit
/// are truncated to avoid unbounded context growth.
const MAX_INSTRUCTION_BYTES: usize = 32 * 1024; // 32KB

/// Truncate `content` at a UTF-8 character boundary not exceeding
/// `MAX_INSTRUCTION_BYTES` bytes, appending a note when truncation occurs.
fn cap_instruction_content(content: &str) -> String {
	if content.len() <= MAX_INSTRUCTION_BYTES {
		return content.to_string();
	}
	// Find the last valid char boundary within the byte limit.
	let truncated = &content[..content
		.char_indices()
		.take_while(|(i, _)| *i < MAX_INSTRUCTION_BYTES)
		.last()
		.map(|(i, c)| i + c.len_utf8())
		.unwrap_or(MAX_INSTRUCTION_BYTES)];
	format!("{truncated}\n\n[Note: instruction file truncated at 32KB]")
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
				parts.push(cap_instruction_content(trimmed));
			}
		}
	}

	// Project: {working_directory}/.roku.md
	let project_path = std::path::Path::new(working_directory).join(".roku.md");
	if let Ok(content) = std::fs::read_to_string(&project_path) {
		let trimmed = content.trim();
		if !trimmed.is_empty() {
			parts.push(cap_instruction_content(trimmed));
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
		let prompt = build_system_prompt(&sample_env(), "/home/user/project", None, None, false);
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
			None,
			false,
		);
		assert!(prompt.contains("# Project Instructions"));
		assert!(prompt.contains("Always use Rust"));
	}

	#[test]
	fn system_prompt_skips_empty_project_instruction() {
		let prompt = build_system_prompt(
			&sample_env(),
			"/home/user/project",
			Some("   "),
			None,
			false,
		);
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
		assert!(section.contains("WebFetch"));
		assert!(section.contains("Read"));
		assert!(section.contains("Bash"));
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
	fn large_instruction_file_is_truncated() {
		let dir = tempfile::tempdir().expect("tempdir");
		let roku_md = dir.path().join(".roku.md");
		// Write a file clearly exceeding 32KB.
		let large_content = "x".repeat(MAX_INSTRUCTION_BYTES + 1024);
		std::fs::write(&roku_md, &large_content).expect("write");
		let result =
			load_project_instructions(dir.path().to_str().unwrap()).expect("should return Some");
		assert!(
			result.len() < large_content.len(),
			"truncated result should be shorter than original"
		);
		assert!(
			result.contains("[Note: instruction file truncated at 32KB]"),
			"truncated result should contain the truncation note"
		);
	}

	#[test]
	fn small_instruction_file_is_not_truncated() {
		let dir = tempfile::tempdir().expect("tempdir");
		let roku_md = dir.path().join(".roku.md");
		let small_content = "Small content.";
		std::fs::write(&roku_md, small_content).expect("write");
		let result =
			load_project_instructions(dir.path().to_str().unwrap()).expect("should return Some");
		assert_eq!(result, small_content);
	}

	#[test]
	fn prompt_token_estimate_within_budget() {
		// Rough token estimate: ~4 chars per token for English text.
		let prompt = build_system_prompt(&sample_env(), "/home/user/project", None, None, false);
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

	#[test]
	fn system_prompt_includes_memory_when_non_empty() {
		let memory = RuntimeMemorySections {
			short_term_continuity: String::new(),
			long_term_recall: "- rec1 | fact | user prefers dark mode".to_string(),
			working_memory: String::new(),
		};
		let prompt = build_system_prompt(
			&sample_env(),
			"/home/user/project",
			None,
			Some(&memory),
			false,
		);
		assert!(prompt.contains("# Memory"));
		assert!(prompt.contains("Long-term recall:"));
		assert!(prompt.contains("user prefers dark mode"));
	}

	#[test]
	fn system_prompt_excludes_memory_when_empty() {
		let memory = RuntimeMemorySections::default();
		let prompt = build_system_prompt(
			&sample_env(),
			"/home/user/project",
			None,
			Some(&memory),
			false,
		);
		assert!(!prompt.contains("# Memory"));
	}

	#[test]
	fn system_prompt_excludes_memory_when_none() {
		let prompt = build_system_prompt(&sample_env(), "/home/user/project", None, None, false);
		assert!(!prompt.contains("# Memory"));
	}

	#[test]
	fn memory_section_is_truncated_when_large() {
		let memory = RuntimeMemorySections {
			short_term_continuity: String::new(),
			long_term_recall: "x".repeat(MAX_MEMORY_CHARS + 1000),
			working_memory: String::new(),
		};
		let section = memory_section(&memory);
		assert!(section.contains("[Note: memory content truncated"));
	}

	#[test]
	fn system_prompt_includes_plan_mode_when_active() {
		let prompt = build_system_prompt(&sample_env(), "/home/user/project", None, None, true);
		assert!(prompt.contains("# Plan Mode (Active)"));
		assert!(prompt.contains("read-only planning mode"));
		assert!(prompt.contains("DO NOT"));
	}

	#[test]
	fn system_prompt_excludes_plan_mode_when_inactive() {
		let prompt = build_system_prompt(&sample_env(), "/home/user/project", None, None, false);
		assert!(!prompt.contains("# Plan Mode"));
	}

	fn concat_contents(blocks: &[SystemPromptBlock]) -> String {
		blocks
			.iter()
			.map(|b| b.content.as_str())
			.collect::<Vec<_>>()
			.join("\n\n")
	}

	#[test]
	fn system_prompt_static_segment_stable_across_cd() {
		// Core cache-stability invariant: the static prefix bytes must be
		// byte-identical across working-directory changes in the same session.
		// This is the §9.3 DYNAMIC_BOUNDARY gate for system prompt.
		let env = sample_env();
		let sections_a =
			build_system_prompt_sections(&env, "/home/user/project-a", None, None, false);
		let sections_b =
			build_system_prompt_sections(&env, "/home/user/project-b", None, None, false);

		assert_eq!(
			concat_contents(&sections_a.static_blocks),
			concat_contents(&sections_b.static_blocks),
			"static blocks must not shift when working_directory changes"
		);
	}

	#[test]
	fn system_prompt_static_segment_stable_with_project_instruction() {
		// Project instruction is loaded once per session; the static prefix
		// stays stable across `cd` even when the instruction is present.
		let env = sample_env();
		let instr = Some("Always use Rust. Never use JavaScript.");
		let sections_a = build_system_prompt_sections(&env, "/a", instr, None, false);
		let sections_b = build_system_prompt_sections(&env, "/b", instr, None, false);

		assert_eq!(
			concat_contents(&sections_a.static_blocks),
			concat_contents(&sections_b.static_blocks)
		);
	}

	#[test]
	fn system_prompt_dynamic_segment_moves_across_cd() {
		// Negative check: the dynamic segment actually does move when
		// working_directory changes — otherwise the split is meaningless.
		let env = sample_env();
		let sections_a = build_system_prompt_sections(&env, "/dir-a", None, None, false);
		let sections_b = build_system_prompt_sections(&env, "/dir-b", None, None, false);

		assert_ne!(
			concat_contents(&sections_a.dynamic_blocks),
			concat_contents(&sections_b.dynamic_blocks),
			"dynamic blocks must reflect working_directory changes"
		);
	}

	#[test]
	fn working_directory_lives_in_dynamic_segment_only() {
		// L3 hard constraint: working_directory must never appear in a
		// static block — it would break the cache-stable prefix.
		let env = sample_env();
		let wd = "/unique-marker-dir-for-test";
		let sections = build_system_prompt_sections(&env, wd, None, None, false);

		for block in &sections.static_blocks {
			assert!(
				!block.content.contains(wd),
				"static block {} contains working_directory",
				block.id.as_str()
			);
		}
		let dynamic_joined = concat_contents(&sections.dynamic_blocks);
		assert!(
			dynamic_joined.contains(wd),
			"dynamic segment must contain working_directory"
		);
	}

	#[test]
	fn block_ids_are_distinct_and_stable() {
		// Cache-marker placement depends on block ids being distinct and
		// predictable; enforce both.
		let env = sample_env();
		let memory = RuntimeMemorySections {
			short_term_continuity: String::new(),
			long_term_recall: "- rec1 | fact | user likes dark mode".to_string(),
			working_memory: String::new(),
		};
		let sections = build_system_prompt_sections(
			&env,
			"/home/user/project",
			Some("Rust only."),
			Some(&memory),
			true,
		);

		let static_ids: Vec<&str> = sections
			.static_blocks
			.iter()
			.map(|b| b.id.as_str())
			.collect();
		let dynamic_ids: Vec<&str> = sections
			.dynamic_blocks
			.iter()
			.map(|b| b.id.as_str())
			.collect();

		assert_eq!(
			static_ids,
			vec!["identity", "tool_guidance", "project_instruction"]
		);
		assert_eq!(dynamic_ids, vec!["environment", "memory", "plan_mode"]);
	}

	#[test]
	fn flatten_preserves_content_presence_for_legacy_callers() {
		// Existing callers that just want a single `String` should still see
		// every expected header; only the intra-string order changes.
		let env = sample_env();
		let memory = RuntimeMemorySections {
			short_term_continuity: "- a | recent | something".to_string(),
			long_term_recall: String::new(),
			working_memory: String::new(),
		};
		let prompt = build_system_prompt(
			&env,
			"/home/user/project",
			Some("Use Rust."),
			Some(&memory),
			true,
		);

		for header in [
			"# Identity",
			"# Tool Usage",
			"# Project Instructions",
			"# Environment",
			"# Memory",
			"# Plan Mode (Active)",
		] {
			assert!(prompt.contains(header), "missing expected header {header}");
		}
	}
}
