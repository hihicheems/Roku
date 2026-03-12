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

use std::fs;
use std::path::{Path, PathBuf};

use roku_plugin_llm::{GenerationRequest, LlmRouter, RiskTier};
use serde_json::{Value, json};

use crate::runtime_loop::grounding::{extract_path_candidates, reply_selects_candidate};
use crate::runtime_loop::{
	NextStepAction, NextStepDecision, ToolObservation, ask_user::ask_user_from_observation,
	summarizer::summarize_observation,
};

pub(crate) fn decide_filesystem_next_step(
	loop_state: &crate::runtime_loop::LoopState,
	router: Option<&LlmRouter>,
	user_reply: Option<&str>,
) -> NextStepDecision {
	if loop_state.last_observation.is_some() {
		return deterministic_next_step(loop_state, user_reply);
	}
	let grounding_input = user_reply.unwrap_or(&loop_state.goal);
	let hints = GoalHints::from_goal(grounding_input);
	if hints.basename_only.is_some() && tool_visible(loop_state, "fs.find") {
		return initial_next_step(loop_state, &hints);
	}
	if let Some(router) = router
		&& let Some(decision) = decide_with_router(loop_state, router, user_reply)
	{
		return decision;
	}
	deterministic_next_step(loop_state, user_reply)
}

pub(crate) fn next_working_directory_from_observation(
	observation: &ToolObservation,
	current_working_directory: &str,
) -> Option<String> {
	if observation.ok && observation.tool_name == "fs.inspect" {
		let is_directory = observation
			.data
			.get("kind")
			.and_then(Value::as_str)
			.is_some_and(|kind| kind == "directory");
		if is_directory {
			return observation
				.data
				.get("path")
				.and_then(Value::as_str)
				.filter(|path| !path.is_empty() && *path != current_working_directory)
				.map(str::to_string);
		}
	}
	None
}

fn decide_with_router(
	loop_state: &crate::runtime_loop::LoopState,
	router: &LlmRouter,
	user_reply: Option<&str>,
) -> Option<NextStepDecision> {
	let response = router
		.generate_json_value(&GenerationRequest {
			system_prompt: Some(
				"You are Roku's filesystem loop next-step decision model. Return only valid JSON."
					.to_string(),
			),
			prompt: filesystem_next_step_prompt(loop_state, user_reply),
			expected_output_tokens: 220,
			risk_tier: RiskTier::Low,
			preferred_provider: None,
			budget_tokens_remaining: 1_500,
			budget_cost_remaining_usd: 0.05,
		})
		.ok()?;
	let decision = NextStepDecision::from_json_value(&response.value).ok()?;
	validate_router_decision(loop_state, decision)
}

fn validate_router_decision(
	loop_state: &crate::runtime_loop::LoopState,
	decision: NextStepDecision,
) -> Option<NextStepDecision> {
	match decision.action {
		NextStepAction::CallTool => {
			let tool_name = decision.tool_name.as_deref()?;
			if !loop_state
				.visible_tools
				.iter()
				.any(|visible| visible == tool_name)
			{
				return None;
			}
			let arguments = decision.arguments.as_ref()?.as_object()?;
			filesystem_required_argument_keys(tool_name)
				.iter()
				.all(|key| arguments.contains_key(*key))
				.then_some(decision)
		}
		NextStepAction::AskUser | NextStepAction::FinalAnswer | NextStepAction::Fail => {
			Some(decision)
		}
	}
}

fn filesystem_next_step_prompt(
	loop_state: &crate::runtime_loop::LoopState,
	user_reply: Option<&str>,
) -> String {
	format!(
		r#"Return only JSON with exactly these keys:
{{
  "action": "call_tool | ask_user | final_answer | fail",
  "tool_name": "visible tool name or null",
  "arguments": {{ }},
  "reason": "short explanation",
  "final_message": "message or null"
}}

Rules:
- Only use a tool from `visible_tools`.
- `call_tool` is the only action that may set `tool_name`.
- `call_tool` should not use `final_message`; if you include it anyway, the runtime will ignore it.
- Every `call_tool` decision must include all required argument keys for the selected tool.
- Prefer `fs.find` before `fs.read_text` or `fs.exists` when the user only gave a basename and no explicit path.
- Use `ask_user` when the last observation reports multiple candidates.
- Use `final_answer` after a successful filesystem observation that already satisfies the user request.
- Use `final_answer` for user-facing negative results like path_not_found or workspace_violation.

Visible tools:
{visible_tools}

Tool requirements:
{tool_requirements}

Current working directory:
{cwd}

Goal:
{goal}

Route decision:
{route_decision}

Last observation:
{last_observation}

Current user follow-up:
{user_reply}"#,
		visible_tools = serde_json::to_string_pretty(&loop_state.visible_tools)
			.unwrap_or_else(|_| "[]".to_string()),
		tool_requirements =
			serde_json::to_string_pretty(&filesystem_tool_requirements(&loop_state.visible_tools))
				.unwrap_or_else(|_| "{}".to_string()),
		cwd = loop_state.working_directory,
		goal = loop_state.goal,
		route_decision = serde_json::to_string_pretty(&loop_state.route_decision)
			.unwrap_or_else(|_| "{}".to_string()),
		last_observation = loop_state
			.last_observation
			.as_ref()
			.map(|observation| serde_json::to_string_pretty(observation).unwrap_or_default())
			.unwrap_or_else(|| "null".to_string()),
		user_reply = user_reply.unwrap_or("null"),
	)
}

fn deterministic_next_step(
	loop_state: &crate::runtime_loop::LoopState,
	user_reply: Option<&str>,
) -> NextStepDecision {
	let grounding_input = user_reply.unwrap_or(&loop_state.goal);
	let hints = GoalHints::from_goal(grounding_input);
	if let Some(observation) = loop_state.last_observation.as_ref() {
		return next_step_from_observation(loop_state, &hints, observation, user_reply);
	}
	initial_next_step(loop_state, &hints)
}

fn initial_next_step(
	loop_state: &crate::runtime_loop::LoopState,
	hints: &GoalHints,
) -> NextStepDecision {
	let preferred_tool = preferred_tool(loop_state);
	if let Some(basename) = hints.basename_only.as_deref()
		&& tool_visible(loop_state, "fs.find")
		&& preferred_tool.is_some_and(|tool| tool != "fs.list_dir")
	{
		return call_tool(
			"fs.find",
			json!({
				"name": basename,
				"kind": find_kind_for_preferred_tool(preferred_tool),
			}),
			"Resolve the basename first before attempting a concrete filesystem action.",
		);
	}

	match preferred_tool {
		Some("fs.list_dir") => call_tool(
			"fs.list_dir",
			json!({ "path": hints.explicit_path.as_deref().unwrap_or(".") }),
			"Use the shortlisted directory listing tool for the current grounded target.",
		),
		Some("fs.inspect") => call_tool(
			"fs.inspect",
			json!({ "path": hints.explicit_path.as_deref().unwrap_or(".") }),
			"Use the shortlisted inspection tool to inspect the current grounded target.",
		),
		Some("fs.exists") => {
			if let Some(path) = hints.explicit_path.as_deref() {
				call_tool(
					"fs.exists",
					json!({ "path": path }),
					"Check whether the grounded target exists.",
				)
			} else {
				ask_user("I still need a concrete path or file name before I can check whether it exists.".to_string())
			}
		}
		Some("fs.read_text") => {
			if let Some(path) = hints.explicit_path.as_deref() {
				call_tool(
					"fs.read_text",
					json!({ "path": path, "max_bytes": 4_096_u64 }),
					"Read the grounded file directly.",
				)
			} else if let Some(basename) = hints.basename_only.as_deref() {
				if tool_visible(loop_state, "fs.find") {
					call_tool(
						"fs.find",
						json!({ "name": basename, "kind": "file" }),
						"Resolve the basename first before reading file contents.",
					)
				} else {
					ask_user(format!(
						"I need a more specific path for `{basename}` before I can read it."
					))
				}
			} else {
				ask_user("I still need a concrete file path before I can read it.".to_string())
			}
		}
		Some("fs.find") => {
			if let Some(basename) = hints.basename_only.as_deref() {
				call_tool(
					"fs.find",
					json!({ "name": basename, "kind": "any" }),
					"Resolve the basename inside the allowed workspace roots first.",
				)
			} else {
				ask_user("I still need a file or directory name to locate.".to_string())
			}
		}
		Some(other) => fail(format!(
			"filesystem loop does not support the shortlisted tool `{other}`"
		)),
		None => fail("filesystem loop does not have any visible tools".to_string()),
	}
}

fn next_step_from_observation(
	loop_state: &crate::runtime_loop::LoopState,
	hints: &GoalHints,
	observation: &ToolObservation,
	user_reply: Option<&str>,
) -> NextStepDecision {
	if observation.ok {
		match observation.tool_name.as_str() {
			"fs.find" => {
				if let Some(resolved_path) = observation
					.data
					.get("resolved_path")
					.and_then(Value::as_str)
				{
					let follow_up_tool =
						follow_up_tool_for_resolved_path(loop_state, resolved_path);
					match follow_up_tool {
						Some("fs.read_text") => {
							return call_tool(
								"fs.read_text",
								json!({ "path": resolved_path, "max_bytes": 4_096_u64 }),
								"fs.find returned a unique candidate; read the resolved file path.",
							);
						}
						Some("fs.exists") => {
							return call_tool(
								"fs.exists",
								json!({ "path": resolved_path }),
								"fs.find returned a unique candidate; confirm that the resolved path exists.",
							);
						}
						Some("fs.list_dir") => {
							return call_tool(
								"fs.list_dir",
								json!({ "path": resolved_path }),
								"fs.find returned a unique candidate; list the resolved directory.",
							);
						}
						Some("fs.inspect") | None => {
							return call_tool(
								"fs.inspect",
								json!({ "path": resolved_path }),
								"fs.find returned a unique candidate; inspect the resolved path.",
							);
						}
						Some(other) => {
							return fail(format!(
								"filesystem loop cannot continue with unsupported follow-up tool `{other}`"
							));
						}
					}
				}
				return final_answer(
					summarize_observation(&loop_state.goal, observation).final_message,
				);
			}
			"fs.read_text" | "fs.list_dir" | "fs.inspect" | "fs.exists" | "fs.glob" => {
				return final_answer(
					summarize_observation(&loop_state.goal, observation).final_message,
				);
			}
			_ => {
				return fail(format!(
					"unexpected filesystem observation from `{}`",
					observation.tool_name
				));
			}
		}
	}

	match observation.error_type.as_deref() {
		Some("multiple_candidates") => {
			if let Some(user_reply) = user_reply
				&& let Some(matches) = observation
					.data
					.get("matches")
					.and_then(Value::as_array)
					.map(|values| {
						values
							.iter()
							.filter_map(Value::as_str)
							.map(str::to_string)
							.collect::<Vec<_>>()
					}) && let Some(selected_path) = reply_selects_candidate(user_reply, &matches)
			{
				return match follow_up_tool_for_resolved_path(loop_state, &selected_path) {
					Some("fs.read_text") => call_tool(
						"fs.read_text",
						json!({ "path": selected_path, "max_bytes": 4_096_u64 }),
						"The user selected one candidate path; read that file.",
					),
					Some("fs.exists") => call_tool(
						"fs.exists",
						json!({ "path": selected_path }),
						"The user selected one candidate path; confirm its existence.",
					),
					Some("fs.list_dir") => call_tool(
						"fs.list_dir",
						json!({ "path": selected_path }),
						"The user selected one candidate path; list that directory.",
					),
					Some("fs.inspect") | None => call_tool(
						"fs.inspect",
						json!({ "path": selected_path }),
						"The user selected one candidate path; inspect it directly.",
					),
					Some(other) => fail(format!(
						"filesystem loop cannot continue with unsupported follow-up tool `{other}`"
					)),
				};
			}
			return ask_user(
				ask_user_from_observation(&loop_state.goal, observation).final_message,
			);
		}
		Some("path_not_found")
			if observation.tool_name != "fs.find"
				&& loop_state.remaining_recovery_budget > 0
				&& tool_visible(loop_state, "fs.find") =>
		{
			if let Some(path) = observation
				.data
				.get("path")
				.and_then(Value::as_str)
				.and_then(file_name_from_path)
				.or(hints.basename_only.clone())
			{
				return call_tool(
					"fs.find",
					json!({
						"name": path,
						"kind": find_kind_for_preferred_tool(preferred_tool(loop_state)),
					}),
					"Recover from a missing path by grounding the basename within the workspace.",
				);
			}
		}
		Some("workspace_violation")
		| Some("not_directory")
		| Some("not_file")
		| Some("permission_denied")
		| Some("path_not_found")
		| Some("tool_timeout")
		| Some("invalid_argument")
		| Some("tool_not_found") => {
			return final_answer(
				summarize_observation(&loop_state.goal, observation).final_message,
			);
		}
		_ => {}
	}

	if loop_state.remaining_recovery_budget == 0 {
		return final_answer(summarize_observation(&loop_state.goal, observation).final_message);
	}

	final_answer(summarize_observation(&loop_state.goal, observation).final_message)
}

fn preferred_tool(loop_state: &crate::runtime_loop::LoopState) -> Option<&str> {
	loop_state.visible_tools.first().map(String::as_str)
}

fn follow_up_tool(loop_state: &crate::runtime_loop::LoopState) -> Option<&str> {
	loop_state
		.visible_tools
		.iter()
		.map(String::as_str)
		.find(|tool| *tool != "fs.find")
}

fn follow_up_tool_for_resolved_path<'a>(
	loop_state: &'a crate::runtime_loop::LoopState,
	resolved_path: &str,
) -> Option<&'a str> {
	if let Ok(metadata) = fs::symlink_metadata(resolved_path) {
		if metadata.is_file() && tool_visible(loop_state, "fs.read_text") {
			return Some("fs.read_text");
		}
		if metadata.is_dir() && tool_visible(loop_state, "fs.list_dir") {
			return Some("fs.list_dir");
		}
	}
	follow_up_tool(loop_state)
}

fn tool_visible(loop_state: &crate::runtime_loop::LoopState, tool_name: &str) -> bool {
	loop_state
		.visible_tools
		.iter()
		.any(|tool| tool == tool_name)
}

fn filesystem_tool_requirements(visible_tools: &[String]) -> Value {
	let mut requirements = serde_json::Map::new();
	for tool_name in visible_tools {
		requirements.insert(
			tool_name.clone(),
			json!({
				"required_arguments": filesystem_required_argument_keys(tool_name),
			}),
		);
	}
	Value::Object(requirements)
}

fn filesystem_required_argument_keys(tool_name: &str) -> &'static [&'static str] {
	match tool_name {
		"fs.find" => &["name"],
		"fs.inspect" | "fs.list_dir" | "fs.read_text" | "fs.exists" => &["path"],
		"fs.glob" => &["pattern"],
		_ => &[],
	}
}

fn find_kind_for_preferred_tool(preferred_tool: Option<&str>) -> &'static str {
	match preferred_tool {
		Some("fs.list_dir") => "directory",
		Some("fs.read_text") => "file",
		_ => "any",
	}
}

fn call_tool(tool_name: &str, arguments: Value, reason: impl Into<String>) -> NextStepDecision {
	NextStepDecision {
		action: NextStepAction::CallTool,
		tool_name: Some(tool_name.to_string()),
		arguments: Some(arguments),
		reason: reason.into(),
		final_message: None,
	}
}

fn ask_user(final_message: String) -> NextStepDecision {
	NextStepDecision {
		action: NextStepAction::AskUser,
		tool_name: None,
		arguments: None,
		reason: "filesystem loop needs more information from the user".to_string(),
		final_message: Some(final_message),
	}
}

fn final_answer(final_message: String) -> NextStepDecision {
	NextStepDecision {
		action: NextStepAction::FinalAnswer,
		tool_name: None,
		arguments: None,
		reason: "filesystem loop has enough information to answer the user".to_string(),
		final_message: Some(final_message),
	}
}

fn fail(reason: String) -> NextStepDecision {
	NextStepDecision {
		action: NextStepAction::Fail,
		tool_name: None,
		arguments: None,
		reason: reason.clone(),
		final_message: Some(reason),
	}
}

#[derive(Debug, Clone, Default)]
struct GoalHints {
	explicit_path: Option<String>,
	basename_only: Option<String>,
}

impl GoalHints {
	fn from_goal(goal: &str) -> Self {
		let explicit_paths = extract_path_candidates(goal);
		let explicit_path = explicit_paths.first().cloned();
		let basename_only = explicit_path
			.as_deref()
			.filter(|path| is_basename_candidate(path))
			.map(str::to_string);
		Self {
			explicit_path,
			basename_only,
		}
	}
}

fn is_basename_candidate(path: &str) -> bool {
	!path.is_empty()
		&& path != "."
		&& path != ".."
		&& !Path::new(path).is_absolute()
		&& !path.contains('/')
		&& !path.contains('\\')
}

fn file_name_from_path(path: &str) -> Option<String> {
	PathBuf::from(path)
		.file_name()
		.and_then(|name| name.to_str())
		.filter(|name| !name.is_empty())
		.map(str::to_string)
}

#[cfg(test)]
mod tests {
	use std::fs;

	use crate::GenericAgentRuntime;
	use crate::router::{IntentFamily, RouteDecision, RouteRisk};
	use roku_common_types::{RequestEnvelope, RequestId};

	use super::follow_up_tool_for_resolved_path;

	#[test]
	fn follow_up_prefers_read_text_for_resolved_files() {
		let tempdir = tempfile::tempdir().expect("tempdir");
		let file_path = tempdir.path().join("example.rs");
		fs::write(&file_path, "fn main() {}\n").expect("write file");
		let runtime = GenericAgentRuntime::default();
		let request = RequestEnvelope {
			request_id: RequestId("req-fs-follow-up".to_string()),
			session_id: "session-fs-follow-up".to_string(),
			goal: "show example.rs".to_string(),
			planning_mode_hint: None,
			conversation_history: Vec::new(),
		};
		let decision = RouteDecision::new(
			IntentFamily::FilesystemRead,
			0.9,
			false,
			RouteRisk::Low,
			vec!["fs.inspect".to_string(), "fs.read_text".to_string()],
			vec!["core-fs".to_string()],
			Vec::new(),
			"filesystem read",
		);
		let loop_state =
			runtime.initialize_runtime_loop(&request, &request.session_id, &decision, Vec::new());

		assert_eq!(
			follow_up_tool_for_resolved_path(&loop_state, &file_path.display().to_string()),
			Some("fs.read_text")
		);
	}
}
