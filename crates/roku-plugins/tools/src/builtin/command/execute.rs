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
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use roku_common_types::{CanonicalExecution, ToolOutputEnvelope};
use roku_plugin_host::ToolFailure;
use serde_json::{Value, json};

use super::prepare::PreparedCommand;

pub(super) fn execute_prepared_command(
	prepared: &PreparedCommand,
	max_output_bytes: usize,
) -> Result<Value, ToolFailure> {
	let execution = &prepared.execution;
	let working_directory = PathBuf::from(&execution.cwd);
	let scope_root = execution
		.resource_scope
		.effective_read_roots
		.first()
		.cloned()
		.unwrap_or_else(|| execution.cwd.clone());

	let mut command = Command::new(&execution.program);
	command
		.args(execution.argv.iter().skip(1))
		.current_dir(&working_directory)
		.stdin(Stdio::null())
		.stdout(Stdio::piped())
		.stderr(Stdio::piped())
		.env("ROKU_COMMAND_SCOPE_ROOT", scope_root.clone())
		.env(
			"ROKU_ALLOWED_READ_ROOTS",
			serde_json::to_string(&execution.resource_scope.effective_read_roots)
				.unwrap_or_else(|_| "[]".to_string()),
		);
	let mut child = command.spawn().map_err(|error| {
		ToolFailure::terminal(format!("failed to spawn `{}`: {error}", execution.program))
	})?;
	let started = Instant::now();
	let timeout = Duration::from_millis(prepared.timeout_ms);
	loop {
		if let Some(status) = child.try_wait().map_err(|error| {
			ToolFailure::terminal(format!(
				"failed to wait for `{}`: {error}",
				execution.program
			))
		})? {
			let (stdout, stderr, truncated) = read_child_output(&mut child, max_output_bytes);
			return Ok(observed_output(
				execution,
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
			let (stdout, stderr, truncated) = read_child_output(&mut child, max_output_bytes);
			return Ok(timeout_output(
				execution,
				&scope_root,
				stdout,
				stderr,
				truncated,
				prepared.timeout_ms,
			));
		}
		thread::sleep(Duration::from_millis(10));
	}
}

fn observed_output(
	execution: &CanonicalExecution,
	scope_root: &str,
	exit_code: Option<i32>,
	stdout: String,
	stderr: String,
	truncated: bool,
) -> Value {
	let command_text = render_command(execution);
	let ok = exit_code == Some(0);
	let message = if ok {
		let out = stdout.trim();
		let err = stderr.trim();
		if !out.is_empty() && !err.is_empty() {
			format!("[stdout]\n{out}\n\n[stderr]\n{err}\n\n[exit_code: 0]")
		} else if !out.is_empty() {
			out.to_string()
		} else if !err.is_empty() {
			format!("[stderr]\n{err}\n\n[exit_code: 0]")
		} else {
			format!("`{command_text}` finished successfully with no output.")
		}
	} else {
		let mut parts: Vec<String> = Vec::new();
		if !stdout.trim().is_empty() {
			parts.push(format!("[stdout]\n{}", stdout.trim()));
		}
		if !stderr.trim().is_empty() {
			parts.push(format!("[stderr]\n{}", stderr.trim()));
		}
		parts.push(format!("[exit_code: {}]", exit_code.unwrap_or(-1)));
		parts.join("\n\n")
	};
	ToolOutputEnvelope::new(
		ok,
		(!ok).then_some("non_zero_exit"),
		false,
		message,
		json!({
			"command": command_text,
			"argv": execution.argv,
			"program": execution.program,
			"cwd": execution.cwd,
			"scope_root": scope_root,
			"exit_code": exit_code,
			"stdout": stdout,
			"stderr": stderr,
			"truncated": truncated,
			"digest": execution.digest.0,
		}),
	)
	.into_value()
}

fn timeout_output(
	execution: &CanonicalExecution,
	scope_root: &str,
	stdout: String,
	stderr: String,
	truncated: bool,
	timeout_ms: u64,
) -> Value {
	let command_text = render_command(execution);
	let mut parts: Vec<String> = vec![format!(
		"`{command_text}` exceeded its {timeout_ms}ms timeout."
	)];
	if !stdout.trim().is_empty() {
		parts.push(format!("[stdout]\n{}", stdout.trim()));
	}
	if !stderr.trim().is_empty() {
		parts.push(format!("[stderr]\n{}", stderr.trim()));
	}
	let message = parts.join("\n\n");
	ToolOutputEnvelope::new(
		false,
		Some("tool_timeout"),
		true,
		message,
		json!({
			"command": command_text,
			"argv": execution.argv,
			"program": execution.program,
			"cwd": execution.cwd,
			"scope_root": scope_root,
			"stdout": stdout,
			"stderr": stderr,
			"truncated": truncated,
			"timeout_ms": timeout_ms,
			"digest": execution.digest.0,
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

fn render_command(execution: &CanonicalExecution) -> String {
	execution
		.argv
		.iter()
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
	use roku_common_types::{
		CanonicalDigest, CanonicalExecution, ExecutionActionClass, ExecutionEnvPolicy,
		ExecutionResourceScope, InvocationMode, ToolOutputEnvelope,
	};
	use serde_json::Value;

	fn fake_execution(argv: &[&str]) -> CanonicalExecution {
		CanonicalExecution {
			tool_name: "command.run".to_string(),
			program: argv[0].to_string(),
			argv: argv.iter().map(|s| s.to_string()).collect(),
			invocation_mode: InvocationMode::DirectExec,
			shell_context: None,
			cwd: "/tmp".to_string(),
			env_policy: ExecutionEnvPolicy::default(),
			resource_scope: ExecutionResourceScope {
				working_directory: "/tmp".to_string(),
				resolved_targets: Vec::new(),
				effective_read_roots: vec!["/tmp".to_string()],
				effective_write_roots: Vec::new(),
			},
			action_class: ExecutionActionClass::Exec,
			digest: CanonicalDigest("test-digest".to_string()),
		}
	}

	fn envelope(v: Value) -> ToolOutputEnvelope {
		serde_json::from_value(v).expect("should deserialize as ToolOutputEnvelope")
	}

	#[test]
	fn success_stdout_only_no_labels() {
		let execution = fake_execution(&["echo", "hello"]);
		let result = super::observed_output(
			&execution,
			"/tmp",
			Some(0),
			"hello\n".to_string(),
			String::new(),
			false,
		);
		let env = envelope(result);
		assert!(env.ok);
		assert_eq!(env.message, "hello");
	}

	#[test]
	fn success_both_stdout_and_stderr_shows_labels() {
		let execution = fake_execution(&["make", "all"]);
		let result = super::observed_output(
			&execution,
			"/tmp",
			Some(0),
			"build ok\n".to_string(),
			"warning: unused\n".to_string(),
			false,
		);
		let env = envelope(result);
		assert!(env.ok);
		assert!(env.message.contains("[stdout]"));
		assert!(env.message.contains("build ok"));
		assert!(env.message.contains("[stderr]"));
		assert!(env.message.contains("warning: unused"));
		assert!(env.message.contains("[exit_code: 0]"));
	}

	#[test]
	fn success_stderr_only_shows_label_and_exit_code() {
		let execution = fake_execution(&["cmd"]);
		let result = super::observed_output(
			&execution,
			"/tmp",
			Some(0),
			String::new(),
			"some warning\n".to_string(),
			false,
		);
		let env = envelope(result);
		assert!(env.ok);
		assert!(env.message.contains("[stderr]"));
		assert!(env.message.contains("[exit_code: 0]"));
		assert!(!env.message.contains("[stdout]"));
	}

	#[test]
	fn success_no_output_emits_finished_message() {
		let execution = fake_execution(&["true"]);
		let result = super::observed_output(
			&execution,
			"/tmp",
			Some(0),
			String::new(),
			String::new(),
			false,
		);
		let env = envelope(result);
		assert!(env.ok);
		assert!(env.message.contains("finished successfully with no output"));
	}

	#[test]
	fn failure_always_shows_both_streams_and_exit_code() {
		let execution = fake_execution(&["false"]);
		let result = super::observed_output(
			&execution,
			"/tmp",
			Some(1),
			"partial output\n".to_string(),
			"error detail\n".to_string(),
			false,
		);
		let env = envelope(result);
		assert!(!env.ok);
		assert!(env.message.contains("[stdout]"));
		assert!(env.message.contains("partial output"));
		assert!(env.message.contains("[stderr]"));
		assert!(env.message.contains("error detail"));
		assert!(env.message.contains("[exit_code: 1]"));
	}

	#[test]
	fn failure_stderr_only_shows_exit_code() {
		let execution = fake_execution(&["false"]);
		let result = super::observed_output(
			&execution,
			"/tmp",
			Some(2),
			String::new(),
			"fatal error\n".to_string(),
			false,
		);
		let env = envelope(result);
		assert!(!env.ok);
		assert!(env.message.contains("[stderr]"));
		assert!(env.message.contains("[exit_code: 2]"));
		assert!(!env.message.contains("[stdout]"));
	}
}
