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

use std::path::{Path, PathBuf};

use crate::runtime_config::{CommandToolRuntimeConfig, HARD_MAX_TIMEOUT_MS};
use roku_common_types::{
	CanonicalDigest, CanonicalExecution, ExecutionActionClass, ExecutionEnvPolicy,
	ExecutionEnvPolicyMode, ExecutionResourceScope, InvocationMode, ToolOutputEnvelope,
};
use roku_plugin_host::{ToolFailure, ToolInvocationRequest};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub(super) enum PrepareCommandOutcome {
	Ready(Box<PreparedCommand>),
	Rejected(Value),
}

pub(super) struct PreparedCommand {
	pub execution: CanonicalExecution,
	pub timeout_ms: u64,
}

pub(super) fn prepare_command(
	request: &ToolInvocationRequest,
	config: &CommandToolRuntimeConfig,
) -> Result<PrepareCommandOutcome, ToolFailure> {
	let command_text = request
		.input
		.get("command")
		.and_then(Value::as_str)
		.filter(|value| !value.trim().is_empty())
		.ok_or_else(|| ToolFailure::terminal("missing required field `command`"))?;
	let timeout_ms = request
		.input
		.get("timeout_ms")
		.and_then(Value::as_u64)
		.unwrap_or(config.default_timeout_ms)
		.min(config.default_timeout_ms)
		.min(HARD_MAX_TIMEOUT_MS);
	let working_directory = resolve_working_directory(
		request.input.get("cwd").and_then(Value::as_str),
		&request.allowed_read_roots,
	)
	.map_err(ToolFailure::terminal)?;
	let scope_root = request
		.allowed_read_roots
		.first()
		.cloned()
		.unwrap_or_else(|| working_directory.clone());

	if contains_forbidden_shell_syntax(command_text) {
		return Ok(PrepareCommandOutcome::Rejected(rejected_output(
			command_text,
			None,
			None,
			&working_directory,
			&scope_root,
			"unsafe_shell_syntax",
			"command.run only accepts one explicit command without shell metacharacters or pipelines.",
			true,
		)));
	}

	let Some(argv) = shlex::split(command_text) else {
		return Ok(PrepareCommandOutcome::Rejected(rejected_output(
			command_text,
			None,
			None,
			&working_directory,
			&scope_root,
			"invalid_command_syntax",
			"command.run could not parse the explicit command string.",
			true,
		)));
	};
	if argv.is_empty() {
		return Ok(PrepareCommandOutcome::Rejected(rejected_output(
			command_text,
			Some(&argv),
			None,
			&working_directory,
			&scope_root,
			"empty_command",
			"command.run requires a non-empty command.",
			true,
		)));
	}

	let program = argv[0].clone();
	let arguments = argv[1..].to_vec();
	let execution = build_canonical_execution(&argv, &working_directory, request)?;
	if let Err(message) = validate_allowed_command(&program, &arguments) {
		return Ok(PrepareCommandOutcome::Rejected(rejected_output(
			command_text,
			Some(&argv),
			Some(&execution),
			&working_directory,
			&scope_root,
			"command_not_allowed",
			&message,
			true,
		)));
	}
	if let Some(argument) =
		first_out_of_scope_argument(&arguments, &working_directory, &request.allowed_read_roots)
	{
		return Ok(PrepareCommandOutcome::Rejected(rejected_output(
			command_text,
			Some(&argv),
			Some(&execution),
			&working_directory,
			&scope_root,
			"path_out_of_scope",
			&format!("`{argument}` resolves outside the allowed workspace roots for command.run."),
			true,
		)));
	}

	Ok(PrepareCommandOutcome::Ready(
		PreparedCommand {
			execution,
			timeout_ms,
		}
		.into(),
	))
}

fn build_canonical_execution(
	argv: &[String],
	working_directory: &Path,
	request: &ToolInvocationRequest,
) -> Result<CanonicalExecution, ToolFailure> {
	let env_policy = ExecutionEnvPolicy {
		mode: ExecutionEnvPolicyMode::InheritSelected,
		allowed_keys: vec![
			"ROKU_COMMAND_SCOPE_ROOT".to_string(),
			"ROKU_ALLOWED_READ_ROOTS".to_string(),
		],
	};
	let resource_scope = ExecutionResourceScope {
		working_directory: working_directory.display().to_string(),
		resolved_targets: resolved_targets(&argv[1..], working_directory),
		effective_read_roots: path_strings(&request.allowed_read_roots),
		effective_write_roots: path_strings(&request.allowed_write_roots),
	};
	let action_class = ExecutionActionClass::Exec;
	let digest = compute_digest(
		"command.run",
		&argv[0],
		argv,
		working_directory,
		&env_policy,
		&resource_scope,
		action_class,
	)?;

	Ok(CanonicalExecution {
		tool_name: "command.run".to_string(),
		program: argv[0].clone(),
		argv: argv.to_vec(),
		invocation_mode: InvocationMode::DirectExec,
		shell_context: None,
		cwd: working_directory.display().to_string(),
		env_policy,
		resource_scope,
		action_class,
		digest,
	})
}

fn compute_digest(
	tool_name: &str,
	program: &str,
	argv: &[String],
	working_directory: &Path,
	env_policy: &ExecutionEnvPolicy,
	resource_scope: &ExecutionResourceScope,
	action_class: ExecutionActionClass,
) -> Result<CanonicalDigest, ToolFailure> {
	let payload = json!({
		"tool_name": tool_name,
		"program": program,
		"argv": argv,
		"invocation_mode": InvocationMode::DirectExec,
		"cwd": working_directory.display().to_string(),
		"env_policy": env_policy,
		"resource_scope": resource_scope,
		"action_class": action_class,
	});
	let bytes = serde_json::to_vec(&payload).map_err(|error| {
		ToolFailure::terminal(format!("failed to encode canonical digest input: {error}"))
	})?;
	let mut hasher = Sha256::new();
	hasher.update(bytes);
	let digest = hasher.finalize();
	Ok(CanonicalDigest(
		digest.iter().map(|b| format!("{b:02x}")).collect::<String>(),
	))
}

fn path_strings(paths: &[PathBuf]) -> Vec<String> {
	paths
		.iter()
		.map(|path| path.display().to_string())
		.collect()
}

fn resolved_targets(arguments: &[String], working_directory: &Path) -> Vec<String> {
	arguments
		.iter()
		.filter(|argument| looks_like_path_argument(argument))
		.filter_map(|argument| resolve_path_argument(argument, working_directory))
		.map(|path| path.display().to_string())
		.collect()
}

fn rejected_output(
	command_text: &str,
	argv: Option<&[String]>,
	execution: Option<&CanonicalExecution>,
	working_directory: &Path,
	scope_root: &Path,
	error_type: &str,
	message: &str,
	terminal: bool,
) -> Value {
	let mut data = json!({
		"command": command_text,
		"argv": argv.unwrap_or_default(),
		"cwd": working_directory.display().to_string(),
		"scope_root": scope_root.display().to_string(),
	});
	if let Some(execution) = execution {
		data["canonical_execution"] = serde_json::to_value(execution).unwrap_or(Value::Null);
		data["digest"] = Value::String(execution.digest.0.clone());
	}
	ToolOutputEnvelope::new(false, Some(error_type), terminal, message, data).into_value()
}

pub(super) fn contains_forbidden_shell_syntax(command_text: &str) -> bool {
	command_text.contains('\n')
		|| command_text.contains('\r')
		|| ['|', '&', ';', '>', '<']
			.iter()
			.any(|token| command_text.contains(*token))
		|| command_text.contains("$(")
}

pub(super) fn validate_allowed_command(program: &str, arguments: &[String]) -> Result<(), String> {
	match program {
		"pwd" | "ls" | "cat" | "head" | "tail" | "wc" | "find" | "rg" | "sleep" | "echo" => Ok(()),
		"git" => validate_git_subcommand(arguments),
		other => Err(format!(
			"`{other}` is outside the constrained allowlist for command.run."
		)),
	}
}

fn validate_git_subcommand(arguments: &[String]) -> Result<(), String> {
	let Some(subcommand) = arguments.first().map(String::as_str) else {
		return Err("`git` requires an explicit safe subcommand.".to_string());
	};
	match subcommand {
		"status" | "diff" | "show" | "log" | "rev-parse" | "branch" | "ls-files" => Ok(()),
		other => Err(format!(
			"`git {other}` is outside the constrained allowlist for command.run."
		)),
	}
}

fn first_out_of_scope_argument(
	arguments: &[String],
	working_directory: &Path,
	allowed_roots: &[PathBuf],
) -> Option<String> {
	arguments
		.iter()
		.filter(|argument| looks_like_path_argument(argument))
		.find_map(|argument| {
			resolve_path_argument(argument, working_directory).and_then(|resolved| {
				let allowed = allowed_roots.iter().any(|root| {
					root.canonicalize()
						.ok()
						.is_some_and(|canonical_root| resolved.starts_with(&canonical_root))
				});
				(!allowed).then(|| argument.clone())
			})
		})
}

fn looks_like_path_argument(argument: &str) -> bool {
	!argument.is_empty()
		&& !argument.starts_with('-')
		&& (argument.starts_with('/')
			|| argument.starts_with('.')
			|| argument.contains('/')
			|| argument.contains('\\'))
}

fn resolve_path_argument(argument: &str, working_directory: &Path) -> Option<PathBuf> {
	let candidate = PathBuf::from(argument);
	let absolute = if candidate.is_absolute() {
		candidate
	} else {
		working_directory.join(candidate)
	};
	absolute.canonicalize().ok()
}

fn resolve_working_directory(
	requested_cwd: Option<&str>,
	allowed_roots: &[PathBuf],
) -> Result<PathBuf, String> {
	let default_root = allowed_roots
		.first()
		.cloned()
		.unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
	let requested = requested_cwd
		.filter(|value| !value.trim().is_empty())
		.map(PathBuf::from)
		.unwrap_or(default_root.clone());
	let candidate = if requested.is_absolute() {
		requested
	} else {
		default_root.join(requested)
	};
	let canonical = candidate
		.canonicalize()
		.map_err(|error| format!("failed to resolve command working directory: {error}"))?;
	if !canonical.is_dir() {
		return Err(format!(
			"`{}` is not a directory for command.run.",
			canonical.display()
		));
	}
	let within_scope = allowed_roots.iter().any(|root| {
		root.canonicalize()
			.ok()
			.is_some_and(|canonical_root| canonical.starts_with(&canonical_root))
	});
	if !within_scope {
		return Err(format!(
			"`{}` is outside the allowed workspace roots for command.run.",
			canonical.display()
		));
	}
	Ok(canonical)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn rejects_shell_metacharacters() {
		assert!(contains_forbidden_shell_syntax("pwd && ls"));
		assert!(contains_forbidden_shell_syntax("cat foo | wc -l"));
		assert!(!contains_forbidden_shell_syntax("git status --short"));
	}

	#[test]
	fn rejects_unsafe_git_subcommands() {
		assert!(validate_git_subcommand(&["status".to_string()]).is_ok());
		assert!(validate_git_subcommand(&["commit".to_string()]).is_err());
	}

	#[test]
	fn rejected_command_output_preserves_canonical_execution_for_trace_evidence() {
		let current_dir = std::env::current_dir().expect("current directory should resolve");
		let request = ToolInvocationRequest {
			invocation_key: "test-command-reject".to_string(),
			attempt: 1,
			input: json!({
				"command": "touch phase3-boundary.tmp",
				"cwd": current_dir.display().to_string(),
			}),
			sandbox_profile: roku_plugin_host::SandboxProfile::ReadOnlyFs,
			attachments: Vec::new(),
			allowed_read_roots: vec![current_dir.clone()],
			allowed_write_roots: Vec::new(),
		};

		let PrepareCommandOutcome::Rejected(output) =
			prepare_command(&request, &CommandToolRuntimeConfig::default())
				.expect("prepare should reject disallowed commands")
		else {
			panic!("touch should be rejected before execution");
		};

		assert_eq!(output["error_type"], "command_not_allowed");
		assert_eq!(output["data"]["canonical_execution"]["program"], "touch");
		assert_eq!(output["data"]["digest"].as_str().map(str::len), Some(64),);
	}
}
