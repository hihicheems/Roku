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

use std::sync::Arc;

use crate::runtime_loop::ground_tool_arguments;
use crate::tool_config::{BuiltinToolRole, ToolCatalogConfig};
use roku_common_types::{
	AgentInstanceSpec, ConversationRole, ConversationTurn, ResultEnvelope, TaskNode,
};
use roku_plugin_host::{ToolInvocation, ToolRuntime};
use roku_plugin_tools::canonical_execution_for_builtin_tool_input;
use serde_json::json;

use crate::result::{tool_failure_result, tool_success_result};
use crate::runtime::RuntimeWorker;

pub(crate) struct ToolBackedWorker {
	worker_id: &'static str,
	tool_name: String,
	capability_prefixes: &'static [&'static str],
	tool_runtime: Arc<ToolRuntime>,
	confidence: f32,
}

impl ToolBackedWorker {
	fn new(
		worker_id: &'static str,
		tool_name: impl Into<String>,
		capability_prefixes: &'static [&'static str],
		tool_runtime: Arc<ToolRuntime>,
		confidence: f32,
	) -> Self {
		Self {
			worker_id,
			tool_name: tool_name.into(),
			capability_prefixes,
			tool_runtime,
			confidence,
		}
	}

	fn invocation(&self, spec: &AgentInstanceSpec, node: &TaskNode) -> ToolInvocation {
		let (goal, step_summary) = goal_and_step(&node.description);
		let mut input = json!({
			"task_id": spec.context.task_id.0,
			"node_id": node.node_id.0,
			"goal": goal,
			"summary": step_summary,
			"granted_capabilities": spec.capabilities.clone(),
			"resource_selectors": spec
				.context
				.resources
				.iter()
				.map(|resource| resource.display_key())
				.collect::<Vec<_>>(),
			"conversation_history": render_conversation_history(&spec.context.conversation_history),
			"runtime_memory_sections": spec.context.runtime_memory_sections.clone(),
			"budget_tokens": spec.policy_bindings.budget_tokens,
			"time_budget_ms": spec.policy_bindings.time_budget_ms,
			"worker_id": self.worker_id,
		});
		if let Some(arguments) = ground_tool_arguments(&self.tool_name, &goal)
			.or_else(|| ground_tool_arguments(&self.tool_name, &step_summary))
			.and_then(|value| value.as_object().cloned())
		{
			for (key, value) in arguments {
				input[key] = value;
			}
		}
		let canonical_execution =
			canonical_execution_for_builtin_tool_input(&self.tool_name, &input);
		ToolInvocation {
			tool_name: self.tool_name.clone(),
			input,
			canonical_execution,
			approved_scope: None,
			skip_policy_check: false,
			granted_capabilities: spec.capabilities.clone(),
			invocation_key: Some(format!(
				"{}:{}:{}",
				spec.context.task_id.0, node.node_id.0, self.worker_id
			)),
			attachments: Vec::new(),
		}
	}
}

impl RuntimeWorker for ToolBackedWorker {
	fn worker_id(&self) -> &'static str {
		self.worker_id
	}

	fn supports(&self, capabilities: &[String]) -> bool {
		if self.capability_prefixes.is_empty() {
			return true;
		}

		capabilities.iter().any(|capability| {
			self.capability_prefixes
				.iter()
				.any(|prefix| capability.starts_with(prefix))
		})
	}

	fn execute(&self, spec: &AgentInstanceSpec, node: &TaskNode) -> ResultEnvelope {
		let invocation = self.invocation(spec, node);
		let canonical_execution = invocation.canonical_execution.clone();
		let tool_input = invocation.input.clone();
		match self.tool_runtime.invoke(invocation) {
			Ok(execution) => tool_success_result(
				spec,
				node,
				self.worker_id,
				&self.tool_name,
				execution,
				self.confidence,
			),
			Err(error) => tool_failure_result(
				spec,
				node,
				self.worker_id,
				&self.tool_name,
				Some(tool_input),
				canonical_execution,
				error,
			),
		}
	}
}

pub(crate) fn skill_worker_with_config(
	tool_runtime: Arc<ToolRuntime>,
	tool_config: &ToolCatalogConfig,
) -> ToolBackedWorker {
	ToolBackedWorker::new(
		"skill-worker",
		tool_name_for_role(tool_config, BuiltinToolRole::SkillInstall),
		&["skill."],
		tool_runtime,
		0.94,
	)
}

pub(crate) fn skill_execute_worker_with_config(
	tool_runtime: Arc<ToolRuntime>,
	tool_config: &ToolCatalogConfig,
) -> ToolBackedWorker {
	ToolBackedWorker::new(
		"skill-execute-worker",
		tool_name_for_role(tool_config, BuiltinToolRole::SkillExecute),
		&["skill.execute"],
		tool_runtime,
		0.97,
	)
}

// generic_worker_with_config removed — general.execute is no longer registered.

fn goal_and_step(description: &str) -> (String, String) {
	if let Some(stripped) = description.strip_prefix("Goal: ")
		&& let Some((goal, step)) = stripped.split_once("\nStep: ")
	{
		return (goal.to_string(), step.to_string());
	}

	(String::new(), description.to_string())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn canonical_execution_for_command_run_invocation_includes_digest() {
		let input = json!({
			"command": "pwd",
		});

		let execution = canonical_execution_for_builtin_tool_input("command.run", &input)
			.expect("command.run canonical execution should build");

		assert_eq!(execution.tool_name, "command.run");
		assert_eq!(execution.program, "pwd");
		assert_eq!(execution.argv, vec!["pwd".to_string()]);
		assert_eq!(execution.digest.0.len(), 64);
	}
}

fn render_conversation_history(history: &[ConversationTurn]) -> String {
	history
		.iter()
		.map(|turn| format!("{}: {}", role_label(turn.role), turn.content))
		.collect::<Vec<_>>()
		.join("\n")
}

fn role_label(role: ConversationRole) -> &'static str {
	match role {
		ConversationRole::User => "user",
		ConversationRole::Assistant => "assistant",
		ConversationRole::System => "system",
	}
}

fn tool_name_for_role(tool_config: &ToolCatalogConfig, role: BuiltinToolRole) -> String {
	tool_config
		.tool_for_role(role)
		.map(|tool| tool.name.clone())
		.unwrap_or_else(|| role.as_str().to_string())
}

#[cfg(test)]
mod test_workers {
	use super::*;
	use std::sync::Arc;

	use roku_common_types::{
		AgentContext, AgentInstanceSpec, NodeId, PolicyBindings, RuntimeMemorySections, TaskId,
		TaskNode,
	};
	use roku_plugin_host::ToolRuntime;

	#[test]
	fn worker_invocation_sends_runtime_memory_sections_without_legacy_blob() {
		let tool_runtime = Arc::new(ToolRuntime::default());
		let worker = ToolBackedWorker::new("test-worker", "tool", &[], tool_runtime, 0.5);
		let spec = AgentInstanceSpec {
			instance_id: "agent-1".to_string(),
			context: AgentContext {
				task_id: TaskId("task-1".to_string()),
				node_id: NodeId("node-1".to_string()),
				summary: "summary".to_string(),
				resources: Vec::new(),
				conversation_history: Vec::new(),
				runtime_memory_sections: RuntimeMemorySections {
					short_term_continuity: "user: hi".to_string(),
					long_term_recall: "memory-record-1 | UserPreference | Rust".to_string(),
					working_memory: "remember runtime seam".to_string(),
				},
			},
			capabilities: Vec::new(),
			capability_tokens: Vec::new(),
			policy_bindings: PolicyBindings {
				budget_tokens: 1,
				time_budget_ms: 1,
			},
		};
		let node = TaskNode::default();
		let invocation = worker.invocation(&spec, &node);
		assert!(
			invocation.input.get("memory_context").is_none(),
			"legacy memory_context key should no longer be sent"
		);
		assert_eq!(
			invocation.input["runtime_memory_sections"]["working_memory"].as_str(),
			Some("remember runtime seam")
		);
	}
}
