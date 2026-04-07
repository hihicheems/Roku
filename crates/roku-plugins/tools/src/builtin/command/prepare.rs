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
use roku_observability::{LogLevel, LogRecord, emit_global_log};
use roku_plugin_host::{ToolFailure, ToolInvocationRequest};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// Command execution policy level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum CommandPolicyLevel {
	/// Read-only commands with no side effects. Always allowed.
	AutoAllow,
	/// Development commands with side effects. Allowed with logging.
	AllowWithLog,
	/// Dangerous commands. Always denied.
	Deny,
}

/// Timeout profile for command execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum TimeoutProfile {
	/// For quick read-only commands (default: 30 000 ms).
	Quick,
	/// For build/compile commands (default: 300 000 ms).
	Build,
	/// For long-running test suites (default: 600 000 ms).
	Long,
}

impl TimeoutProfile {
	/// Resolve to a concrete millisecond value.
	///
	/// The returned value is still subject to the caller's own hard-cap after
	/// the optional user-supplied `timeout_ms` override is applied.
	pub(crate) fn resolve_ms(self) -> u64 {
		match self {
			TimeoutProfile::Quick => 30_000,
			TimeoutProfile::Build => 300_000,
			TimeoutProfile::Long => 600_000,
		}
	}

	fn is_build_command(program: &str) -> bool {
		matches!(
			program,
			"cargo"
				| "just" | "make"
				| "cmake" | "npm"
				| "yarn" | "pnpm"
				| "bun" | "pip"
				| "poetry" | "uv"
				| "go" | "mvn"
				| "gradle" | "rustc"
		)
	}
}

/// Result of evaluating a command against the execution policy.
#[derive(Debug, Clone)]
pub(crate) struct CommandPolicy {
	pub level: CommandPolicyLevel,
	pub timeout_profile: TimeoutProfile,
}

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

	if let Err(message) = validate_shell_syntax(command_text) {
		return Ok(PrepareCommandOutcome::Rejected(rejected_output(
			command_text,
			None,
			None,
			&working_directory,
			&scope_root,
			"unsafe_shell_syntax",
			&message,
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

	let policy = match evaluate_command_policy(&program, &arguments) {
		Ok(policy) => policy,
		Err(message) => {
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
	};

	if policy.level == CommandPolicyLevel::AllowWithLog {
		let record = LogRecord::new(
			"command.run",
			LogLevel::Info,
			"executing allow_with_log command",
		)
		.with_field("program", program.as_str())
		.with_field("args", format!("{arguments:?}"));
		let _ = emit_global_log(record);
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

	// Resolve timeout: user-supplied value takes precedence, then the profile
	// default. Both are capped by the configured default (which is itself
	// capped by the hard maximum in validate_and_clamp).
	let profile_ms = policy.timeout_profile.resolve_ms();
	let timeout_ms = request
		.input
		.get("timeout_ms")
		.and_then(Value::as_u64)
		.unwrap_or(profile_ms)
		.min(profile_ms)
		.min(config.default_timeout_ms)
		.min(HARD_MAX_TIMEOUT_MS);

	Ok(PrepareCommandOutcome::Ready(
		PreparedCommand {
			execution,
			timeout_ms,
		}
		.into(),
	))
}

/// Evaluate a parsed command against the 3-level execution policy.
///
/// Returns `Ok(CommandPolicy)` for `AutoAllow` and `AllowWithLog` commands,
/// and `Err(message)` for `Deny` commands with a human-readable explanation.
pub(crate) fn evaluate_command_policy(
	program: &str,
	arguments: &[String],
) -> Result<CommandPolicy, String> {
	// --- Level 3: Deny --- checked first so deny patterns win over allow lists.
	if let Some(reason) = deny_reason(program, arguments) {
		return Err(reason);
	}

	// --- Level 1: AutoAllow ---
	if is_auto_allow(program, arguments) {
		return Ok(CommandPolicy {
			level: CommandPolicyLevel::AutoAllow,
			timeout_profile: TimeoutProfile::Quick,
		});
	}

	// --- Level 2: AllowWithLog ---
	if is_allow_with_log(program, arguments) {
		let timeout_profile = if TimeoutProfile::is_build_command(program) {
			TimeoutProfile::Build
		} else {
			TimeoutProfile::Quick
		};
		return Ok(CommandPolicy {
			level: CommandPolicyLevel::AllowWithLog,
			timeout_profile,
		});
	}

	Err(format!(
		"`{program}` is outside the allowed command policy for command.run."
	))
}

/// Returns `true` if the command is unconditionally allowed (Level 1).
fn is_auto_allow(program: &str, arguments: &[String]) -> bool {
	match program {
		// NOTE: `env` is intentionally excluded — `env <program>` bypasses the deny list
		// because only argv[0] is checked against the policy.
		"pwd" | "ls" | "cat" | "head" | "tail" | "wc" | "find" | "rg" | "echo" | "sleep"
		| "which" | "whoami" | "uname" | "date" | "realpath" | "dirname" | "basename" | "sort"
		| "uniq" | "tr" | "cut" | "diff" | "comm" | "tee" | "file" | "stat" | "du" | "df" => true,
		"git" => is_git_auto_allow(arguments),
		_ => false,
	}
}

fn is_git_auto_allow(arguments: &[String]) -> bool {
	let subcommand = match arguments.first().map(String::as_str) {
		Some(s) => s,
		None => return false,
	};
	match subcommand {
		"status" | "diff" | "show" | "log" | "rev-parse" | "branch" | "ls-files" | "remote"
		| "tag" => true,
		// `git stash list` only — bare `git stash` is a write operation
		"stash" => arguments.get(1).map(String::as_str) == Some("list"),
		_ => false,
	}
}

/// Returns `true` if the command is allowed with logging (Level 2).
fn is_allow_with_log(program: &str, arguments: &[String]) -> bool {
	match program {
		// Build / test runners
		"cargo" | "just" | "make" | "cmake" | "npm" | "yarn" | "pnpm" | "bun" | "pip"
		| "poetry" | "uv" | "rustup" | "rustc" | "rustfmt" | "clippy-driver" | "python"
		| "python3" | "node" | "deno" | "go" | "javac" | "java" | "mvn" | "gradle" => true,
		// Package managers — read subcommands only
		"brew" => is_brew_read_subcommand(arguments),
		"apt" | "apt-get" => is_apt_read_subcommand(arguments),
		"pacman" => is_pacman_read_subcommand(arguments),
		"dnf" | "yum" => is_dnf_read_subcommand(arguments),
		// Docker — read subcommands only
		"docker" => is_docker_read_subcommand(arguments),
		// File manipulation (bounded)
		"mkdir" | "touch" | "cp" | "mv" | "ln" => true,
		"chmod" => !has_recursive_flag(arguments),
		"rm" => is_rm_single_file(arguments),
		// Text processing (for pipe usage)
		"sed" | "awk" | "grep" => true,
		// Version control write operations
		"git" => is_git_allow_with_log(arguments),
		_ => false,
	}
}

fn is_git_allow_with_log(arguments: &[String]) -> bool {
	let subcommand = match arguments.first().map(String::as_str) {
		Some(s) => s,
		None => return false,
	};
	match subcommand {
		"add" | "commit" | "push" | "pull" | "fetch" | "checkout" | "switch" | "merge"
		| "rebase" | "cherry-pick" | "restore" => true,
		// `git reset` without --hard
		"reset" => !arguments.iter().any(|a| a == "--hard"),
		// `git stash` (bare, pop, drop)
		"stash" => {
			let sub2 = arguments.get(1).map(String::as_str);
			matches!(sub2, None | Some("pop") | Some("drop"))
		}
		// `git clean` only with -n (dry-run)
		"clean" => {
			arguments.iter().any(|a| a == "-n" || a.contains('n'))
				&& !arguments.iter().any(|a| a == "-f" || a.contains('f'))
		}
		_ => false,
	}
}

fn is_brew_read_subcommand(arguments: &[String]) -> bool {
	matches!(
		arguments.first().map(String::as_str),
		Some("list") | Some("search") | Some("info")
	)
}

fn is_apt_read_subcommand(arguments: &[String]) -> bool {
	matches!(
		arguments.first().map(String::as_str),
		Some("list") | Some("search") | Some("show") | Some("info")
	)
}

fn is_pacman_read_subcommand(arguments: &[String]) -> bool {
	// -Q (query), -S (sync) search only
	arguments.iter().any(|a| a.starts_with("-Q") || a == "-Ss")
}

fn is_dnf_read_subcommand(arguments: &[String]) -> bool {
	matches!(
		arguments.first().map(String::as_str),
		Some("list") | Some("search") | Some("info")
	)
}

fn is_docker_read_subcommand(arguments: &[String]) -> bool {
	matches!(
		arguments.first().map(String::as_str),
		Some("ps") | Some("images") | Some("logs") | Some("inspect")
	)
}

fn has_recursive_flag(arguments: &[String]) -> bool {
	arguments
		.iter()
		.any(|a| a == "-R" || a == "-r" || a == "--recursive")
}

/// Returns `true` if the `rm` invocation targets a single file without the
/// recursive / force flags that would make it dangerous.
fn is_rm_single_file(arguments: &[String]) -> bool {
	// Deny if -r/-R/--recursive is present (directory removal).
	if has_recursive_flag(arguments) {
		return false;
	}
	// Deny combined -rf/-fr variants.
	let has_rf = arguments
		.iter()
		.any(|a| matches!(a.as_str(), "-rf" | "-fr" | "-Rf" | "-fR"));
	!has_rf
}

/// Returns a denial reason string if the command matches a Level-3 deny rule,
/// or `None` if the command may proceed to the allow checks.
fn deny_reason(program: &str, arguments: &[String]) -> Option<String> {
	match program {
		"dd" | "mkfs" | "fdisk" | "mount" | "umount" | "reboot" | "shutdown" | "halt" | "init"
		| "killall" | "nc" | "ncat" | "socat" => Some(format!(
			"`{program}` is permanently denied by the command execution policy."
		)),

		"kill" => {
			if arguments.iter().any(|a| a == "-9" || a == "-SIGKILL") {
				Some(
					"`kill -9` (SIGKILL) is denied. Use a softer signal such as SIGTERM instead."
						.to_string(),
				)
			} else {
				None
			}
		}

		"rm" => deny_reason_rm(arguments),

		"git" => deny_reason_git(arguments),

		_ => None,
	}
}

fn deny_reason_rm(arguments: &[String]) -> Option<String> {
	// Detect -rf/-fr combined flags.
	let has_rf_combined = arguments
		.iter()
		.any(|a| matches!(a.as_str(), "-rf" | "-fr" | "-Rf" | "-fR"));
	// Detect separate -r and -f flags together.
	let has_recursive = has_recursive_flag(arguments);
	let has_force = arguments.iter().any(|a| a == "-f" || a == "--force");
	let has_rf = has_rf_combined || (has_recursive && has_force);

	if !has_rf {
		return None;
	}

	// Catastrophic targets: /, ~, .
	let targets: Vec<&str> = arguments
		.iter()
		.filter(|a| !a.starts_with('-'))
		.map(String::as_str)
		.collect();

	for target in &targets {
		if matches!(*target, "/" | "~" | ".") {
			return Some(format!(
				"`rm` with `-rf` on `{target}` is permanently denied (catastrophic destruction)."
			));
		}
	}

	// Any rm -rf on a directory path is also denied.
	Some("`rm -rf` on directories is denied. Use `rm` on single files only.".to_string())
}

fn deny_reason_git(arguments: &[String]) -> Option<String> {
	let subcommand = arguments.first().map(String::as_str)?;

	match subcommand {
		"reset" => {
			if arguments.iter().any(|a| a == "--hard") {
				Some(
					"`git reset --hard` is denied. Use `git restore` or `git reset` without \
					 `--hard` instead."
						.to_string(),
				)
			} else {
				None
			}
		}
		"push" => {
			let is_force = arguments
				.iter()
				.any(|a| a == "--force" || a == "-f" || a == "--force-with-lease");
			if !is_force {
				return None;
			}
			// Force push to main/master is always denied.
			let to_main = arguments.iter().any(|a| {
				let s = a.as_str();
				s == "main" || s == "master" || s.ends_with(":main") || s.ends_with(":master")
			});
			if to_main {
				Some("`git push --force` to `main`/`master` is permanently denied.".to_string())
			} else {
				None
			}
		}
		"clean" => {
			// `git clean` without a dry-run flag (-n) is denied.
			let has_dry_run = arguments.iter().any(|a| a == "-n" || a.contains('n'));
			let has_force = arguments.iter().any(|a| a == "-f" || a.contains('f'));
			if has_force && !has_dry_run {
				Some(
					"`git clean -f` without `-n` (dry-run) is denied. Add `-n` first to preview \
					 what would be removed."
						.to_string(),
				)
			} else {
				None
			}
		}
		_ => None,
	}
}

/// Validate shell syntax in the command text.
///
/// This replaces the old `contains_forbidden_shell_syntax` with a
/// Validates that a command string does not contain shell metacharacters.
///
/// The executor uses `std::process::Command` (direct exec, no shell), so
/// `|`, `>`, `&&`, `||` would be passed as literal arguments and silently
/// misbehave. They are forbidden here until a shell-based execution path is
/// added.
pub(super) fn validate_shell_syntax(command_text: &str) -> Result<(), String> {
	if command_text.contains('\n') || command_text.contains('\r') {
		return Err(
			"command.run does not accept multi-line commands. Use a single explicit command."
				.to_string(),
		);
	}
	if command_text.contains("$(") {
		return Err(
			"command.run does not accept command substitution `$(...)`. Pass values explicitly."
				.to_string(),
		);
	}
	// Pipe, redirect, chaining, and background operators are forbidden because
	// the executor uses direct exec (no shell). These characters would be
	// passed as literal arguments to the program.
	for &token in &['|', '&', ';', '>', '<'] {
		if command_text.contains(token) {
			return Err(format!(
				"command.run does not accept shell operator `{token}`. \
				 The executor uses direct exec without a shell. \
				 Run each command separately instead."
			));
		}
	}
	Ok(())
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
		digest
			.iter()
			.map(|b| format!("{b:02x}"))
			.collect::<String>(),
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
		// Semicolons and command substitution are still forbidden.
		assert!(validate_shell_syntax("cat foo; ls").is_err());
		assert!(validate_shell_syntax("echo $(pwd)").is_err());
		// Newlines are forbidden.
		assert!(validate_shell_syntax("git status\nrm -rf /").is_err());
		// Bare & (background) is forbidden.
		assert!(validate_shell_syntax("sleep 100 &").is_err());
	}

	#[test]
	fn rejects_shell_operators_because_executor_uses_direct_exec() {
		// Pipes, redirects, and logical operators are rejected because the
		// executor uses std::process::Command (direct exec, no shell).
		assert!(validate_shell_syntax("cat foo | wc -l").is_err());
		assert!(validate_shell_syntax("ls > out.txt").is_err());
		assert!(validate_shell_syntax("ls >> out.txt").is_err());
		assert!(validate_shell_syntax("cargo build && cargo test").is_err());
		assert!(validate_shell_syntax("cargo build || echo failed").is_err());
		// Plain git commands still pass.
		assert!(validate_shell_syntax("git status --short").is_ok());
	}

	#[test]
	fn rejects_unsafe_git_subcommands() {
		// git reset --hard is now explicitly denied at the policy level.
		assert!(
			evaluate_command_policy("git", &["reset".to_string(), "--hard".to_string()]).is_err()
		);
		// git push --force to main is denied.
		assert!(
			evaluate_command_policy(
				"git",
				&[
					"push".to_string(),
					"--force".to_string(),
					"main".to_string()
				]
			)
			.is_err()
		);
		// git clean -fd (without dry-run) is denied.
		assert!(evaluate_command_policy("git", &["clean".to_string(), "-fd".to_string()]).is_err());
	}

	#[test]
	fn allows_git_read_subcommands() {
		assert!(evaluate_command_policy("git", &["status".to_string()]).is_ok());
		assert!(
			evaluate_command_policy("git", &["log".to_string(), "--oneline".to_string()]).is_ok()
		);
		assert!(evaluate_command_policy("git", &["stash".to_string(), "list".to_string()]).is_ok());
	}

	#[test]
	fn allows_git_write_subcommands_with_log() {
		let policy = evaluate_command_policy(
			"git",
			&["commit".to_string(), "-m".to_string(), "msg".to_string()],
		)
		.expect("git commit should be allowed with log");
		assert_eq!(policy.level, CommandPolicyLevel::AllowWithLog);
	}

	#[test]
	fn allows_build_tools() {
		let policy = evaluate_command_policy("cargo", &["check".to_string()])
			.expect("cargo should be allowed");
		assert_eq!(policy.level, CommandPolicyLevel::AllowWithLog);
		assert_eq!(policy.timeout_profile, TimeoutProfile::Build);
	}

	#[test]
	fn denies_dangerous_binaries() {
		assert!(evaluate_command_policy("dd", &[]).is_err());
		assert!(evaluate_command_policy("mkfs", &[]).is_err());
		assert!(evaluate_command_policy("nc", &[]).is_err());
	}

	#[test]
	fn denies_rm_rf() {
		assert!(evaluate_command_policy("rm", &["-rf".to_string(), "/".to_string()]).is_err());
		assert!(evaluate_command_policy("rm", &["-rf".to_string(), "~".to_string()]).is_err());
		assert!(evaluate_command_policy("rm", &["-rf".to_string(), ".".to_string()]).is_err());
		// rm on single file without -r is allowed
		assert!(evaluate_command_policy("rm", &["somefile.txt".to_string()]).is_ok());
	}

	#[test]
	fn auto_allow_commands_get_quick_timeout() {
		let policy = evaluate_command_policy("ls", &[]).expect("ls should be auto-allowed");
		assert_eq!(policy.level, CommandPolicyLevel::AutoAllow);
		assert_eq!(policy.timeout_profile, TimeoutProfile::Quick);
	}

	#[test]
	fn rejected_command_output_preserves_canonical_execution_for_trace_evidence() {
		let current_dir = std::env::current_dir().expect("current directory should resolve");
		let request = roku_plugin_host::ToolInvocationRequest {
			invocation_key: "test-command-reject".to_string(),
			attempt: 1,
			input: serde_json::json!({
				"command": "dd if=/dev/zero of=test.img bs=1M count=1",
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
			panic!("dd should be rejected before execution");
		};

		assert_eq!(output["error_type"], "command_not_allowed");
		assert_eq!(output["data"]["canonical_execution"]["program"], "dd");
		assert_eq!(output["data"]["digest"].as_str().map(str::len), Some(64),);
	}
}
