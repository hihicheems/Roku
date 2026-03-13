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
use std::path::Path;

use roku_plugin_llm::{GenerationRequest, LlmRouter, RiskTier};
use serde_json::{Value, json};

use crate::router::IntentFamily;
use crate::runtime_loop::grounding::{
	extract_explicit_python_code, extract_path_candidates, extract_row_limit, extract_sheet_name,
	extract_skill_source_url, extract_table_path, extract_web_query, file_name_from_path,
	reply_selects_candidate,
};
use crate::runtime_loop::{
	ContextProjection, LoopState, NextStepAction, NextStepDecision, ToolObservation,
	ask_user::ask_user_from_observation, summarize_observation,
};

pub(crate) fn decide_tool_loop_next_step(
	loop_state: &LoopState,
	context_projection: &ContextProjection,
	router: Option<&LlmRouter>,
	user_reply: Option<&str>,
) -> NextStepDecision {
	if let Some(decision) = deterministic_terminal_shortcut(loop_state) {
		return decision;
	}
	if let Some(router) = router
		&& let Some(decision) =
			decide_with_router(loop_state, context_projection, router, user_reply)
	{
		return decision;
	}
	deterministic_next_step(loop_state, user_reply, router.is_some())
}

fn decide_with_router(
	loop_state: &LoopState,
	context_projection: &ContextProjection,
	router: &LlmRouter,
	user_reply: Option<&str>,
) -> Option<NextStepDecision> {
	let response = router
		.generate_json_value(&GenerationRequest {
			system_prompt: Some(
				"You are Roku's runtime loop next-step decision model. Return only valid JSON."
					.to_string(),
			),
			prompt: tool_loop_prompt(context_projection, user_reply),
			expected_output_tokens: 240,
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
	loop_state: &LoopState,
	decision: NextStepDecision,
) -> Option<NextStepDecision> {
	match decision.action {
		NextStepAction::CallTool => {
			let tool_name = decision.tool_name.as_deref()?;
			if !tool_visible(loop_state, tool_name) {
				return None;
			}
			let arguments = decision.arguments.as_ref()?.as_object()?;
			tool_required_argument_keys(tool_name)
				.iter()
				.all(|key| arguments.contains_key(*key))
				.then_some(decision)
		}
		NextStepAction::FinalAnswer => {
			let latest_observation = loop_state.last_observation.as_ref();
			if latest_observation.is_some_and(|observation| {
				should_synthesize_with_general_tool(loop_state, observation)
			}) {
				return None;
			}
			Some(decision)
		}
		NextStepAction::AskUser | NextStepAction::Fail => Some(decision),
	}
}

fn tool_loop_prompt(context_projection: &ContextProjection, user_reply: Option<&str>) -> String {
	let projection_json =
		serde_json::to_string_pretty(context_projection).unwrap_or_else(|_| "{}".to_string());
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
- Keep `final_message` concise. Do not paste large grounded documents, search dumps, or long synthesized answers into the JSON decision.
- Use the current user follow-up if it is present; do not inherit concrete code, paths, or queries from prior conversation turns unless they already exist in the current context projection.
- For `chat`, prefer `general.execute` when it is visible.
- For `code_exec`, only call `python.run` when explicit code is present in the grounding context.
- For `table_read`, prefer the first shortlisted `table.*` tool that matches the grounded table path.
- For `web_lookup`, use `web.search` when a concrete query is available.
- When a filesystem, table, web, or python observation provides raw evidence but the user still needs explanation, comparison, or synthesis, prefer `general.execute` before emitting `final_answer`.
- Use `ask_user` when the current information is still insufficient.
- Use `final_answer` only when the current context projection already proves the user request is satisfied.

Context projection:
{projection_json}

History digest:
{history_digest}

Tool requirements:
{tool_requirements}

Current user follow-up:
{user_reply}
"#,
		projection_json = projection_json,
		history_digest = context_projection.history_digest,
		tool_requirements =
			serde_json::to_string_pretty(&tool_requirements(&context_projection.visible_tools))
				.unwrap_or_else(|_| "{}".to_string()),
		user_reply = user_reply.unwrap_or("null"),
	)
}

fn deterministic_next_step(
	loop_state: &LoopState,
	user_reply: Option<&str>,
	router_available: bool,
) -> NextStepDecision {
	if let Some(observation) = loop_state.last_observation.as_ref() {
		return next_step_from_observation(loop_state, observation, user_reply);
	}
	initial_next_step(
		loop_state,
		user_reply.unwrap_or(&loop_state.goal),
		router_available,
	)
}

fn initial_next_step(
	loop_state: &LoopState,
	grounding_input: &str,
	router_available: bool,
) -> NextStepDecision {
	match preferred_tool(
		loop_state,
		fallback_tool_for_intent(loop_state.route_decision.intent_family),
	) {
		"fs.find" | "fs.glob" | "fs.inspect" | "fs.list_dir" | "fs.exists" | "fs.read_text" => {
			initial_filesystem_step(loop_state, grounding_input)
		}
		"inventory.describe" => call_tool(
			"inventory.describe",
			json!({}),
			"Use the inventory tool to answer the runtime inventory request directly.",
		),
		"skill.install" => initial_skill_install_step(grounding_input),
		"skill.execute" => call_tool(
			"skill.execute",
			json!({}),
			"Invoke the selected installed skill through the runtime loop.",
		),
		"general.execute" => initial_chat_step(loop_state, router_available),
		"table.inspect" | "table.list_sheets" | "table.preview" | "table.schema" => {
			initial_table_step(loop_state, grounding_input)
		}
		"web.search" => initial_web_step(loop_state, grounding_input),
		"python.run" => initial_python_step(loop_state, grounding_input),
		other => initial_generic_step(loop_state, other),
	}
}

fn initial_skill_install_step(grounding_input: &str) -> NextStepDecision {
	let Some(source_url) = extract_skill_source_url(grounding_input) else {
		return ask_user(
			"I need a concrete skill source URL before I can install that skill.".to_string(),
		);
	};
	call_tool(
		"skill.install",
		json!({ "source_url": source_url }),
		"Grounded a concrete skill source URL; call the skill install tool.",
	)
}

fn initial_filesystem_step(loop_state: &LoopState, grounding_input: &str) -> NextStepDecision {
	let preferred_tool = preferred_tool(loop_state, "fs.inspect");
	let explicit_path = extract_path_candidates(grounding_input)
		.into_iter()
		.next()
		.map(|path| path.trim().to_string())
		.filter(|path| !path.is_empty());
	let basename_only = explicit_path
		.as_deref()
		.filter(|path| is_basename_reference(path))
		.map(str::to_string);

	if let Some(basename) = basename_only
		&& tool_visible(loop_state, "fs.find")
		&& preferred_tool != "fs.list_dir"
	{
		return call_tool(
			"fs.find",
			json!({
				"name": basename,
				"kind": find_kind_for_preferred_tool(preferred_tool),
			}),
			"Resolve the basename into a concrete filesystem target before attempting another read step.",
		);
	}

	match preferred_tool {
		"fs.list_dir" => call_tool(
			"fs.list_dir",
			json!({ "path": explicit_path.as_deref().unwrap_or(".") }),
			"Use the filesystem listing tool for the current grounded target.",
		),
		"fs.inspect" => call_tool(
			"fs.inspect",
			json!({ "path": explicit_path.as_deref().unwrap_or(".") }),
			"Inspect the grounded filesystem target first.",
		),
		"fs.exists" => {
			let Some(path) = explicit_path else {
				return ask_user(
					"I need a concrete path before I can check whether it exists.".to_string(),
				);
			};
			call_tool(
				"fs.exists",
				json!({ "path": path }),
				"Check whether the grounded filesystem target exists.",
			)
		}
		"fs.read_text" => {
			let Some(path) = explicit_path else {
				return ask_user(
					"I need a concrete file path before I can read that file.".to_string(),
				);
			};
			call_tool(
				"fs.read_text",
				json!({ "path": path }),
				"Read the grounded text file through the generic runtime loop.",
			)
		}
		"fs.glob" => {
			let Some(pattern) = explicit_path.or_else(|| extract_glob_pattern(grounding_input))
			else {
				return ask_user(
					"I need a concrete glob pattern before I can search the filesystem."
						.to_string(),
				);
			};
			call_tool(
				"fs.glob",
				json!({ "pattern": pattern }),
				"Search the workspace with the grounded glob pattern.",
			)
		}
		"fs.find" => {
			let Some(path) = extract_path_candidates(grounding_input).into_iter().next() else {
				return ask_user(
					"I need a concrete file or directory name before I can search for it."
						.to_string(),
				);
			};
			call_tool(
				"fs.find",
				json!({
					"name": path,
					"kind": "any",
				}),
				"Search the workspace for the grounded filesystem target.",
			)
		}
		other => initial_generic_step(loop_state, other),
	}
}

fn initial_table_step(loop_state: &LoopState, grounding_input: &str) -> NextStepDecision {
	let Some(path) = extract_table_path(grounding_input) else {
		return ask_user(
			"I need a concrete csv/tsv/xlsx path before I can inspect that table.".to_string(),
		);
	};
	let preferred_tool = preferred_tool(loop_state, "table.preview");
	let mut arguments = json!({ "path": path });
	if preferred_tool == "table.preview" {
		arguments["rows"] = Value::from(extract_row_limit(grounding_input).unwrap_or(5_u64));
	}
	if let Some(sheet) = extract_sheet_name(grounding_input) {
		arguments["sheet"] = Value::String(sheet);
	}
	call_tool(
		preferred_tool,
		arguments,
		"Grounded a concrete table path; invoke the shortlisted table tool.",
	)
}

fn initial_web_step(loop_state: &LoopState, grounding_input: &str) -> NextStepDecision {
	let Some(query) = extract_web_query(grounding_input) else {
		return ask_user("I need a concrete search query before I can search the web.".to_string());
	};
	let tool_name = preferred_tool(loop_state, "web.search");
	call_tool(
		tool_name,
		json!({ "query": query, "top_k": 5_u64 }),
		"Grounded a concrete search query; call the web search tool.",
	)
}

fn initial_python_step(loop_state: &LoopState, grounding_input: &str) -> NextStepDecision {
	let Some(code) = extract_explicit_python_code(grounding_input) else {
		return ask_user(
			"Please send explicit Python code in a fenced block or inline code snippet."
				.to_string(),
		);
	};
	let tool_name = preferred_tool(loop_state, "python.run");
	call_tool(
		tool_name,
		json!({ "code": code }),
		"Explicit Python code is grounded in the current input; call python.run.",
	)
}

fn initial_chat_step(loop_state: &LoopState, router_available: bool) -> NextStepDecision {
	if preferred_tool(loop_state, "general.execute") == "inventory.describe"
		&& tool_visible(loop_state, "inventory.describe")
	{
		return call_tool(
			"inventory.describe",
			json!({}),
			"Use the inventory tool to answer a runtime inventory question directly.",
		);
	}
	if router_available && tool_visible(loop_state, "general.execute") {
		return call_tool(
			"general.execute",
			json!({}),
			"Use the general assistant tool for a direct conversational reply.",
		);
	}
	final_answer(deterministic_chat_loop_message(&loop_state.goal))
}

fn initial_generic_step(loop_state: &LoopState, tool_name: &str) -> NextStepDecision {
	if !tool_visible(loop_state, tool_name) {
		return fail(format!(
			"tool loop cannot see the shortlisted tool `{tool_name}`"
		));
	}
	if tool_required_argument_keys(tool_name).is_empty() {
		return call_tool(
			tool_name,
			json!({}),
			"The shortlisted direct tool does not require any grounded arguments.",
		);
	}
	fail(format!(
		"tool loop does not know how to ground required arguments for `{tool_name}`"
	))
}

fn next_step_from_observation(
	loop_state: &LoopState,
	observation: &ToolObservation,
	user_reply: Option<&str>,
) -> NextStepDecision {
	if observation.ok {
		if let Some(decision) = filesystem_follow_up_from_success(loop_state, observation) {
			return decision;
		}
		if should_synthesize_with_general_tool(loop_state, observation) {
			return call_tool(
				"general.execute",
				json!({}),
				"Use the general assistant tool to synthesize the grounded observation into a user-facing answer.",
			);
		}
		return final_answer(summarize_observation(&loop_state.goal, observation).final_message);
	}
	match observation.error_type.as_deref() {
		Some("multiple_candidates") => {
			if let Some(decision) =
				filesystem_follow_up_from_multiple_candidates(loop_state, observation, user_reply)
			{
				return decision;
			}
			ask_user(ask_user_from_observation(&loop_state.goal, observation).final_message)
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
				.or_else(|| {
					extract_path_candidates(&loop_state.goal)
						.into_iter()
						.find(|candidate| is_basename_reference(candidate))
				}) {
				return call_tool(
					"fs.find",
					json!({
						"name": path,
						"kind": find_kind_for_preferred_tool(preferred_filesystem_follow_up_tool(loop_state).unwrap_or("fs.inspect")),
					}),
					"Recover from a missing filesystem path by grounding the basename inside the workspace.",
				);
			}
			final_answer(summarize_observation(&loop_state.goal, observation).final_message)
		}
		Some("invalid_argument")
		| Some("path_not_found")
		| Some("workspace_violation")
		| Some("unsupported_content_type")
		| Some("tool_timeout")
		| Some("permission_denied")
		| Some("tool_not_found")
		| Some("not_directory")
		| Some("not_file") => {
			final_answer(summarize_observation(&loop_state.goal, observation).final_message)
		}
		_ => fail(observation.message.clone()),
	}
}

fn tool_requirements(visible_tools: &[String]) -> serde_json::Value {
	serde_json::Value::Object(
		visible_tools
			.iter()
			.map(|tool_name| {
				(
					tool_name.clone(),
					json!({
						"required_argument_keys": tool_required_argument_keys(tool_name),
					}),
				)
			})
			.collect(),
	)
}

fn tool_required_argument_keys(tool_name: &str) -> &'static [&'static str] {
	match tool_name {
		"fs.exists" | "fs.inspect" | "fs.list_dir" | "fs.read_text" => &["path"],
		"fs.find" => &["name"],
		"fs.glob" => &["pattern"],
		"table.inspect" | "table.list_sheets" | "table.preview" | "table.schema" => &["path"],
		"web.search" => &["query"],
		"python.run" => &["code"],
		"inventory.describe" | "general.execute" | "skill.install" | "skill.execute" => &[],
		_ => &[],
	}
}

fn preferred_tool<'a>(loop_state: &'a LoopState, fallback: &'a str) -> &'a str {
	loop_state
		.visible_tools
		.first()
		.map(String::as_str)
		.unwrap_or(fallback)
}

fn fallback_tool_for_intent(intent_family: IntentFamily) -> &'static str {
	match intent_family {
		IntentFamily::Chat => "general.execute",
		IntentFamily::TableRead => "table.preview",
		IntentFamily::WebLookup => "web.search",
		IntentFamily::CodeExec => "python.run",
		IntentFamily::TextTransform => "general.execute",
		IntentFamily::FilesystemRead | IntentFamily::MultiStep | IntentFamily::Unknown => {
			"general.execute"
		}
	}
}

fn tool_visible(loop_state: &LoopState, tool_name: &str) -> bool {
	loop_state
		.visible_tools
		.iter()
		.any(|visible| visible == tool_name)
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
		reason: "The loop needs more concrete user input before the next tool step.".to_string(),
		final_message: Some(final_message),
	}
}

fn final_answer(final_message: String) -> NextStepDecision {
	NextStepDecision {
		action: NextStepAction::FinalAnswer,
		tool_name: None,
		arguments: None,
		reason: "The latest observation is sufficient to answer the user.".to_string(),
		final_message: Some(final_message),
	}
}

fn fail(message: String) -> NextStepDecision {
	NextStepDecision {
		action: NextStepAction::Fail,
		tool_name: None,
		arguments: None,
		reason: "The loop could not find a valid next step.".to_string(),
		final_message: Some(message),
	}
}

fn deterministic_chat_loop_message(goal: &str) -> String {
	if !goal.is_ascii() {
		"我是 Roku。当前这条请求会通过 runtime loop 的 direct chat 路径处理。".to_string()
	} else {
		"I'm Roku. This request is being handled through the runtime loop direct chat path."
			.to_string()
	}
}

fn deterministic_terminal_shortcut(loop_state: &LoopState) -> Option<NextStepDecision> {
	let observation = loop_state.last_observation.as_ref()?;
	if observation.ok && observation.tool_name == "general.execute" {
		return Some(final_answer(
			summarize_observation(&loop_state.goal, observation).final_message,
		));
	}
	None
}

fn filesystem_follow_up_from_success(
	loop_state: &LoopState,
	observation: &ToolObservation,
) -> Option<NextStepDecision> {
	if observation.tool_name != "fs.find" {
		return None;
	}
	let resolved_path = observation
		.data
		.get("resolved_path")
		.and_then(Value::as_str)?;
	Some(call_tool(
		filesystem_follow_up_tool_for_resolved_path(loop_state, resolved_path),
		filesystem_follow_up_arguments(
			filesystem_follow_up_tool_for_resolved_path(loop_state, resolved_path),
			resolved_path,
		),
		"Continue from the resolved filesystem candidate with the most suitable follow-up read step.",
	))
}

fn filesystem_follow_up_from_multiple_candidates(
	loop_state: &LoopState,
	observation: &ToolObservation,
	user_reply: Option<&str>,
) -> Option<NextStepDecision> {
	if !observation.tool_name.starts_with("fs.") {
		return None;
	}
	let matches = observation
		.data
		.get("matches")
		.and_then(Value::as_array)
		.map(|values| {
			values
				.iter()
				.filter_map(Value::as_str)
				.map(str::to_string)
				.collect::<Vec<_>>()
		})?;
	if let Some(selected_path) =
		user_reply.and_then(|reply| reply_selects_candidate(reply, &matches))
	{
		let tool_name = filesystem_follow_up_tool_for_resolved_path(loop_state, &selected_path);
		return Some(call_tool(
			tool_name,
			filesystem_follow_up_arguments(tool_name, &selected_path),
			"The user selected one filesystem candidate; continue with the resolved follow-up step.",
		));
	}
	let selected_path = auto_select_working_directory_candidate(loop_state, &matches)?;
	let tool_name = filesystem_follow_up_tool_for_resolved_path(loop_state, &selected_path);
	Some(call_tool(
		tool_name,
		filesystem_follow_up_arguments(tool_name, &selected_path),
		"Continue with the unique candidate that matches the current working directory before asking the user.",
	))
}

fn auto_select_working_directory_candidate(
	loop_state: &LoopState,
	matches: &[String],
) -> Option<String> {
	let working_directory = Path::new(&loop_state.working_directory);
	let mut candidates = matches
		.iter()
		.filter(|candidate| {
			Path::new(candidate)
				.parent()
				.is_some_and(|parent| parent == working_directory)
		})
		.cloned()
		.collect::<Vec<_>>();
	candidates.dedup();
	(candidates.len() == 1).then(|| candidates.remove(0))
}

fn filesystem_follow_up_tool_for_resolved_path<'a>(
	loop_state: &'a LoopState,
	resolved_path: &str,
) -> &'a str {
	if let Ok(metadata) = fs::symlink_metadata(resolved_path) {
		if metadata.is_file() && tool_visible(loop_state, "fs.read_text") {
			return "fs.read_text";
		}
		if metadata.is_dir() && tool_visible(loop_state, "fs.list_dir") {
			return "fs.list_dir";
		}
	}
	preferred_filesystem_follow_up_tool(loop_state).unwrap_or("fs.inspect")
}

fn preferred_filesystem_follow_up_tool(loop_state: &LoopState) -> Option<&str> {
	loop_state
		.visible_tools
		.iter()
		.map(String::as_str)
		.find(|tool_name| {
			matches!(
				*tool_name,
				"fs.read_text" | "fs.list_dir" | "fs.inspect" | "fs.exists"
			)
		})
}

fn filesystem_follow_up_arguments(tool_name: &str, resolved_path: &str) -> Value {
	match tool_name {
		"fs.read_text" => json!({ "path": resolved_path, "max_bytes": 4_096_u64 }),
		"fs.list_dir" | "fs.inspect" | "fs.exists" => json!({ "path": resolved_path }),
		other => json!({ "path": resolved_path, "requested_tool": other }),
	}
}

fn should_synthesize_with_general_tool(
	loop_state: &LoopState,
	observation: &ToolObservation,
) -> bool {
	if !tool_visible(loop_state, "general.execute") {
		return false;
	}
	let requires_synthesis = goal_requires_synthesis(&loop_state.goal);
	if !requires_synthesis {
		return false;
	}
	matches!(
		observation.tool_name.as_str(),
		"fs.find"
			| "fs.glob"
			| "fs.inspect"
			| "fs.list_dir"
			| "fs.read_text"
			| "table.preview"
			| "table.inspect"
			| "table.list_sheets"
			| "table.schema"
			| "web.search"
			| "python.run"
	)
}

fn goal_requires_synthesis(goal: &str) -> bool {
	let lowered = goal.to_ascii_lowercase();
	let ascii_hints = [
		"summarize",
		"summary",
		"compare",
		"difference",
		"explain",
		"tell me",
		"what is",
		"organize",
		"organization",
		"best practice",
	];
	if ascii_hints.iter().any(|hint| lowered.contains(hint)) {
		return true;
	}
	[
		"总结",
		"差异",
		"解释",
		"告诉我",
		"是什么",
		"最佳实践",
		"组织",
		"比较",
	]
	.iter()
	.any(|hint| goal.contains(hint))
}

fn find_kind_for_preferred_tool(tool_name: &str) -> &'static str {
	match tool_name {
		"fs.list_dir" => "directory",
		"fs.read_text" => "file",
		_ => "any",
	}
}

fn is_basename_reference(path: &str) -> bool {
	!path.is_empty()
		&& !matches!(path, "." | "..")
		&& !path.contains(std::path::MAIN_SEPARATOR)
		&& !path.contains('/')
		&& !path.contains('\\')
}

fn extract_glob_pattern(value: &str) -> Option<String> {
	extract_path_candidates(value)
		.into_iter()
		.find(|candidate| candidate.contains('*') || candidate.contains('?'))
}

pub(crate) fn attachments_for_tool(
	tool_name: &str,
	grounding_input: &str,
) -> Vec<std::path::PathBuf> {
	match tool_name {
		"python.run" => extract_path_candidates(grounding_input)
			.into_iter()
			.map(std::path::PathBuf::from)
			.collect(),
		_ => Vec::new(),
	}
}

#[cfg(test)]
mod tests {
	use std::collections::VecDeque;
	use std::env;
	use std::sync::{Arc, Mutex};

	use roku_common_types::ResourceSelector;
	use roku_plugin_llm::{
		GenerationRequest, LlmProvider, LlmRouter, ModelProfile, ProviderCallError,
		ProviderResponse, RiskTier, RoutingPolicy,
	};
	use serde_json::json;

	use super::{decide_tool_loop_next_step, tool_loop_prompt};
	use crate::router::{IntentFamily, RouteDecision, RouteRisk};
	use crate::runtime_loop::{
		ContextProjection, LoopContext, LoopDriverKind, LoopState, StepObservation,
		ToolObservation, build_context_projection, step_record::StepRecord,
	};

	struct PromptRecordingProvider {
		prompts: Arc<Mutex<Vec<String>>>,
		responses: Arc<Mutex<VecDeque<String>>>,
	}

	impl LlmProvider for PromptRecordingProvider {
		fn provider_name(&self) -> &'static str {
			"tool-loop-test-provider"
		}

		fn complete(
			&self,
			_model: &ModelProfile,
			request: &GenerationRequest,
		) -> Result<ProviderResponse, ProviderCallError> {
			self.prompts
				.lock()
				.expect("prompt lock should succeed")
				.push(request.prompt.clone());
			let output = self
				.responses
				.lock()
				.expect("response lock should succeed")
				.pop_front()
				.expect("a canned response should be available");
			Ok(ProviderResponse {
				output,
				finish_reason: None,
				prompt_tokens: 24,
				output_tokens: 18,
				latency_ms: 10,
			})
		}
	}

	fn sample_loop_state() -> LoopState {
		let context = LoopContext {
			request_id: "req-1".to_string(),
			session_id: "session-1".to_string(),
			goal: "Summarize the runtime inventory".to_string(),
			workspace_root: "/workspace".to_string(),
			working_directory: "/workspace".to_string(),
			visible_tools: vec!["inventory.describe".to_string()],
			bound_resources: vec![ResourceSelector::tool("inventory.describe".to_string())],
			route_decision: RouteDecision::new(
				IntentFamily::Chat,
				0.91,
				false,
				RouteRisk::Low,
				vec!["inventory.describe".to_string()],
				Vec::new(),
				Vec::new(),
				"inventory request",
			),
			last_observation: None,
		};
		LoopState::new("loop-req-1", &context, LoopDriverKind::ToolLoop)
	}

	fn sample_filesystem_loop_state() -> LoopState {
		let cwd = env::current_dir()
			.expect("cwd should resolve for tests")
			.display()
			.to_string();
		let context = LoopContext {
			request_id: "req-fs-1".to_string(),
			session_id: "session-fs-1".to_string(),
			goal: "Read Cargo.toml and summarize the workspace layout".to_string(),
			workspace_root: cwd.clone(),
			working_directory: cwd.clone(),
			visible_tools: vec![
				"fs.read_text".to_string(),
				"fs.find".to_string(),
				"fs.inspect".to_string(),
				"general.execute".to_string(),
			],
			bound_resources: vec![ResourceSelector::tool("fs.read_text".to_string())],
			route_decision: RouteDecision::new(
				IntentFamily::FilesystemRead,
				0.96,
				false,
				RouteRisk::Low,
				vec!["fs.read_text".to_string(), "fs.find".to_string()],
				Vec::new(),
				Vec::new(),
				"filesystem request",
			),
			last_observation: None,
		};
		LoopState::new("loop-fs-1", &context, LoopDriverKind::ToolLoop)
	}

	fn router_with_responses(
		responses: Vec<serde_json::Value>,
	) -> (LlmRouter, Arc<Mutex<Vec<String>>>) {
		let prompts = Arc::new(Mutex::new(Vec::new()));
		let mut router = LlmRouter::new(RoutingPolicy {
			max_request_cost_usd: 1.0,
			max_latency_ms: 5_000,
		});
		router.register_provider(PromptRecordingProvider {
			prompts: Arc::clone(&prompts),
			responses: Arc::new(Mutex::new(
				responses
					.into_iter()
					.map(|value| value.to_string())
					.collect::<VecDeque<_>>(),
			)),
		});
		router.register_model(ModelProfile {
			model_id: "tool-loop-test-model".to_string(),
			provider: "tool-loop-test-provider".to_string(),
			max_context_tokens: 16_000,
			cost_per_1k_tokens_usd: 0.0,
			max_risk_tier: RiskTier::Low,
			route_priority: 100,
		});
		(router, prompts)
	}

	#[test]
	fn prompt_uses_context_projection_history_digest_instead_of_raw_step_log() {
		let mut loop_state = sample_loop_state();
		loop_state.record_step(StepRecord::tool_call(
			1,
			"inventory.describe",
			"Use the inventory tool first.",
			StepObservation::Tool(ToolObservation {
				ok: true,
				tool_name: "inventory.describe".to_string(),
				error_type: None,
				terminal: false,
				data: json!({ "runtime_mode": "deterministic" }),
				message: "deterministic placeholder only: inventory summary was not generated by a live runtime".to_string(),
			}),
			Some(15),
			3,
			2,
			"/workspace",
		));
		let projection = build_context_projection(&loop_state);
		let prompt = tool_loop_prompt(&projection, None);

		assert!(prompt.contains("History digest:"));
		assert!(prompt.contains("step 1"));
		assert!(prompt.contains("inventory.describe"));
		assert!(!prompt.contains("\"started_at\""));
		assert!(!prompt.contains("\"finished_at\""));
	}

	#[test]
	fn router_can_continue_with_call_tool_after_successful_observation() {
		let mut loop_state = sample_loop_state();
		loop_state.last_observation = Some(ToolObservation {
			ok: true,
			tool_name: "inventory.describe".to_string(),
			error_type: None,
			terminal: false,
			data: json!({ "runtime_mode": "deterministic" }),
			message: "partial inventory summary".to_string(),
		});
		let projection: ContextProjection = build_context_projection(&loop_state);
		let (router, prompts) = router_with_responses(vec![json!({
			"action": "call_tool",
			"tool_name": "inventory.describe",
			"arguments": {},
			"reason": "Collect one more grounded inventory observation.",
			"final_message": null
		})]);

		let decision = decide_tool_loop_next_step(&loop_state, &projection, Some(&router), None);

		assert_eq!(
			decision.action,
			crate::runtime_loop::NextStepAction::CallTool
		);
		assert_eq!(decision.tool_name.as_deref(), Some("inventory.describe"));
		let prompts = prompts.lock().expect("prompt lock should succeed");
		assert_eq!(prompts.len(), 1);
		assert!(prompts[0].contains("partial inventory summary"));
	}

	#[test]
	fn resolved_filesystem_find_continues_with_follow_up_read_step() {
		let mut loop_state = sample_filesystem_loop_state();
		let resolved_path = env::current_dir()
			.expect("cwd should resolve for tests")
			.join("Cargo.toml")
			.display()
			.to_string();
		loop_state.last_observation = Some(ToolObservation {
			ok: true,
			tool_name: "fs.find".to_string(),
			error_type: None,
			terminal: false,
			data: json!({
				"name": "Cargo.toml",
				"resolved_path": resolved_path,
			}),
			message: "Found 1 matching candidate for `Cargo.toml`.".to_string(),
		});
		let projection = build_context_projection(&loop_state);

		let decision = decide_tool_loop_next_step(&loop_state, &projection, None, None);

		assert_eq!(
			decision.action,
			crate::runtime_loop::NextStepAction::CallTool
		);
		assert_eq!(decision.tool_name.as_deref(), Some("fs.read_text"));
	}

	#[test]
	fn multiple_candidate_filesystem_match_prefers_working_directory_candidate() {
		let mut loop_state = sample_filesystem_loop_state();
		let cwd = env::current_dir().expect("cwd should resolve for tests");
		let root_manifest = cwd.join("Cargo.toml").display().to_string();
		let nested_manifest = cwd
			.join("crates/roku-agent-runtime/Cargo.toml")
			.display()
			.to_string();
		loop_state.last_observation = Some(ToolObservation {
			ok: false,
			tool_name: "fs.find".to_string(),
			error_type: Some("multiple_candidates".to_string()),
			terminal: false,
			data: json!({
				"name": "Cargo.toml",
				"matches": [root_manifest, nested_manifest],
			}),
			message: "Found 2 matching candidates for `Cargo.toml`.".to_string(),
		});
		let projection = build_context_projection(&loop_state);

		let decision = decide_tool_loop_next_step(&loop_state, &projection, None, None);

		assert_eq!(
			decision.action,
			crate::runtime_loop::NextStepAction::CallTool
		);
		assert_eq!(decision.tool_name.as_deref(), Some("fs.read_text"));
		let selected_path = decision
			.arguments
			.as_ref()
			.and_then(|arguments| arguments.get("path"))
			.and_then(serde_json::Value::as_str);
		assert_eq!(selected_path, cwd.join("Cargo.toml").to_str());
	}
}
