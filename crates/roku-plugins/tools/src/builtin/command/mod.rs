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

mod execute;
mod policy_bridge;
mod prepare;

use crate::contract::{
	contract_input_schema, contract_tool_schema, grounding_contract_simple, input_contract,
	input_field, output_contract, runtime_contract, selection_contract,
};
use crate::runtime_config::CommandToolRuntimeConfig;
use prepare::{PrepareCommandOutcome, prepare_command};
use roku_common_types::{
	CanonicalExecution, ExtractionHint, GroundingStrategy, PolicyDecision, ToolContract,
	ToolRetryPolicy, ToolSideEffectPolicy,
};
use roku_common_types::{CatalogDescriptor, ResourceCost, ResourceKind, ResourceRisk};
use roku_plugin_host::{
	RuntimeConstraints, SandboxProfile, Tool, ToolDescriptor, ToolRuntime, ToolRuntimeError,
};
use serde_json::Value;
use std::path::PathBuf;

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
		description: "Execute a shell command and return its output (exit code, stdout, stderr). This is your general-purpose tool for running any CLI program — use it freely for `gh`, `git`, `cargo`, `docker`, `kubectl`, `jq`, `curl`, `make`, or any other available command. Prefer specialized tools (`fs.read_text`, `fs.grep`, `fs.list_dir`, `web.fetch`) only when they directly cover the operation; otherwise default to `command.run`. Do not chain multiple commands with `&&` or `|` — run one command per call."
			.to_string(),
		selection_hint: "Run any shell command. Use this as the default when no specialized tool fits: git operations, package managers, build commands, system commands. Also use for `gh repo view`, `curl`, etc."
			.to_string(),
		discoverable: true,
		tags: vec![
			"command".to_string(),
			"execution".to_string(),
			"probe".to_string(),
		],
		examples: vec![
			"Run this command: `pwd`".to_string(),
			"Check the current git branch".to_string(),
			"Show me PR #116 details".to_string(),
		],
		input_schema: contract_input_schema(
			Some(&contract),
			&["command".to_string(), "cwd".to_string()],
		),
		risk: ResourceRisk::Medium,
		cost: ResourceCost {
			estimated_tokens: 0,
			estimated_latency_ms: config.default_timeout_ms,
		},
		required_capabilities: vec!["command.run".to_string()],
		summary: "Run a shell command and return its output. Default tool for any CLI operation."
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

pub(crate) fn canonical_execution_from_runtime_input(input: &Value) -> Option<CanonicalExecution> {
	let request = roku_plugin_host::ToolInvocationRequest {
		invocation_key: "command.run:agent-runtime-canonicalization".to_string(),
		attempt: 1,
		input: input.clone(),
		sandbox_profile: SandboxProfile::ReadOnlyFs,
		attachments: Vec::new(),
		allowed_read_roots: default_allowed_roots(),
		allowed_write_roots: Vec::new(),
	};

	match prepare_command(&request, &CommandToolRuntimeConfig::default()).ok()? {
		PrepareCommandOutcome::Ready(prepared) => Some(prepared.execution),
		PrepareCommandOutcome::Rejected(output) => output
			.get("data")
			.and_then(|value| value.get("canonical_execution"))
			.cloned()
			.and_then(|value| serde_json::from_value(value).ok()),
	}
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

	fn invoke(
		&self,
		request: roku_plugin_host::ToolInvocationRequest,
	) -> Result<serde_json::Value, roku_plugin_host::ToolFailure> {
		match prepare_command(&request, &self.config)? {
			PrepareCommandOutcome::Ready(prepared) => {
				execute::execute_prepared_command(&prepared, self.config.max_output_bytes)
			}
			PrepareCommandOutcome::Rejected(output) => Ok(output),
		}
	}

	fn policy_decision(&self, execution: &CanonicalExecution) -> Option<PolicyDecision> {
		(execution.tool_name == "command.run")
			.then(|| policy_bridge::evaluate_command_policy(execution))
	}
}

pub(super) fn default_allowed_roots() -> Vec<PathBuf> {
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
				"Use as a general-purpose shell fallback when no specialized tool covers the operation.",
				"Best for CLI tools such as gh, git, cargo, docker, kubectl, jq, make, and bounded read-oriented commands.",
				"You may generate the command yourself based on the task — it does not need to be literally present in the user message.",
			],
			&[
				"Do not use for multi-step scripts, chained shell expressions, or commands with shell metacharacters.",
				"Do not use when the user asks to explain a command without executing it.",
				"Prefer specialized tools (fs.read_text, fs.grep, fs.list_dir, web.fetch) when they cover the operation.",
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
					&[
						"Require approval when the directory resolves outside the allowed workspace roots.",
					],
				),
				input_field(
					"timeout_ms",
					false,
					"Optional per-call timeout capped by the configured command.run default timeout.",
					&["Reject when zero or larger than the configured hard timeout ceiling."],
				),
			],
			&[
				"Execution stays inside read-only workspace roots unless an invocation-scoped approval widens access for one exact frozen command.",
			],
		),
		output: output_contract(
			"Returns grounded subprocess facts such as argv, cwd, exit code, stdout, stderr, truncation, and scope_root.",
			"Successful commands with no stdout still return ok=true and a message explaining that stdout was empty.",
			&[
				"Unsupported commands, unsafe shell syntax, out-of-scope paths, and timeouts surface as explicit error_type values or approval-required policy payloads.",
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
		grounding: grounding_contract_simple(
			GroundingStrategy::CommandBased,
			&["command"],
			Some("command"),
			false,
			ExtractionHint::ShellCommand,
		),
	}
}

#[cfg(test)]
mod tests {
	use serde_json::json;

	use super::canonical_execution_from_runtime_input;

	#[test]
	fn canonical_execution_from_runtime_input_preserves_untrusted_command_execution() {
		let execution = canonical_execution_from_runtime_input(&json!({
			"command": "just lint"
		}))
		.expect("untrusted commands should still project canonical execution for policy gating");

		assert_eq!(execution.tool_name, "command.run");
		assert_eq!(execution.program, "just");
		assert_eq!(execution.argv, vec!["just".to_string(), "lint".to_string()]);
	}
}
