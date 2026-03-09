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

use roku_common_types::{
	AgentInstanceSpec, ConversationRole, ConversationTurn, ResultEnvelope, TaskNode,
};
use roku_tool_runtime::{ToolInvocation, ToolRuntime};
use serde_json::json;

use crate::result::{tool_failure_result, tool_success_result};
use crate::runtime::RuntimeWorker;
use crate::tools::{
	DATA_TOOL_NAME, GENERAL_TOOL_NAME, RESEARCH_TOOL_NAME, REVIEW_TOOL_NAME, SKILL_TOOL_NAME,
};

pub(crate) struct ToolBackedWorker {
	worker_id: &'static str,
	tool_name: &'static str,
	capability_prefixes: &'static [&'static str],
	tool_runtime: Arc<ToolRuntime>,
	confidence: f32,
}

impl ToolBackedWorker {
	fn new(
		worker_id: &'static str,
		tool_name: &'static str,
		capability_prefixes: &'static [&'static str],
		tool_runtime: Arc<ToolRuntime>,
		confidence: f32,
	) -> Self {
		Self {
			worker_id,
			tool_name,
			capability_prefixes,
			tool_runtime,
			confidence,
		}
	}

	fn invocation(&self, spec: &AgentInstanceSpec, node: &TaskNode) -> ToolInvocation {
		let (goal, step_summary) = goal_and_step(&node.description);
		ToolInvocation {
			tool_name: self.tool_name.to_string(),
			input: json!({
				"task_id": spec.context.task_id.0,
				"node_id": node.node_id.0,
				"goal": goal,
				"summary": step_summary,
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
				self.tool_name,
				execution,
				self.confidence,
			),
			Err(error) => tool_failure_result(spec, node, self.worker_id, self.tool_name, error),
		}
	}
}

pub(crate) fn research_worker(tool_runtime: Arc<ToolRuntime>) -> ToolBackedWorker {
	ToolBackedWorker::new(
		"research-worker",
		RESEARCH_TOOL_NAME,
		&["information.", "research."],
		tool_runtime,
		0.86,
	)
}

pub(crate) fn data_worker(tool_runtime: Arc<ToolRuntime>) -> ToolBackedWorker {
	ToolBackedWorker::new(
		"data-worker",
		DATA_TOOL_NAME,
		&["data."],
		tool_runtime,
		0.88,
	)
}

pub(crate) fn review_worker(tool_runtime: Arc<ToolRuntime>) -> ToolBackedWorker {
	ToolBackedWorker::new(
		"review-worker",
		REVIEW_TOOL_NAME,
		&["review.", "validation."],
		tool_runtime,
		0.92,
	)
}

pub(crate) fn skill_worker(tool_runtime: Arc<ToolRuntime>) -> ToolBackedWorker {
	ToolBackedWorker::new(
		"skill-worker",
		SKILL_TOOL_NAME,
		&["skill."],
		tool_runtime,
		0.94,
	)
}

pub(crate) fn generic_worker(tool_runtime: Arc<ToolRuntime>) -> ToolBackedWorker {
	ToolBackedWorker::new("generic-worker", GENERAL_TOOL_NAME, &[], tool_runtime, 0.75)
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
