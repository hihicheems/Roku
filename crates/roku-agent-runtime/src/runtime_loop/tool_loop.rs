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

use roku_plugin_llm::{GenerationRequest, LlmRouter, RiskTier};
use serde_json::{Value, json};

use crate::router::IntentFamily;
use crate::runtime_loop::grounding::{
	extract_explicit_python_code, extract_path_candidates, extract_row_limit, extract_sheet_name,
	extract_table_path, extract_web_query,
};
use crate::runtime_loop::{
	LoopState, NextStepAction, NextStepDecision, ToolObservation,
	ask_user::ask_user_from_observation, summarize_observation,
};

pub(crate) fn decide_tool_loop_next_step(
	loop_state: &LoopState,
	router: Option<&LlmRouter>,
	user_reply: Option<&str>,
) -> NextStepDecision {
	if loop_state.last_observation.is_some() {
		return deterministic_next_step(loop_state, user_reply, router.is_some());
	}
	if let Some(router) = router
		&& let Some(decision) = decide_with_router(loop_state, router, user_reply)
	{
		return decision;
	}
	deterministic_next_step(loop_state, user_reply, router.is_some())
}

fn decide_with_router(
	loop_state: &LoopState,
	router: &LlmRouter,
	user_reply: Option<&str>,
) -> Option<NextStepDecision> {
	let response = router
		.generate_json_value(&GenerationRequest {
			system_prompt: Some(
				"You are Roku's runtime loop next-step decision model. Return only valid JSON."
					.to_string(),
			),
			prompt: tool_loop_prompt(loop_state, user_reply),
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
		NextStepAction::AskUser | NextStepAction::FinalAnswer | NextStepAction::Fail => {
			Some(decision)
		}
	}
}

fn tool_loop_prompt(loop_state: &LoopState, user_reply: Option<&str>) -> String {
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
- Use the current user follow-up if it is present; do not inherit concrete code, paths, or queries from prior conversation turns unless they already exist in the last observation.
- For `chat`, prefer `general.execute` when it is visible.
- For `code_exec`, only call `python.run` when explicit code is present in the grounding context.
- For `table_read`, prefer the first shortlisted `table.*` tool that matches the grounded table path.
- For `web_lookup`, use `web.search` when a concrete query is available.
- Use `ask_user` when the current information is still insufficient.
- Use `final_answer` after a successful observation that already satisfies the user request.

Visible tools:
{visible_tools}

Tool requirements:
{tool_requirements}

Intent family:
{intent_family}

Current working directory:
{cwd}

Original goal:
{goal}

Current user follow-up:
{user_reply}

Last observation:
{last_observation}"#,
		visible_tools = serde_json::to_string_pretty(&loop_state.visible_tools)
			.unwrap_or_else(|_| "[]".to_string()),
		tool_requirements =
			serde_json::to_string_pretty(&tool_requirements(&loop_state.visible_tools))
				.unwrap_or_else(|_| "{}".to_string()),
		intent_family = serde_json::to_string(&loop_state.route_decision.intent_family)
			.unwrap_or_else(|_| "\"unknown\"".to_string()),
		cwd = loop_state.working_directory,
		goal = loop_state.goal,
		user_reply = user_reply.unwrap_or("null"),
		last_observation = loop_state
			.last_observation
			.as_ref()
			.map(|observation| serde_json::to_string_pretty(observation).unwrap_or_default())
			.unwrap_or_else(|| "null".to_string()),
	)
}

fn deterministic_next_step(
	loop_state: &LoopState,
	user_reply: Option<&str>,
	router_available: bool,
) -> NextStepDecision {
	if let Some(observation) = loop_state.last_observation.as_ref() {
		return next_step_from_observation(loop_state, observation);
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
	match loop_state.route_decision.intent_family {
		IntentFamily::TableRead => initial_table_step(loop_state, grounding_input),
		IntentFamily::WebLookup => initial_web_step(loop_state, grounding_input),
		IntentFamily::CodeExec => initial_python_step(loop_state, grounding_input),
		IntentFamily::Chat => initial_chat_step(loop_state, router_available),
		_ => fail("tool loop does not support this intent family".to_string()),
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
	if router_available && tool_visible(loop_state, "general.execute") {
		return call_tool(
			"general.execute",
			json!({}),
			"Use the general assistant tool for a direct conversational reply.",
		);
	}
	final_answer(deterministic_chat_loop_message(&loop_state.goal))
}

fn next_step_from_observation(
	loop_state: &LoopState,
	observation: &ToolObservation,
) -> NextStepDecision {
	if observation.ok {
		return final_answer(summarize_observation(&loop_state.goal, observation).final_message);
	}
	match observation.error_type.as_deref() {
		Some("multiple_candidates") => {
			ask_user(ask_user_from_observation(&loop_state.goal, observation).final_message)
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
		"table.inspect" | "table.list_sheets" | "table.preview" | "table.schema" => &["path"],
		"web.search" => &["query"],
		"python.run" => &["code"],
		"general.execute" => &[],
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
