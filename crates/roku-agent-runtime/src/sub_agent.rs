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

//! Sub-agent execution: spawning, budget allocation, and result truncation.
//!
//! Extracted from `runtime.rs` — the `execute_sub_agent` method is a thin
//! wrapper around the tool loop with independent message history and budget.

use roku_common_types::{RequestEnvelope, ResultStatus, RuntimeMemorySections, TaskId};
use serde_json::Value;

use crate::runtime_loop::LoopState;

/// Maximum character count for sub-agent result content.
const MAX_SUB_AGENT_RESULT_CHARS: usize = 4000;

/// Configuration for sub-agent execution.
pub(crate) struct SubAgentConfig {
	/// Maximum steps the sub-agent may use (capped at parent's remaining budget).
	pub max_steps: u32,
	/// Tools the sub-agent is forbidden from using.
	pub disallowed_tools: Vec<String>,
	/// Timeout in seconds (0 = no timeout).
	pub timeout_secs: u64,
}

impl Default for SubAgentConfig {
	fn default() -> Self {
		Self {
			max_steps: 10,
			disallowed_tools: vec![
				roku_plugin_tools::PSEUDO_AGENT.to_string(),
				roku_plugin_tools::PSEUDO_ASK_USER.to_string(),
			],
			timeout_secs: 120,
		}
	}
}

/// Execute a sub-agent for an `Agent` tool call.
///
/// The sub-agent runs with a fresh message history and an independent budget drawn
/// from the parent loop's remaining budget. The parent budget is deducted **before**
/// this function is called (caller responsibility, Class G guard).
///
/// Recursion is blocked at depth 1: a sub-agent cannot spawn further sub-agents.
pub(crate) async fn execute_sub_agent(
	runtime: &crate::runtime::GenericAgentRuntime,
	parent_task_id: &TaskId,
	parent_request: &RequestEnvelope,
	parent_loop_state: &mut LoopState,
	arguments: &Value,
	runtime_memory_sections: &RuntimeMemorySections,
	event_sender: Option<&crate::runtime_loop::LoopEventSender>,
	approval_gate: Option<&dyn crate::runtime_loop::approval::ToolApprovalGate>,
	config: &SubAgentConfig,
) -> (String, bool) {
	// Recursion guard: sub-agents cannot spawn sub-sub-agents.
	if parent_loop_state.sub_agent_depth >= 1 {
		return (
			"[Sub-agent error] Sub-agents cannot spawn further sub-agents.".to_string(),
			true,
		);
	}

	let task = arguments
		.get("task")
		.and_then(Value::as_str)
		.unwrap_or("")
		.trim()
		.to_string();
	if task.is_empty() {
		return ("[Sub-agent error] No task provided.".to_string(), true);
	}

	// Allocate budget for the sub-agent from the parent's remaining budget.
	let sub_budget = parent_loop_state
		.remaining_step_budget
		.min(config.max_steps);
	parent_loop_state.remaining_step_budget = parent_loop_state
		.remaining_step_budget
		.saturating_sub(sub_budget);

	// Filter out disallowed tools from parent's visible tools.
	let sub_visible_tools: Vec<String> = parent_loop_state
		.visible_tools
		.iter()
		.filter(|t| !config.disallowed_tools.iter().any(|d| d == *t))
		.cloned()
		.collect();

	// Build a fresh sub-request with independent message history.
	let sub_request = RequestEnvelope {
		request_id: roku_common_types::RequestId(format!("{}-sub", parent_request.request_id.0)),
		session_id: parent_request.session_id.clone(),
		goal: task.clone(),
		planning_mode_hint: None,
		conversation_history: Vec::new(),
		model_override: parent_request.model_override.clone(),
		thinking_effort: parent_request.thinking_effort.clone(),
	};

	// Build a minimal LoopContext for the sub-agent.
	let sub_route_decision = parent_loop_state.route_decision.clone();
	let sub_context = crate::runtime_loop::LoopContext {
		request_id: sub_request.request_id.0.clone(),
		session_id: sub_request.session_id.clone(),
		goal: task.clone(),
		workspace_root: parent_loop_state.working_directory.clone(),
		working_directory: parent_loop_state.working_directory.clone(),
		visible_tools: sub_visible_tools,
		bound_resources: parent_loop_state.bound_resources.clone(),
		route_decision: sub_route_decision,
		last_observation: None,
	};

	let recovery_budget = runtime
		.agent_runtime_config()
		.r#loop
		.initial_recovery_budget;
	let mut sub_loop_state = LoopState::with_budgets(
		format!("sub-{}", sub_request.request_id.0),
		&sub_context,
		sub_budget,
		recovery_budget,
	);
	sub_loop_state.sub_agent_depth = parent_loop_state.sub_agent_depth + 1;
	sub_loop_state.disallowed_tools = config.disallowed_tools.clone();

	// Run the sub-agent with an optional timeout.
	let result = if config.timeout_secs > 0 {
		let timeout = std::time::Duration::from_secs(config.timeout_secs);
		match tokio::time::timeout(
			timeout,
			Box::pin(runtime.execute_tool_loop(
				parent_task_id,
				&sub_request,
				&mut sub_loop_state,
				runtime_memory_sections,
				None,
				event_sender,
				approval_gate,
			)),
		)
		.await
		{
			Ok(result) => result,
			Err(_) => {
				return (
					format!(
						"[Sub-agent error] Timed out after {} seconds.",
						config.timeout_secs
					),
					true,
				);
			}
		}
	} else {
		Box::pin(runtime.execute_tool_loop(
			parent_task_id,
			&sub_request,
			&mut sub_loop_state,
			runtime_memory_sections,
			None,
			event_sender,
			approval_gate,
		))
		.await
	};

	// Determine error status from the structured result.
	let is_error = result.result.status == ResultStatus::Error;

	// Truncate to avoid bloating the parent's context window.
	let content = truncate_sub_agent_result(result.message);
	(content, is_error)
}

/// Truncate sub-agent result content to `MAX_SUB_AGENT_RESULT_CHARS`.
fn truncate_sub_agent_result(message: String) -> String {
	let char_count = message.chars().count();
	if char_count > MAX_SUB_AGENT_RESULT_CHARS {
		let byte_end = message
			.char_indices()
			.nth(MAX_SUB_AGENT_RESULT_CHARS)
			.map(|(i, _)| i)
			.unwrap_or(message.len());
		format!(
			"{}...\n[Sub-agent response truncated to {} chars]",
			&message[..byte_end],
			MAX_SUB_AGENT_RESULT_CHARS
		)
	} else {
		message
	}
}
