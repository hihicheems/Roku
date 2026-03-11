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

use roku_plugin_catalog::{CatalogDescriptor, ResourceCost, ResourceKind, ResourceRisk};
use roku_plugin_host::{
	RuntimeConstraints, SandboxProfile, Tool, ToolDescriptor, ToolFailure, ToolInvocationRequest,
	ToolRuntime, ToolRuntimeError, ToolSchema,
};
use serde_json::{Value, json};

const DEFAULT_TIMEOUT_MS: u64 = 10_000;
const MAX_OUTPUT_BYTES: usize = 8_192;

pub(crate) fn catalog_descriptors() -> Vec<CatalogDescriptor> {
	vec![CatalogDescriptor {
		selector: roku_common_types::ResourceSelector::tool("python.run"),
		kind: ResourceKind::Tool,
		name: "python.run".to_string(),
		role: Some("core_python".to_string()),
		description:
			"Run explicit Python code through a constrained subprocess and return stdout/stderr."
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
		input_schema: vec!["code".to_string(), "attachments".to_string()],
		risk: ResourceRisk::Medium,
		cost: ResourceCost {
			estimated_tokens: 0,
			estimated_latency_ms: DEFAULT_TIMEOUT_MS,
		},
		required_capabilities: vec!["python.run".to_string()],
		summary: "Execute explicit Python code in a bounded subprocess.".to_string(),
		key_commands: Vec::new(),
		use_cases: Vec::new(),
	}]
}

pub(crate) fn register_tools(runtime: &mut ToolRuntime) -> Result<(), ToolRuntimeError> {
	runtime.register_tool(PythonRunTool)?;
	Ok(())
}

struct PythonRunTool;

impl Tool for PythonRunTool {
	fn descriptor(&self) -> ToolDescriptor {
		ToolDescriptor {
			name: "python.run".to_string(),
			version: "1.0.0".to_string(),
			input_schema: ToolSchema {
				required_fields: vec![
					"task_id".to_string(),
					"node_id".to_string(),
					"goal".to_string(),
					"summary".to_string(),
					"conversation_history".to_string(),
					"budget_tokens".to_string(),
					"time_budget_ms".to_string(),
					"code".to_string(),
				],
			},
			output_schema: "result.v1".to_string(),
			required_capabilities: vec!["python.run".to_string()],
			runtime_constraints: RuntimeConstraints {
				timeout_ms: DEFAULT_TIMEOUT_MS,
				max_retries: 0,
				retry_backoff_ms: 0,
				sandbox_profile: SandboxProfile::PythonResearch,
				deterministic_hooks: false,
				allowed_read_roots: default_allowed_roots(),
				allowed_write_roots: Vec::new(),
			},
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
			.unwrap_or(DEFAULT_TIMEOUT_MS)
			.min(DEFAULT_TIMEOUT_MS);
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
				let mut stdout = String::new();
				let mut stderr = String::new();
				if let Some(handle) = child.stdout.take() {
					let _ = handle
						.take(u64::try_from(MAX_OUTPUT_BYTES).unwrap_or(u64::MAX))
						.read_to_string(&mut stdout);
				}
				if let Some(handle) = child.stderr.take() {
					let _ = handle
						.take(u64::try_from(MAX_OUTPUT_BYTES).unwrap_or(u64::MAX))
						.read_to_string(&mut stderr);
				}
				let truncated =
					stdout.len() >= MAX_OUTPUT_BYTES || stderr.len() >= MAX_OUTPUT_BYTES;
				let message = if status.success() {
					let visible = stdout.trim();
					if visible.is_empty() {
						"Python finished successfully with no stdout.".to_string()
					} else {
						visible.to_string()
					}
				} else if !stderr.trim().is_empty() {
					format!(
						"Python exited with status {}: {}",
						status.code().unwrap_or(-1),
						stderr.trim()
					)
				} else {
					format!("Python exited with status {}.", status.code().unwrap_or(-1))
				};
				return Ok(json!({
					"message": message,
					"exit_code": status.code(),
					"stdout": stdout,
					"stderr": stderr,
					"attachments": request.attachments.iter().map(|path| path.display().to_string()).collect::<Vec<_>>(),
					"truncated": truncated,
				}));
			}
			if started.elapsed() >= timeout {
				let _ = child.kill();
				let _ = child.wait();
				return Err(ToolFailure::terminal(format!(
					"python.run exceeded its {}ms timeout",
					timeout_ms
				)));
			}
			thread::sleep(Duration::from_millis(10));
		}
	}
}

fn default_allowed_roots() -> Vec<PathBuf> {
	std::env::current_dir()
		.ok()
		.and_then(|path| path.canonicalize().ok())
		.map(|path| vec![path])
		.unwrap_or_default()
}
