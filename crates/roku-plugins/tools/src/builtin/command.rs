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

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::contract::{
	contract_input_schema, contract_tool_schema, input_contract, input_field, output_contract,
	runtime_contract, selection_contract,
};
use crate::runtime_config::{CommandToolRuntimeConfig, HARD_MAX_TIMEOUT_MS};
use roku_common_types::{ToolContract, ToolOutputEnvelope, ToolRetryPolicy, ToolSideEffectPolicy};
use roku_plugin_catalog::{CatalogDescriptor, ResourceCost, ResourceKind, ResourceRisk};
use roku_plugin_host::{
	RuntimeConstraints, SandboxProfile, Tool, ToolDescriptor, ToolFailure, ToolInvocationRequest,
	ToolRuntime, ToolRuntimeError,
};
use serde_json::{Value, json};

#[allow(dead_code)]
pub(crate) fn catalog_descriptors() -> Vec<CatalogDescriptor> {
	catalog_descriptors_with_config(&CommandToolRuntimeConfig::default())
}

pub(crate) fn catalog_descriptors_with_config(
	config: &CommandToolRuntimeConfig,
) -> Vec<CatalogDescriptor> {
	let contract = command_contract(config.default_timeout_ms);
	vec![CatalogDescriptor {
		selector: roku_common_types::ResourceSelector::tool("command.run"),
		kind: ResourceKind::Tool,
		name: "command.run".to_string(),
		role: Some("core_command".to_string()),
		description: "Use this only when the request already includes one explicit shell-style command to run, such as a fenced bash snippet or inline command. Do not use it for multi-step scripts, shell metacharacters, or commands that would modify the workspace. It returns grounded command execution facts such as argv, cwd, exit code, stdout, stderr, truncation, and scope metadata."
			.to_string(),
		selection_hint: "Run one explicit read-only shell command that is already present in the request."
			.to_string(),
		discoverable: true,
		tags: vec![
			"command".to_string(),
			"execution".to_string(),
			"probe".to_string(),
		],
		examples: vec![
			"Run this command: `pwd`".to_string(),
			"Execute this bash command: ```bash\nrg \"tool\" crates/roku-agent-runtime\n```"
				.to_string(),
		],
		input_schema: contract_input_schema(Some(&contract), &["command".to_string(), "cwd".to_string()]),
		risk: ResourceRisk::Medium,
		cost: ResourceCost {
			estimated_tokens: 0,
			estimated_latency_ms: config.default_timeout_ms,
		},
		required_capabilities: vec!["command.run".to_string()],
		summary: "Run one constrained read-oriented command and return grounded subprocess facts."
			.to_string(),
		key_commands: Vec::new(),
		use_cases: Vec::new(),
		contract: Some(contract),
	}]
}

#[allow(dead_code)]
pub(crate) fn register_tools(runtime: &mut ToolRuntime) -> Result<(), ToolRuntimeError> {
	register_tools_with_config(runtime, &CommandToolRuntimeConfig::default())
}

pub(crate) fn register_tools_with_config(
	runtime: &mut ToolRuntime,
	config: &CommandToolRuntimeConfig,
) -> Result<(), ToolRuntimeError> {
	runtime.register_tool(CommandRunTool {
		config: config.clone(),
	})?;
	Ok(())
}

#[derive(Clone)]
struct CommandRunTool {
	config: CommandToolRuntimeConfig,
}

impl Tool for CommandRunTool {
	fn descriptor(&self) -> ToolDescriptor {
		let runtime_constraints = RuntimeConstraints {
			timeout_ms: self.config.default_timeout_ms,
			max_retries: 0,
			retry_backoff_ms: 0,
			sandbox_profile: SandboxProfile::ReadOnlyFs,
			deterministic_hooks: false,
			allowed_read_roots: default_allowed_roots(),
			allowed_write_roots: Vec::new(),
		};
		let contract = command_contract(self.config.default_timeout_ms);
		ToolDescriptor {
			name: "command.run".to_string(),
			version: "1.0.0".to_string(),
			input_schema: contract_tool_schema(
				Some(&contract),
				&[
					"task_id",
					"node_id",
					"goal",
					"summary",
					"conversation_history",
					"budget_tokens",
					"time_budget_ms",
				],
			),
			output_schema: contract.output.observation_schema.clone(),
			required_capabilities: vec!["command.run".to_string()],
			runtime_constraints,
			contract: Some(contract),
		}
	}

	fn invoke(&self, request: ToolInvocationRequest) -> Result<Value, ToolFailure> {
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
			.unwrap_or(self.config.default_timeout_ms)
			.min(self.config.default_timeout_ms)
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
			return Ok(rejected_output(
				command_text,
				None,
				&working_directory,
				&scope_root,
				"unsafe_shell_syntax",
				"command.run only accepts one explicit command without shell metacharacters or pipelines.",
				true,
			));
		}

		let Some(argv) = shlex::split(command_text) else {
			return Ok(rejected_output(
				command_text,
				None,
				&working_directory,
				&scope_root,
				"invalid_command_syntax",
				"command.run could not parse the explicit command string.",
				true,
			));
		};
		if argv.is_empty() {
			return Ok(rejected_output(
				command_text,
				Some(&argv),
				&working_directory,
				&scope_root,
				"empty_command",
				"command.run requires a non-empty command.",
				true,
			));
		}

		let program = argv[0].clone();
		let arguments = argv[1..].to_vec();
		if let Err(message) = validate_allowed_command(&program, &arguments) {
			return Ok(rejected_output(
				command_text,
				Some(&argv),
				&working_directory,
				&scope_root,
				"command_not_allowed",
				&message,
				true,
			));
		}
		if let Some(argument) =
			first_out_of_scope_argument(&arguments, &working_directory, &request.allowed_read_roots)
		{
			return Ok(rejected_output(
				command_text,
				Some(&argv),
				&working_directory,
				&scope_root,
				"path_out_of_scope",
				&format!(
					"`{argument}` resolves outside the allowed workspace roots for command.run."
				),
				true,
			));
		}

		let mut command = Command::new(&program);
		command
			.args(&arguments)
			.current_dir(&working_directory)
			.stdin(Stdio::null())
			.stdout(Stdio::piped())
			.stderr(Stdio::piped())
			.env("ROKU_COMMAND_SCOPE_ROOT", scope_root.display().to_string())
			.env(
				"ROKU_ALLOWED_READ_ROOTS",
				serde_json::to_string(
					&request
						.allowed_read_roots
						.iter()
						.map(|path| path.display().to_string())
						.collect::<Vec<_>>(),
				)
				.unwrap_or_else(|_| "[]".to_string()),
			);
		let mut child = command.spawn().map_err(|error| {
			ToolFailure::terminal(format!("failed to spawn `{program}`: {error}"))
		})?;
		let started = Instant::now();
		let timeout = Duration::from_millis(timeout_ms);
		loop {
			if let Some(status) = child.try_wait().map_err(|error| {
				ToolFailure::terminal(format!("failed to wait for `{program}`: {error}"))
			})? {
				let (stdout, stderr, truncated) =
					read_child_output(&mut child, self.config.max_output_bytes);
				return Ok(observed_output(
					command_text,
					&argv,
					&working_directory,
					&scope_root,
					status.code(),
					stdout,
					stderr,
					truncated,
				));
			}
			if started.elapsed() >= timeout {
				let _ = child.kill();
				let _ = child.wait();
				let (stdout, stderr, truncated) =
					read_child_output(&mut child, self.config.max_output_bytes);
				return Ok(timeout_output(
					command_text,
					&argv,
					&working_directory,
					&scope_root,
					stdout,
					stderr,
					truncated,
					timeout_ms,
				));
			}
			thread::sleep(Duration::from_millis(10));
		}
	}
}

fn observed_output(
	command_text: &str,
	argv: &[String],
	working_directory: &Path,
	scope_root: &Path,
	exit_code: Option<i32>,
	stdout: String,
	stderr: String,
	truncated: bool,
) -> Value {
	let ok = exit_code == Some(0);
	let message = if ok {
		let visible = stdout.trim();
		if visible.is_empty() {
			format!("`{command_text}` finished successfully with no stdout.")
		} else {
			visible.to_string()
		}
	} else if !stderr.trim().is_empty() {
		stderr.trim().to_string()
	} else {
		format!(
			"`{command_text}` exited with status {}.",
			exit_code.unwrap_or(-1)
		)
	};
	ToolOutputEnvelope::new(
		ok,
		(!ok).then_some("non_zero_exit"),
		false,
		message,
		json!({
			"command": command_text,
			"argv": argv,
			"program": argv.first().cloned().unwrap_or_default(),
			"cwd": working_directory.display().to_string(),
			"scope_root": scope_root.display().to_string(),
			"exit_code": exit_code,
			"stdout": stdout,
			"stderr": stderr,
			"truncated": truncated,
		}),
	)
	.into_value()
}

fn timeout_output(
	command_text: &str,
	argv: &[String],
	working_directory: &Path,
	scope_root: &Path,
	stdout: String,
	stderr: String,
	truncated: bool,
	timeout_ms: u64,
) -> Value {
	ToolOutputEnvelope::new(
		false,
		Some("tool_timeout"),
		true,
		format!("`{command_text}` exceeded its {}ms timeout.", timeout_ms),
		json!({
			"command": command_text,
			"argv": argv,
			"program": argv.first().cloned().unwrap_or_default(),
			"cwd": working_directory.display().to_string(),
			"scope_root": scope_root.display().to_string(),
			"stdout": stdout,
			"stderr": stderr,
			"truncated": truncated,
			"timeout_ms": timeout_ms,
		}),
	)
	.into_value()
}

fn rejected_output(
	command_text: &str,
	argv: Option<&[String]>,
	working_directory: &Path,
	scope_root: &Path,
	error_type: &str,
	message: &str,
	terminal: bool,
) -> Value {
	ToolOutputEnvelope::new(
		false,
		Some(error_type),
		terminal,
		message,
		json!({
			"command": command_text,
			"argv": argv.unwrap_or_default(),
			"cwd": working_directory.display().to_string(),
			"scope_root": scope_root.display().to_string(),
		}),
	)
	.into_value()
}

fn read_child_output(
	child: &mut std::process::Child,
	max_output_bytes: usize,
) -> (String, String, bool) {
	let mut stdout = String::new();
	let mut stderr = String::new();
	if let Some(handle) = child.stdout.take() {
		let _ = handle
			.take(u64::try_from(max_output_bytes).unwrap_or(u64::MAX))
			.read_to_string(&mut stdout);
	}
	if let Some(handle) = child.stderr.take() {
		let _ = handle
			.take(u64::try_from(max_output_bytes).unwrap_or(u64::MAX))
			.read_to_string(&mut stderr);
	}
	let truncated = stdout.len() >= max_output_bytes || stderr.len() >= max_output_bytes;
	(stdout, stderr, truncated)
}

fn contains_forbidden_shell_syntax(command_text: &str) -> bool {
	command_text.contains('\n')
		|| command_text.contains('\r')
		|| ['|', '&', ';', '>', '<']
			.iter()
			.any(|token| command_text.contains(*token))
		|| command_text.contains("$(")
}

fn validate_allowed_command(program: &str, arguments: &[String]) -> Result<(), String> {
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

fn default_allowed_roots() -> Vec<PathBuf> {
	std::env::current_dir()
		.ok()
		.and_then(|path| path.canonicalize().ok())
		.map(|path| vec![path])
		.unwrap_or_default()
}

fn command_contract(timeout_ms: u64) -> ToolContract {
	let runtime_constraints = RuntimeConstraints {
		timeout_ms,
		max_retries: 0,
		retry_backoff_ms: 0,
		sandbox_profile: SandboxProfile::ReadOnlyFs,
		deterministic_hooks: false,
		allowed_read_roots: default_allowed_roots(),
		allowed_write_roots: Vec::new(),
	};
	ToolContract {
		selection: selection_contract(
			&[
				"Use when the user already provided one explicit shell command to run.",
				"Best for bounded read-oriented commands such as pwd, ls, cat, rg, or safe git inspection.",
			],
			&[
				"Do not use for multi-step scripts, chained shell expressions, or commands that modify the workspace.",
				"Do not use when the user asks to explain a command without executing it.",
			],
			&[
				"Commonly confused with python.run for inline backticks that are shell commands, not Python snippets.",
				"Commonly confused with fs.* tools when a direct file read or directory listing would answer the question more safely.",
			],
		),
		input: input_contract(
			vec![
				input_field(
					"command",
					true,
					"One explicit shell-style command string to execute after shlex-style parsing.",
					&[
						"Reject when the string contains shell metacharacters, multiple commands, or unsupported programs.",
					],
				),
				input_field(
					"cwd",
					false,
					"Optional working directory resolved under the allowed read roots.",
					&["Reject when the directory resolves outside the allowed workspace roots."],
				),
				input_field(
					"timeout_ms",
					false,
					"Optional per-call timeout capped by the configured command.run default timeout.",
					&["Reject when zero or larger than the configured hard timeout ceiling."],
				),
			],
			&[
				"Execution stays inside read-only workspace roots and a constrained command allowlist.",
			],
		),
		output: output_contract(
			"Returns grounded subprocess facts such as argv, cwd, exit code, stdout, stderr, truncation, and scope_root.",
			"Successful commands with no stdout still return ok=true and a message explaining that stdout was empty.",
			&[
				"Unsupported commands, unsafe shell syntax, out-of-scope paths, and timeouts surface as explicit error_type values.",
				"Non-zero exit codes remain non-terminal tool observations and do not by themselves declare task completion.",
			],
			true,
			false,
		),
		runtime: runtime_contract(
			&runtime_constraints,
			ToolSideEffectPolicy::ReadOnly,
			ToolRetryPolicy::Never,
		),
	}
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
}
