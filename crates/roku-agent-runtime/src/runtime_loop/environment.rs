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

//! Startup environment probing for the runtime loop.
//!
//! Detects available CLI tools and git repository context once per process,
//! then provides a compact text summary that is injected into the LLM prompt
//! so the model knows what shell tools are available and which repository it
//! is operating in.

use std::process::Command;
use std::sync::OnceLock;

/// Cached environment snapshot, computed once per process on first access.
static ENVIRONMENT_SNAPSHOT: OnceLock<EnvironmentSnapshot> = OnceLock::new();

/// Probed environment information available for prompt injection.
#[derive(Debug, Clone)]
pub(crate) struct EnvironmentSnapshot {
	/// CLI tools confirmed available via `which`.
	pub available_tools: Vec<String>,
	/// Git repository context (if inside a git repo).
	pub git_context: Option<GitContext>,
}

/// Git repository metadata for the current working directory.
#[derive(Debug, Clone)]
pub(crate) struct GitContext {
	pub branch: String,
	pub remote_url: String,
	pub repo_name: String,
}

/// Probe the environment and cache the result. Returns the cached snapshot on
/// subsequent calls.
pub(crate) fn probe_environment() -> &'static EnvironmentSnapshot {
	ENVIRONMENT_SNAPSHOT.get_or_init(|| EnvironmentSnapshot {
		available_tools: probe_cli_tools(),
		git_context: probe_git_context(),
	})
}

/// Format the environment snapshot as a compact text section for prompt injection.
///
/// `working_directory` is passed explicitly because it may change per step
/// (via `LoopState`), unlike CLI tools and git context which are static per
/// process.
pub(crate) fn format_environment_context(
	snapshot: &EnvironmentSnapshot,
	working_directory: &str,
) -> String {
	let mut sections = Vec::new();

	sections.push(format!(
		"Working directory: {}",
		sanitize_prompt_value(working_directory)
	));

	if let Some(git) = &snapshot.git_context {
		sections.push(format!(
			"Git repository: {} (branch: {}, remote: {})",
			sanitize_prompt_value(&git.repo_name),
			sanitize_prompt_value(&git.branch),
			sanitize_prompt_value(&git.remote_url),
		));
	}

	if !snapshot.available_tools.is_empty() {
		sections.push(format!(
			"Available CLI tools: {}",
			snapshot.available_tools.join(", ")
		));
	}

	sections.join("\n")
}

/// Strip control characters and truncate to prevent prompt injection via
/// crafted directory names, git remotes, or branch names.
fn sanitize_prompt_value(value: &str) -> String {
	const MAX_LEN: usize = 256;
	let sanitized: String = value
		.chars()
		.filter(|c| !c.is_control() || *c == ' ')
		.take(MAX_LEN)
		.collect();
	sanitized
}

/// Check which CLI tools are available via `which`.
fn probe_cli_tools() -> Vec<String> {
	const TOOLS: &[&str] = &[
		"gh", "git", "node", "python3", "curl", "jq", "docker", "kubectl", "cargo", "make", "just",
		"rg",
	];

	TOOLS
		.iter()
		.filter(|tool| {
			Command::new("which")
				.arg(tool)
				.stdout(std::process::Stdio::null())
				.stderr(std::process::Stdio::null())
				.status()
				.map(|status| status.success())
				.unwrap_or(false)
		})
		.map(|tool| (*tool).to_string())
		.collect()
}

/// Read git repository context from the current working directory.
fn probe_git_context() -> Option<GitContext> {
	let branch = Command::new("git")
		.args(["rev-parse", "--abbrev-ref", "HEAD"])
		.output()
		.ok()
		.filter(|output| output.status.success())
		.map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())?;

	let remote_url = Command::new("git")
		.args(["remote", "get-url", "origin"])
		.output()
		.ok()
		.filter(|output| output.status.success())
		.map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
		.unwrap_or_default();

	let repo_name = extract_repo_name(&remote_url);

	Some(GitContext {
		branch,
		remote_url,
		repo_name,
	})
}

/// Extract a human-readable repository name from a git remote URL.
///
/// Handles both SSH (`git@github.com:owner/repo.git`) and HTTPS
/// (`https://github.com/owner/repo.git`) formats.
fn extract_repo_name(remote_url: &str) -> String {
	let path = if let Some(rest) = remote_url.strip_prefix("git@") {
		// SSH: git@github.com:owner/repo.git
		rest.split_once(':').map(|(_, path)| path)
	} else {
		// HTTPS: https://github.com/owner/repo.git
		remote_url
			.strip_prefix("https://")
			.or_else(|| remote_url.strip_prefix("http://"))
			.and_then(|rest| rest.split_once('/').map(|(_, path)| path))
	};

	path.unwrap_or(remote_url)
		.trim_end_matches(".git")
		.to_string()
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn extract_repo_name_from_ssh_url() {
		assert_eq!(
			extract_repo_name("git@github.com:itscheems/Roku.git"),
			"itscheems/Roku"
		);
	}

	#[test]
	fn extract_repo_name_from_https_url() {
		assert_eq!(
			extract_repo_name("https://github.com/itscheems/Roku.git"),
			"itscheems/Roku"
		);
	}

	#[test]
	fn extract_repo_name_from_bare_path() {
		assert_eq!(extract_repo_name("itscheems/Roku"), "itscheems/Roku");
	}

	#[test]
	fn probe_environment_returns_consistent_snapshot() {
		let snapshot_a = probe_environment();
		let snapshot_b = probe_environment();
		// OnceLock guarantees same reference.
		assert!(std::ptr::eq(snapshot_a, snapshot_b));
	}

	#[test]
	fn format_environment_context_renders_tools_and_git() {
		let snapshot = EnvironmentSnapshot {
			available_tools: vec!["git".to_string(), "gh".to_string()],
			git_context: Some(GitContext {
				branch: "main".to_string(),
				remote_url: "https://github.com/itscheems/Roku.git".to_string(),
				repo_name: "itscheems/Roku".to_string(),
			}),
		};
		let context = format_environment_context(&snapshot, "/home/user/project");
		assert!(context.contains("Working directory: /home/user/project"));
		assert!(context.contains("Available CLI tools: git, gh"));
		assert!(context.contains("Git repository: itscheems/Roku"));
		assert!(context.contains("branch: main"));
	}

	#[test]
	fn sanitize_strips_control_characters() {
		assert_eq!(sanitize_prompt_value("normal/path"), "normal/path");
		assert_eq!(
			sanitize_prompt_value("path\nwith\nnewlines"),
			"pathwithnewlines"
		);
		assert_eq!(sanitize_prompt_value("has\x00null\x1besc"), "hasnullesc");
	}

	#[test]
	fn sanitize_truncates_long_values() {
		let long = "a".repeat(500);
		assert_eq!(sanitize_prompt_value(&long).len(), 256);
	}
}
