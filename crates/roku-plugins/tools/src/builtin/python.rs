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

use crate::contract::{
	contract_input_schema, contract_tool_schema, input_contract, input_field, output_contract,
	runtime_contract, selection_contract,
};
use crate::runtime_config::{HARD_MAX_TIMEOUT_MS, PythonToolRuntimeConfig};
use roku_common_types::{ToolContract, ToolOutputEnvelope, ToolRetryPolicy, ToolSideEffectPolicy};
use roku_plugin_catalog::{CatalogDescriptor, ResourceCost, ResourceKind, ResourceRisk};
use roku_plugin_host::{
	RuntimeConstraints, SandboxProfile, Tool, ToolDescriptor, ToolFailure, ToolInvocationRequest,
	ToolRuntime, ToolRuntimeError,
};
use serde_json::{Value, json};

#[allow(dead_code)]
pub(crate) fn catalog_descriptors() -> Vec<CatalogDescriptor> {
	catalog_descriptors_with_config(&PythonToolRuntimeConfig::default())
}

pub(crate) fn catalog_descriptors_with_config(
	config: &PythonToolRuntimeConfig,
) -> Vec<CatalogDescriptor> {
	let contract = python_contract(config.default_timeout_ms);
	vec![CatalogDescriptor {
		selector: roku_common_types::ResourceSelector::tool("python.run"),
		kind: ResourceKind::Tool,
		name: "python.run".to_string(),
		role: Some("core_python".to_string()),
		description: "Use this only when the request already contains explicit Python code to run or a clearly bounded snippet the agent has produced as code. Do not dump raw natural-language tasks into it and do not use it for shell commands. It returns stdout/stderr and exit facts from a constrained subprocess, which can be interpreted or summarized later."
			.to_string(),
		selection_hint:
			"Run explicit Python code from the request or one clearly bounded snippet for local computation."
				.to_string(),
		discoverable: true,
		tags: vec![
			"python".to_string(),
			"code".to_string(),
			"execution".to_string(),
		],
		examples: vec![
			"Run this Python code: ```python\nprint(sum(range(1, 11)))\n```".to_string(),
		],
		input_schema: contract_input_schema(
			Some(&contract),
			&["code".to_string(), "timeout_ms".to_string()],
		),
		risk: ResourceRisk::Medium,
		cost: ResourceCost {
			estimated_tokens: 0,
			estimated_latency_ms: config.default_timeout_ms,
		},
		required_capabilities: vec!["python.run".to_string()],
		summary: "Run explicit Python code and return grounded subprocess output.".to_string(),
		key_commands: Vec::new(),
		use_cases: Vec::new(),
		contract: Some(contract),
	}]
}

#[allow(dead_code)]
pub(crate) fn register_tools(runtime: &mut ToolRuntime) -> Result<(), ToolRuntimeError> {
	register_tools_with_config(runtime, &PythonToolRuntimeConfig::default())
}

pub(crate) fn register_tools_with_config(
	runtime: &mut ToolRuntime,
	config: &PythonToolRuntimeConfig,
) -> Result<(), ToolRuntimeError> {
	runtime.register_tool(PythonRunTool {
		config: config.clone(),
	})?;
	Ok(())
}

#[derive(Clone)]
struct PythonRunTool {
	config: PythonToolRuntimeConfig,
}

impl Tool for PythonRunTool {
	fn descriptor(&self) -> ToolDescriptor {
		let runtime_constraints = RuntimeConstraints {
			timeout_ms: self.config.default_timeout_ms,
			max_retries: 0,
			retry_backoff_ms: 0,
			sandbox_profile: SandboxProfile::PythonResearch,
			deterministic_hooks: false,
			allowed_read_roots: default_allowed_roots(),
			allowed_write_roots: Vec::new(),
		};
		let contract = python_contract(self.config.default_timeout_ms);
		ToolDescriptor {
			name: "python.run".to_string(),
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
			required_capabilities: vec!["python.run".to_string()],
			runtime_constraints,
			contract: Some(contract),
		}
	}

	fn invoke(&self, request: ToolInvocationRequest) -> Result<Value, ToolFailure> {
		let code = request
			.input
			.get("code")
			.and_then(Value::as_str)
			.filter(|value| !value.trim().is_empty())
			.ok_or_else(|| ToolFailure::terminal("missing required field `code`"))?;
		let timeout_ms = request
			.input
			.get("timeout_ms")
			.and_then(Value::as_u64)
			.unwrap_or(self.config.default_timeout_ms)
			.min(self.config.default_timeout_ms)
			.min(HARD_MAX_TIMEOUT_MS);
		let attachments = request
			.attachments
			.iter()
			.map(|path| path.display().to_string())
			.collect::<Vec<_>>();
		let mut command = Command::new("python3");
		command
			.arg("-c")
			.arg(code)
			.stdin(Stdio::null())
			.stdout(Stdio::piped())
			.stderr(Stdio::piped())
			.env("PYTHONUNBUFFERED", "1")
			.env(
				"ROKU_ATTACHMENTS",
				serde_json::to_string(
					&request
						.attachments
						.iter()
						.map(|path| path.display().to_string())
						.collect::<Vec<_>>(),
				)
				.unwrap_or_else(|_| "[]".to_string()),
			)
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
			)
			.env(
				"ROKU_ALLOWED_WRITE_ROOTS",
				serde_json::to_string(
					&request
						.allowed_write_roots
						.iter()
						.map(|path| path.display().to_string())
						.collect::<Vec<_>>(),
				)
				.unwrap_or_else(|_| "[]".to_string()),
			);
		if let Some(cwd) = request.allowed_read_roots.first() {
			command.current_dir(cwd);
		}
		let mut child = command
			.spawn()
			.map_err(|error| ToolFailure::terminal(format!("failed to spawn python3: {error}")))?;
		let started = Instant::now();
		let timeout = Duration::from_millis(timeout_ms);
		loop {
			if let Some(status) = child.try_wait().map_err(|error| {
				ToolFailure::terminal(format!("failed to wait for python3: {error}"))
			})? {
				let (stdout, stderr, truncated) =
					read_child_output(&mut child, self.config.max_output_bytes);
				return Ok(observed_output(
					code,
					status.code(),
					stdout,
					stderr,
					truncated,
					attachments.clone(),
				));
			}
			if started.elapsed() >= timeout {
				let _ = child.kill();
				let _ = child.wait();
				let (stdout, stderr, truncated) =
					read_child_output(&mut child, self.config.max_output_bytes);
				return Ok(timeout_output(
					code,
					stdout,
					stderr,
					truncated,
					attachments.clone(),
					timeout_ms,
				));
			}
			thread::sleep(Duration::from_millis(10));
		}
	}
}

fn observed_output(
	code: &str,
	exit_code: Option<i32>,
	stdout: String,
	stderr: String,
	truncated: bool,
	attachments: Vec<String>,
) -> Value {
	let ok = exit_code == Some(0);
	let message = if ok {
		let visible = stdout.trim();
		if visible.is_empty() {
			"Python finished successfully with no stdout.".to_string()
		} else {
			visible.to_string()
		}
	} else if !stderr.trim().is_empty() {
		stderr.trim().to_string()
	} else {
		format!("Python exited with status {}.", exit_code.unwrap_or(-1))
	};
	ToolOutputEnvelope::new(
		ok,
		(!ok).then_some("non_zero_exit"),
		false,
		message,
		json!({
			"code": code,
			"exit_code": exit_code,
			"stdout": stdout,
			"stderr": stderr,
			"attachments": attachments,
			"truncated": truncated,
		}),
	)
	.into_value()
}

fn timeout_output(
	code: &str,
	stdout: String,
	stderr: String,
	truncated: bool,
	attachments: Vec<String>,
	timeout_ms: u64,
) -> Value {
	ToolOutputEnvelope::new(
		false,
		Some("tool_timeout"),
		true,
		format!("python.run exceeded its {}ms timeout.", timeout_ms),
		json!({
			"code": code,
			"stdout": stdout,
			"stderr": stderr,
			"attachments": attachments,
			"truncated": truncated,
			"timeout_ms": timeout_ms,
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

fn default_allowed_roots() -> Vec<PathBuf> {
	std::env::current_dir()
		.ok()
		.and_then(|path| path.canonicalize().ok())
		.map(|path| vec![path])
		.unwrap_or_default()
}

fn python_contract(timeout_ms: u64) -> ToolContract {
	let runtime_constraints = RuntimeConstraints {
		timeout_ms,
		max_retries: 0,
		retry_backoff_ms: 0,
		sandbox_profile: SandboxProfile::PythonResearch,
		deterministic_hooks: false,
		allowed_read_roots: default_allowed_roots(),
		allowed_write_roots: Vec::new(),
	};
	ToolContract {
		selection: selection_contract(
			&[
				"Use when the user already provided explicit Python code and clearly asked to run it.",
				"Best for bounded Python snippets whose stdout/stderr facts should be grounded before later explanation.",
			],
			&[
				"Do not use for shell commands, filesystem reads, or natural-language tasks that do not already contain runnable Python.",
				"Do not use when the user only wants an explanation of the Python code rather than execution.",
			],
			&[
				"Commonly confused with command.run for inline code fences that are actually shell commands.",
				"Commonly confused with general.execute for requests that ask to explain code rather than run it.",
			],
		),
		input: input_contract(
			vec![
				input_field(
					"code",
					true,
					"Explicit Python source code to execute in one bounded subprocess.",
					&["Reject when the request does not contain runnable Python code."],
				),
				input_field(
					"timeout_ms",
					false,
					"Optional per-call timeout capped by the configured python.run timeout ceiling.",
					&["Reject when zero or larger than the configured hard timeout ceiling."],
				),
			],
			&[
				"Execution happens in a constrained Python subprocess with no allowed workspace write roots.",
			],
		),
		output: output_contract(
			"Returns grounded subprocess facts such as exit_code, stdout, stderr, attachments, and truncation status.",
			"Successful Python runs with no stdout still return ok=true and a message that stdout was empty.",
			&[
				"Non-zero exits surface as `error_type=non_zero_exit` and remain non-terminal tool observations.",
				"Timeouts surface as `error_type=tool_timeout` and are terminal for the tool contract.",
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
	use roku_common_types::ToolOutputEnvelope;

	fn invocation_request(input: Value) -> ToolInvocationRequest {
		ToolInvocationRequest {
			invocation_key: "test-python-run".to_string(),
			attempt: 1,
			input,
			sandbox_profile: SandboxProfile::PythonResearch,
			attachments: Vec::new(),
			allowed_read_roots: default_allowed_roots(),
			allowed_write_roots: Vec::new(),
		}
	}

	#[test]
	fn descriptor_exposes_unified_contract() {
		let descriptor = PythonRunTool {
			config: PythonToolRuntimeConfig::default(),
		}
		.descriptor();

		assert_eq!(descriptor.output_schema, "tool_observation.v1");
		assert!(descriptor.contract.is_some());
	}

	#[test]
	fn non_zero_exit_returns_tool_output_envelope() {
		let tool = PythonRunTool {
			config: PythonToolRuntimeConfig::default(),
		};
		let output = tool
			.invoke(invocation_request(json!({
				"code": "raise SystemExit(3)"
			})))
			.expect("python.run should return a structured observation");
		let envelope = serde_json::from_value::<ToolOutputEnvelope>(output)
			.expect("python.run should emit ToolOutputEnvelope");

		assert!(!envelope.ok);
		assert_eq!(envelope.error_type.as_deref(), Some("non_zero_exit"));
		assert!(!envelope.terminal);
	}

	#[test]
	fn timeout_returns_terminal_envelope() {
		let tool = PythonRunTool {
			config: PythonToolRuntimeConfig {
				default_timeout_ms: 25,
				max_output_bytes: 8_192,
			},
		};
		let output = tool
			.invoke(invocation_request(json!({
				"code": "import time\ntime.sleep(1)"
			})))
			.expect("python.run timeout should still surface as a structured observation");
		let envelope = serde_json::from_value::<ToolOutputEnvelope>(output)
			.expect("python.run timeout should emit ToolOutputEnvelope");

		assert!(!envelope.ok);
		assert_eq!(envelope.error_type.as_deref(), Some("tool_timeout"));
		assert!(envelope.terminal);
	}
}
