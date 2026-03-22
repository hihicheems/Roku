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

//! Shared read-only execution preview projection.
//!
//! The preview model is derived from canonical execution truth, but it must
//! never become the source of truth for approval, execution, or resume.

use serde::{Deserialize, Serialize};

use crate::{CanonicalDigest, CanonicalExecution, InvocationMode};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionPreview {
	pub tool_name: String,
	pub digest: CanonicalDigest,
	pub summary: String,
	pub command_text: String,
	pub working_directory: String,
}

pub fn project_execution_preview(execution: &CanonicalExecution) -> Option<ExecutionPreview> {
	if execution.tool_name != "command.run" {
		return None;
	}
	if execution.invocation_mode != InvocationMode::DirectExec || execution.shell_context.is_some()
	{
		return None;
	}

	let command_text = render_command_preview(&execution.argv);
	Some(ExecutionPreview {
		tool_name: execution.tool_name.clone(),
		digest: execution.digest.clone(),
		summary: format!("Run command {} from {}", command_text, execution.cwd),
		command_text,
		working_directory: execution.cwd.clone(),
	})
}

fn render_command_preview(argv: &[String]) -> String {
	argv.iter()
		.map(|argument| shell_quote(argument))
		.collect::<Vec<_>>()
		.join(" ")
}

fn shell_quote(value: &str) -> String {
	if value.is_empty() {
		return "''".to_string();
	}
	if value.chars().all(|character| {
		character.is_ascii_alphanumeric() || matches!(character, '.' | '/' | '-' | '_' | ':')
	}) {
		value.to_string()
	} else {
		format!("'{}'", value.replace('\'', "'\"'\"'"))
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{
		CanonicalExecution, ExecutionActionClass, ExecutionEnvPolicy, ExecutionEnvPolicyMode,
		ExecutionResourceScope,
	};

	fn sample_command_execution(argv: &[&str]) -> CanonicalExecution {
		CanonicalExecution {
			tool_name: "command.run".to_string(),
			program: argv[0].to_string(),
			argv: argv.iter().map(|value| value.to_string()).collect(),
			invocation_mode: InvocationMode::DirectExec,
			shell_context: None,
			cwd: "/workspace".to_string(),
			env_policy: ExecutionEnvPolicy {
				mode: ExecutionEnvPolicyMode::InheritSelected,
				allowed_keys: vec!["ROKU_COMMAND_SCOPE_ROOT".to_string()],
			},
			resource_scope: ExecutionResourceScope {
				working_directory: "/workspace".to_string(),
				resolved_targets: Vec::new(),
				effective_read_roots: vec!["/workspace".to_string()],
				effective_write_roots: Vec::new(),
			},
			action_class: ExecutionActionClass::Exec,
			digest: CanonicalDigest("digest-123".to_string()),
		}
	}

	#[test]
	fn projects_command_run_preview_from_canonical_execution() {
		let preview = project_execution_preview(&sample_command_execution(&["pwd"]))
			.expect("command.run preview should project");

		assert_eq!(preview.command_text, "pwd");
		assert_eq!(preview.summary, "Run command pwd from /workspace");
		assert_eq!(preview.digest, CanonicalDigest("digest-123".to_string()));
	}

	#[test]
	fn quotes_preview_command_arguments_consistently() {
		let preview =
			project_execution_preview(&sample_command_execution(&["printf", "hello world"]))
				.expect("command.run preview should project");

		assert_eq!(preview.command_text, "printf 'hello world'");
		assert_eq!(
			preview.summary,
			"Run command printf 'hello world' from /workspace"
		);
	}

	#[test]
	fn skips_non_command_preview_projection() {
		let mut execution = sample_command_execution(&["pwd"]);
		execution.tool_name = "python.run".to_string();

		assert!(project_execution_preview(&execution).is_none());
	}
}
