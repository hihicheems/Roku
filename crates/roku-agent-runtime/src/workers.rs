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

use crate::tool_config::{BuiltinToolRole, ToolCatalogConfig};
use roku_common_types::{
	AgentInstanceSpec, ConversationRole, ConversationTurn, ResultEnvelope, TaskNode,
};
use roku_tool_runtime::{ToolInvocation, ToolRuntime};
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
		ToolInvocation {
			tool_name: self.tool_name.clone(),
			input: json!({
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
				"budget_tokens": spec.policy_bindings.budget_tokens,
				"time_budget_ms": spec.policy_bindings.time_budget_ms,
				"worker_id": self.worker_id,
			}),
			granted_capabilities: spec.capabilities.clone(),
			invocation_key: Some(format!(
				"{}:{}:{}",
				spec.context.task_id.0, node.node_id.0, self.worker_id
			)),
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
		match self.tool_runtime.invoke(self.invocation(spec, node)) {
			Ok(execution) => tool_success_result(
				spec,
				node,
				self.worker_id,
				&self.tool_name,
				execution,
				self.confidence,
			),
			Err(error) => tool_failure_result(spec, node, self.worker_id, &self.tool_name, error),
		}
	}
}

pub(crate) fn research_worker_with_config(
	tool_runtime: Arc<ToolRuntime>,
	tool_config: &ToolCatalogConfig,
) -> ToolBackedWorker {
	ToolBackedWorker::new(
		"research-worker",
		tool_name_for_role(tool_config, BuiltinToolRole::Research),
		&["information.", "research."],
		tool_runtime,
		0.86,
	)
}

pub(crate) fn inventory_worker_with_config(
	tool_runtime: Arc<ToolRuntime>,
	tool_config: &ToolCatalogConfig,
) -> ToolBackedWorker {
	ToolBackedWorker::new(
		"inventory-worker",
		tool_name_for_role(tool_config, BuiltinToolRole::Inventory),
		&["inventory."],
		tool_runtime,
		0.84,
	)
}

pub(crate) fn data_worker_with_config(
	tool_runtime: Arc<ToolRuntime>,
	tool_config: &ToolCatalogConfig,
) -> ToolBackedWorker {
	ToolBackedWorker::new(
		"data-worker",
		tool_name_for_role(tool_config, BuiltinToolRole::Data),
		&["data."],
		tool_runtime,
		0.88,
	)
}

pub(crate) fn review_worker_with_config(
	tool_runtime: Arc<ToolRuntime>,
	tool_config: &ToolCatalogConfig,
) -> ToolBackedWorker {
	ToolBackedWorker::new(
		"review-worker",
		tool_name_for_role(tool_config, BuiltinToolRole::Review),
		&["review.", "validation."],
		tool_runtime,
		0.92,
	)
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

pub(crate) fn generic_worker_with_config(
	tool_runtime: Arc<ToolRuntime>,
	tool_config: &ToolCatalogConfig,
) -> ToolBackedWorker {
	ToolBackedWorker::new(
		"generic-worker",
		tool_name_for_role(tool_config, BuiltinToolRole::General),
		&[],
		tool_runtime,
		0.75,
	)
}

fn goal_and_step(description: &str) -> (String, String) {
	if let Some(stripped) = description.strip_prefix("Goal: ")
		&& let Some((goal, step)) = stripped.split_once("\nStep: ")
	{
		return (goal.to_string(), step.to_string());
	}

	(String::new(), description.to_string())
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
