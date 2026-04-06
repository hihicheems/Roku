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

use roku_common_types::GroundingStrategy;
use roku_observability::{LogLevel, LogRecord, emit_global_log};
use roku_plugin_catalog::ResourceCatalog;
use roku_plugin_llm::{GenerationRequest, LlmRouter, RiskTier};
use serde_json::{Value, json};

use crate::runtime_config::NextStepRuntimeConfig;
use crate::runtime_loop::grounding::{
	explanatory_python_code_request, explanatory_shell_command_request,
	extract_concrete_path_candidates, extract_concrete_table_path,
	extract_explicit_path_candidates, extract_explicit_python_code, extract_explicit_shell_command,
	extract_fetch_url, extract_glob_pattern, extract_grep_pattern, extract_path_candidates,
	extract_row_limit, extract_sheet_name, extract_skill_source_url, extract_web_query,
	grounded_python_code_allows_execution, grounded_shell_command_allows_execution,
};
use crate::runtime_loop::{
	ContextProjection, LoopState, NextStepAction, NextStepDecision, ToolObservation,
	ask_user::ask_user_from_observation, file_name_from_path, summarize_observation,
};

pub(crate) fn decide_tool_loop_next_step(
	loop_state: &LoopState,
	context_projection: &ContextProjection,
	router: Option<&LlmRouter>,
	user_reply: Option<&str>,
	config: &NextStepRuntimeConfig,
	catalog: Option<&ResourceCatalog>,
) -> NextStepDecision {
	if should_force_ask_user_for_ambiguous_stagnation(loop_state, user_reply) {
		return ask_user(
			ask_user_from_observation(
				&loop_state.goal,
				loop_state
					.last_observation
					.as_ref()
					.expect("ambiguous stagnation requires a last observation"),
			)
			.final_message,
		);
	}
	if let Some(router) = router
		&& let Some(decision) = decide_with_router(
			loop_state,
			context_projection,
			router,
			user_reply,
			config,
			catalog,
		) {
		return decision;
	}
	deterministic_next_step(loop_state, user_reply, catalog)
}

fn decide_with_router(
	loop_state: &LoopState,
	context_projection: &ContextProjection,
	router: &LlmRouter,
	user_reply: Option<&str>,
	config: &NextStepRuntimeConfig,
	catalog: Option<&ResourceCatalog>,
) -> Option<NextStepDecision> {
	let response = match router.generate_json_value(&GenerationRequest {
		system_prompt: Some(
			"You are Roku's runtime loop next-step decision model. Return only valid JSON."
				.to_string(),
		),
		prompt: tool_loop_prompt(context_projection, user_reply),
		expected_output_tokens: config.expected_output_tokens,
		risk_tier: RiskTier::Low,
		preferred_provider: None,
		budget_tokens_remaining: config.budget_tokens_remaining,
		budget_cost_remaining_usd: config.budget_cost_remaining_usd,
	}) {
		Ok(response) => response,
		Err(error) => {
			log_tool_loop_warning(
				"next-step model did not return a usable response",
				[
					("run_id", loop_state.run_id.clone()),
					(
						"last_tool",
						loop_state
							.last_observation
							.as_ref()
							.map(|observation| observation.tool_name.clone())
							.unwrap_or_else(|| "none".to_string()),
					),
					("error", error.to_string()),
				],
			);
			return None;
		}
	};
	let decision = match NextStepDecision::from_json_value(&response.value) {
		Ok(decision) => decision,
		Err(error) => {
			log_tool_loop_warning(
				"next-step model returned invalid decision JSON",
				[
					("run_id", loop_state.run_id.clone()),
					(
						"last_tool",
						loop_state
							.last_observation
							.as_ref()
							.map(|observation| observation.tool_name.clone())
							.unwrap_or_else(|| "none".to_string()),
					),
					("error", error.to_string()),
					(
						"response",
						truncate_for_log(
							&serde_json::to_string(&response.value)
								.unwrap_or_else(|_| "<unserializable-json>".to_string()),
							320,
						),
					),
				],
			);
			return None;
		}
	};
	let decision =
		align_router_tool_arguments(decision, user_reply.unwrap_or(&loop_state.goal), catalog);
	match validate_router_decision(loop_state, decision, catalog) {
		Ok(decision) => Some(decision),
		Err(reason) => {
			log_tool_loop_warning(
				"next-step model decision was rejected by runtime validation",
				[
					("run_id", loop_state.run_id.clone()),
					(
						"last_tool",
						loop_state
							.last_observation
							.as_ref()
							.map(|observation| observation.tool_name.clone())
							.unwrap_or_else(|| "none".to_string()),
					),
					("reason", reason),
				],
			);
			None
		}
	}
}

fn align_router_tool_arguments(
	mut decision: NextStepDecision,
	grounding_input: &str,
	catalog: Option<&ResourceCatalog>,
) -> NextStepDecision {
	if !matches!(decision.action, NextStepAction::CallTool) {
		return decision;
	}
	let Some(tool_name) = decision.tool_name.as_deref() else {
		return decision;
	};
	let Some(grounded_arguments) = uniquely_grounded_arguments(tool_name, grounding_input, catalog)
	else {
		return decision;
	};
	let mut merged_arguments = decision
		.arguments
		.take()
		.and_then(|value| value.as_object().cloned())
		.unwrap_or_default();
	if let Some(grounded_object) = grounded_arguments.as_object() {
		for (key, value) in grounded_object {
			merged_arguments.insert(key.clone(), value.clone());
		}
	}
	decision.arguments = Some(Value::Object(merged_arguments));
	decision
}

fn uniquely_grounded_arguments(
	tool_name: &str,
	grounding_input: &str,
	catalog: Option<&ResourceCatalog>,
) -> Option<Value> {
	if let Some(grounding) = catalog.and_then(|c| c.lookup_grounding_metadata(tool_name))
		&& grounding.grounding_strategy != GroundingStrategy::None
	{
		let result = match grounding.grounding_strategy {
			GroundingStrategy::PathBased => {
				let arg = grounding.grounding_argument.as_deref().unwrap_or("path");
				if tool_name == "fs.find" {
					let paths = extract_explicit_path_candidates(grounding_input);
					(paths.len() == 1).then(|| json!({ arg: paths[0].clone(), "kind": "any" }))
				} else if tool_name.starts_with("table.") {
					let path = extract_concrete_table_path(grounding_input)?;
					let mut arguments = json!({ arg: path });
					if tool_name == "table.preview" {
						arguments["rows"] =
							Value::from(extract_row_limit(grounding_input).unwrap_or(5_u64));
					}
					if let Some(sheet) = extract_sheet_name(grounding_input) {
						arguments["sheet"] = Value::String(sheet);
					}
					Some(arguments)
				} else {
					let paths = extract_concrete_path_candidates(grounding_input);
					(paths.len() == 1).then(|| json!({ arg: paths[0].clone() }))
				}
			}
			GroundingStrategy::PatternBased => {
				let arg = grounding.grounding_argument.as_deref().unwrap_or("pattern");
				if arg == "query" {
					extract_web_query(grounding_input).map(|q| json!({ arg: q, "top_k": 5_u64 }))
				} else if tool_name == "fs.grep" {
					extract_grep_pattern(grounding_input).map(|p| json!({ arg: p }))
				} else {
					extract_glob_pattern(grounding_input).map(|p| json!({ arg: p }))
				}
			}
			GroundingStrategy::UrlBased => {
				extract_fetch_url(grounding_input).map(|url| json!({ "url": url }))
			}
			GroundingStrategy::CommandBased => {
				let arg = grounding.grounding_argument.as_deref().unwrap_or("command");
				if arg == "code" {
					extract_explicit_python_code(grounding_input)
						.map(|code| json!({ "code": code }))
				} else {
					extract_explicit_shell_command(grounding_input)
						.map(|cmd| json!({ "command": cmd }))
				}
			}
			GroundingStrategy::None => unreachable!(),
		};
		return result;
	}
	// Fallback for unregistered tools (e.g. skills without grounding metadata).
	match tool_name {
		"skill.install" | "skill.ensure_installed" => extract_skill_source_url(grounding_input)
			.map(|source_url| json!({ "source_url": source_url })),
		_ => None,
	}
}

fn validate_router_decision(
	loop_state: &LoopState,
	decision: NextStepDecision,
	catalog: Option<&ResourceCatalog>,
) -> Result<NextStepDecision, String> {
	match decision.action {
		NextStepAction::CallTool => {
			let Some(tool_name) = decision.tool_name.as_deref() else {
				return Err("call_tool decision omitted tool_name".to_string());
			};
			if !tool_visible(loop_state, tool_name) {
				return Err(format!(
					"tool `{tool_name}` is not visible in this round ({})",
					loop_state.visible_tools.join(", ")
				));
			}
			let Some(arguments) = decision.arguments.as_ref().and_then(Value::as_object) else {
				return Err(format!(
					"call_tool decision for `{tool_name}` omitted an arguments object"
				));
			};
			let required_keys = tool_required_argument_keys(tool_name, catalog);
			let missing_keys = required_keys
				.iter()
				.filter(|key| !arguments.contains_key(key.as_str()))
				.map(String::as_str)
				.collect::<Vec<_>>();
			if !missing_keys.is_empty() {
				return Err(format!(
					"call_tool decision for `{tool_name}` omitted required keys: {}",
					missing_keys.join(", ")
				));
			}
			if let Some(reason) =
				ungrounded_consumer_path_rejection_reason(loop_state, tool_name, arguments, catalog)
			{
				return Err(reason);
			}
			Ok(decision)
		}
		NextStepAction::FinalAnswer => Ok(decision),
		NextStepAction::AskUser | NextStepAction::Fail => Ok(decision),
	}
}

fn ungrounded_consumer_path_rejection_reason(
	loop_state: &LoopState,
	tool_name: &str,
	arguments: &serde_json::Map<String, Value>,
	catalog: Option<&ResourceCatalog>,
) -> Option<String> {
	let path = arguments
		.get("path")
		.or_else(|| arguments.get("file_path"))
		.and_then(Value::as_str)?;
	let resolved_lookup_path = loop_state
		.last_observation
		.as_ref()
		.and_then(|observation| {
			observation
				.data
				.get("resolved_path")
				.and_then(Value::as_str)
				.map(str::to_string)
		});
	let path_is_grounded = resolved_lookup_path.as_deref() == Some(path)
		|| extract_concrete_path_candidates(&loop_state.goal)
			.iter()
			.any(|candidate| candidate == path)
		|| extract_concrete_table_path(&loop_state.goal).as_deref() == Some(path)
		|| matches!(tool_name, "fs.inspect" | "fs.list_dir")
			&& path == loop_state.working_directory;

	if path_is_grounded {
		return None;
	}

	let requires_grounded_path =
		if let Some(grounding) = catalog.and_then(|c| c.lookup_grounding_metadata(tool_name)) {
			grounding.requires_grounded_path
		} else {
			false
		};
	if !requires_grounded_path || !tool_visible(loop_state, "fs.find") {
		return None;
	}

	Some(format!(
		"`{tool_name}` requires a grounded concrete path. The current round only mentions an unresolved file hint, so resolve it with `fs.find` before calling `{tool_name}`."
	))
}

fn tool_loop_prompt(context_projection: &ContextProjection, user_reply: Option<&str>) -> String {
	let projection_json =
		serde_json::to_string_pretty(context_projection).unwrap_or_else(|_| "{}".to_string());
	let prior_work_section = if context_projection.working_summary.is_empty() {
		String::new()
	} else {
		format!(
			"\n## Prior Work Summary\n{}\n",
			context_projection.working_summary
		)
	};
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
- For `code_exec`, you may call `python.run` when explicit Python code is present or when the task now requires one clearly bounded Python snippet for local computation over already grounded evidence. Prefer `python.run` over `command.run` for counting, aggregation, filtering, or transformation tasks. Only call `command.run` when the request includes one explicit shell command and the user is asking to execute it.
- When generating `python.run` arguments, keep the code short and self-contained. Prefer walking one grounded directory or reading one grounded path at execution time. Do not inline huge path arrays, copied directory listings, or large observation payloads into the code string.
- For `table_read`, prefer the first shortlisted `table.*` tool that matches the grounded table path.
- For `web_lookup`, use `web.search` when a concrete query is available.
- When the current request references concrete local files, directories, workspace paths, or shell-style inspection goals and `fs.*` tools are visible, gather grounded filesystem evidence before using `general.execute`.
- If the request only mentions a bare filename or fuzzy path hint like `grounding.rs` or `runtime.rs`, do not jump straight to `fs.read_text` or `table.preview`; resolve it with `fs.find` first unless the context already includes one grounded concrete path.
- Do not call `general.execute` only to speculate about which filesystem tools could be used. Prefer `fs.inspect`, `fs.list_dir`, `fs.read_text`, `fs.find`, or `fs.glob` when the current context already grounds one of them.
- If a filesystem or table consumer tool fails with `path_not_found` and `fs.find` is visible, prefer locating the target before failing the loop.
- If a lookup tool returns one `resolved_path` and a visible consumer tool can now accept that concrete path, you may continue with that consumer tool instead of stopping at the lookup step.
- When a filesystem, table, web, or python observation provides raw evidence but the user still needs explanation, comparison, or synthesis, prefer `general.execute` before emitting `final_answer`.
- Do not use `general.execute` as a placeholder for future work. If the user asked for a counted, aggregated, transformed, searched, or executed result and the current observations do not already contain that result, keep gathering evidence, use another visible tool, ask the user, or fail honestly.
- Never emit pseudo tool-call markup, future execution plans, or "let me run/use tool X" prose as if it were a completed result.
- When the latest observation already directly satisfies a bounded inspection or listing request, emit `final_answer` with a concise grounded reply that reuses the observation message instead of copying large raw payloads into JSON.
- Treat `intent_family`, `route_reason`, and the initial shortlist as weak seeds, not binding truth. If the current observation is insufficient, you may choose any better-fitting tool from `visible_tools`.
- Do not assume an ambiguous lookup must immediately become `ask_user` when another visible tool can still answer the task more directly.
- Use `ask_user` when the current information is still insufficient.
- Use `final_answer` only when the current context projection already proves the user request is satisfied.


{prior_work_section}Context projection:
{projection_json}

Current user follow-up:
{user_reply}
"#,
		prior_work_section = prior_work_section,
		projection_json = projection_json,
		user_reply = user_reply.unwrap_or("null"),
	)
}

#[cfg(test)]
pub(crate) fn tool_loop_prompt_for_test(
	context_projection: &ContextProjection,
	user_reply: Option<&str>,
) -> String {
	tool_loop_prompt(context_projection, user_reply)
}

fn deterministic_next_step(
	loop_state: &LoopState,
	user_reply: Option<&str>,
	catalog: Option<&ResourceCatalog>,
) -> NextStepDecision {
	if let Some(observation) = loop_state.last_observation.as_ref() {
		return next_step_from_observation(loop_state, observation, user_reply);
	}
	initial_next_step(loop_state, user_reply.unwrap_or(&loop_state.goal), catalog)
}

fn initial_next_step(
	loop_state: &LoopState,
	grounding_input: &str,
	catalog: Option<&ResourceCatalog>,
) -> NextStepDecision {
	let Some(tool_name) = bootstrap_tool_name(loop_state, grounding_input, catalog) else {
		return fail("tool loop cannot start without any visible tool".to_string());
	};
	bootstrap_tool_call(loop_state, tool_name, grounding_input, catalog)
}

fn next_step_from_observation(
	loop_state: &LoopState,
	observation: &ToolObservation,
	user_reply: Option<&str>,
) -> NextStepDecision {
	if observation.ok {
		if observation.terminal {
			return final_answer(
				summarize_observation(&loop_state.goal, observation).final_message,
			);
		}
		if let Some(next_decision) = follow_up_tool_after_observation(loop_state, observation) {
			return next_decision;
		}
		if observation.tool_name != "general.execute" && tool_visible(loop_state, "general.execute")
		{
			return call_tool(
				"general.execute",
				json!({}),
				"Use the general worker to synthesize the latest grounded observation into a user-facing answer without inventing new evidence.",
			);
		}
		return final_answer(summarize_observation(&loop_state.goal, observation).final_message);
	}
	if observation.error_type.as_deref() == Some("multiple_candidates") {
		if let Some(decision) = resume_from_awaiting_user_contract(loop_state, user_reply) {
			return decision;
		}
		return ask_user(ask_user_from_observation(&loop_state.goal, observation).final_message);
	}
	if let Some(next_decision) = recovery_tool_after_observation(loop_state, observation) {
		return next_decision;
	}
	fail(summarize_observation(&loop_state.goal, observation).final_message)
}

fn follow_up_tool_after_observation(
	loop_state: &LoopState,
	observation: &ToolObservation,
) -> Option<NextStepDecision> {
	match observation.tool_name.as_str() {
		"fs.find" => follow_up_after_fs_find(loop_state, observation),
		_ => None,
	}
}

fn follow_up_after_fs_find(
	loop_state: &LoopState,
	observation: &ToolObservation,
) -> Option<NextStepDecision> {
	let resolved_path = observation
		.data
		.get("resolved_path")
		.and_then(Value::as_str)?;
	let consumer_tool = deterministic_consumer_after_lookup(loop_state, "fs.find")?;
	Some(call_tool(
		consumer_tool,
		arguments_for_resolved_lookup_consumer(consumer_tool, resolved_path, &loop_state.goal),
		format!(
			"Reuse the unique `fs.find` result with `{consumer_tool}` because the current shortlist already established that consumer path."
		),
	))
}

fn recovery_tool_after_observation(
	loop_state: &LoopState,
	observation: &ToolObservation,
) -> Option<NextStepDecision> {
	let error_type = observation.error_type.as_deref()?;
	match error_type {
		"path_not_found" => recover_path_not_found_with_fs_find(loop_state, observation),
		_ => None,
	}
}

fn recover_path_not_found_with_fs_find(
	loop_state: &LoopState,
	observation: &ToolObservation,
) -> Option<NextStepDecision> {
	if observation.tool_name == "fs.find" || !tool_visible(loop_state, "fs.find") {
		return None;
	}
	let requested_name = extract_explicit_path_candidates(&loop_state.goal)
		.into_iter()
		.next()
		.and_then(|path| file_name_from_path(&path).or(Some(path)))?;
	Some(call_tool(
		"fs.find",
		json!({ "name": requested_name, "kind": "any" }),
		"Recover from a missing concrete path by locating the requested file inside the current workspace before giving up.",
	))
}

fn bootstrap_tool_name<'a>(
	loop_state: &'a LoopState,
	grounding_input: &str,
	catalog: Option<&ResourceCatalog>,
) -> Option<&'a str> {
	let shortlisted_tools = loop_state
		.route_decision
		.candidate_tools
		.iter()
		.map(String::as_str)
		.filter(|tool_name| tool_visible(loop_state, tool_name))
		.collect::<Vec<_>>();
	if let Some(tool_name) = shortlisted_tools
		.iter()
		.copied()
		.find(|tool_name| bootstrap_tool_matches_request(tool_name, grounding_input, catalog))
	{
		return Some(tool_name);
	}
	if prefers_advisory_bootstrap(grounding_input) && tool_visible(loop_state, "general.execute") {
		return Some("general.execute");
	}
	if prefers_advisory_bootstrap(grounding_input) {
		return shortlisted_tools
			.iter()
			.copied()
			.find(|tool_name| !is_execution_tool(tool_name))
			.or_else(|| {
				loop_state
					.visible_tools
					.iter()
					.map(String::as_str)
					.find(|tool_name| !is_execution_tool(tool_name))
			});
	}
	shortlisted_tools
		.into_iter()
		.find(|tool_name| bootstrap_tool_is_groundable(tool_name, grounding_input, catalog))
		.or_else(|| tool_visible(loop_state, "general.execute").then_some("general.execute"))
		.or_else(|| loop_state.visible_tools.first().map(String::as_str))
}

fn bootstrap_tool_matches_request(
	tool_name: &str,
	grounding_input: &str,
	catalog: Option<&ResourceCatalog>,
) -> bool {
	if let Some(grounding) = catalog.and_then(|c| c.lookup_grounding_metadata(tool_name)) {
		if !grounding.bootstrap_matchable {
			return false;
		}
		return match grounding.grounding_strategy {
			GroundingStrategy::PathBased => {
				if tool_name == "fs.find" {
					!extract_explicit_path_candidates(grounding_input).is_empty()
						&& ground_tool_arguments(tool_name, grounding_input).is_some()
				} else if tool_name.starts_with("table.") {
					extract_concrete_table_path(grounding_input).is_some()
						&& ground_tool_arguments(tool_name, grounding_input).is_some()
				} else {
					!extract_concrete_path_candidates(grounding_input).is_empty()
						&& ground_tool_arguments(tool_name, grounding_input).is_some()
				}
			}
			GroundingStrategy::PatternBased => {
				if tool_name == "fs.grep" {
					extract_grep_pattern(grounding_input).is_some()
				} else if tool_name == "web.search" {
					extract_web_query(grounding_input).is_some()
				} else {
					extract_glob_pattern(grounding_input).is_some()
				}
			}
			GroundingStrategy::UrlBased => extract_fetch_url(grounding_input).is_some(),
			GroundingStrategy::CommandBased => {
				if tool_name == "python.run" {
					grounded_python_code_allows_execution(grounding_input)
				} else {
					grounded_shell_command_allows_execution(grounding_input)
				}
			}
			GroundingStrategy::None => {
				bootstrap_tool_is_groundable(tool_name, grounding_input, catalog)
			}
		};
	}
	// Fallback for unregistered tools.
	bootstrap_tool_is_groundable(tool_name, grounding_input, catalog)
}

fn bootstrap_tool_is_groundable(
	tool_name: &str,
	grounding_input: &str,
	catalog: Option<&ResourceCatalog>,
) -> bool {
	if let Some(grounding) = catalog.and_then(|c| c.lookup_grounding_metadata(tool_name)) {
		return match grounding.grounding_strategy {
			GroundingStrategy::PathBased => {
				if tool_name == "fs.find" {
					!extract_explicit_path_candidates(grounding_input).is_empty()
						&& ground_tool_arguments(tool_name, grounding_input).is_some()
				} else if tool_name.starts_with("table.") {
					extract_concrete_table_path(grounding_input).is_some()
						&& ground_tool_arguments(tool_name, grounding_input).is_some()
				} else {
					!extract_concrete_path_candidates(grounding_input).is_empty()
						&& ground_tool_arguments(tool_name, grounding_input).is_some()
				}
			}
			GroundingStrategy::PatternBased => {
				if tool_name == "fs.grep" {
					extract_grep_pattern(grounding_input).is_some()
				} else if tool_name == "web.search" {
					extract_web_query(grounding_input).is_some()
				} else {
					extract_glob_pattern(grounding_input).is_some()
				}
			}
			GroundingStrategy::UrlBased => extract_fetch_url(grounding_input).is_some(),
			GroundingStrategy::CommandBased => {
				ground_tool_arguments(tool_name, grounding_input).is_some()
			}
			GroundingStrategy::None => {
				tool_required_argument_keys(tool_name, catalog).is_empty()
					|| ground_tool_arguments(tool_name, grounding_input).is_some()
			}
		};
	}
	// Fallback for unregistered tools.
	tool_required_argument_keys(tool_name, catalog).is_empty()
		|| ground_tool_arguments(tool_name, grounding_input).is_some()
}

fn prefers_advisory_bootstrap(grounding_input: &str) -> bool {
	explanatory_shell_command_request(grounding_input)
		|| explanatory_python_code_request(grounding_input)
}

fn is_execution_tool(tool_name: &str) -> bool {
	matches!(tool_name, "command.run" | "python.run")
}

fn bootstrap_tool_call(
	loop_state: &LoopState,
	tool_name: &str,
	grounding_input: &str,
	catalog: Option<&ResourceCatalog>,
) -> NextStepDecision {
	if !tool_visible(loop_state, tool_name) {
		return fail(format!(
			"tool loop cannot see the shortlisted tool `{tool_name}`"
		));
	}
	if tool_required_argument_keys(tool_name, catalog).is_empty() {
		return call_tool(
			tool_name,
			json!({}),
			"Use the currently visible tool hint without adding any extra semantic routing.",
		);
	}
	match ground_tool_arguments(tool_name, grounding_input) {
		Some(arguments) => call_tool(
			tool_name,
			arguments,
			"Ground the currently selected tool from the current request without adding extra follow-up routing.",
		),
		None => ask_user(missing_argument_message(tool_name)),
	}
}

pub(crate) fn ground_tool_arguments(tool_name: &str, grounding_input: &str) -> Option<Value> {
	match tool_name {
		"fs.exists" | "fs.inspect" | "fs.list_dir" | "fs.read_text" => {
			extract_concrete_path_candidates(grounding_input)
				.into_iter()
				.next()
				.map(|path| json!({ "path": path }))
		}
		"fs.find" => extract_explicit_path_candidates(grounding_input)
			.into_iter()
			.next()
			.map(|name| json!({ "name": name, "kind": "any" })),
		"fs.glob" => {
			extract_glob_pattern(grounding_input).map(|pattern| json!({ "pattern": pattern }))
		}
		// The following arms are temporary hardcoded integrations added by EPIC-0.
		// They will be migrated to descriptor-driven grounding under EPIC-5.
		"fs.grep" => {
			extract_grep_pattern(grounding_input).map(|pattern| json!({ "pattern": pattern }))
		}
		"fs.edit" | "fs.write" => extract_concrete_path_candidates(grounding_input)
			.into_iter()
			.next()
			.map(|path| json!({ "file_path": path })),
		"table.inspect" | "table.list_sheets" | "table.preview" | "table.schema" => {
			let path = extract_concrete_table_path(grounding_input)?;
			let mut arguments = json!({ "path": path });
			if tool_name == "table.preview" {
				arguments["rows"] =
					Value::from(extract_row_limit(grounding_input).unwrap_or(5_u64));
			}
			if let Some(sheet) = extract_sheet_name(grounding_input) {
				arguments["sheet"] = Value::String(sheet);
			}
			Some(arguments)
		}
		"web.search" => extract_web_query(grounding_input)
			.map(|query| json!({ "query": query, "top_k": 5_u64 })),
		"web.fetch" => extract_fetch_url(grounding_input).map(|url| json!({ "url": url })),
		"command.run" => extract_explicit_shell_command(grounding_input)
			.map(|command| json!({ "command": command })),
		"python.run" => {
			extract_explicit_python_code(grounding_input).map(|code| json!({ "code": code }))
		}
		"skill.install" | "skill.ensure_installed" => extract_skill_source_url(grounding_input)
			.map(|source_url| json!({ "source_url": source_url })),
		_ => None,
	}
}

fn deterministic_consumer_after_lookup<'a>(
	loop_state: &'a LoopState,
	lookup_tool: &str,
) -> Option<&'a str> {
	let shortlist = loop_state
		.route_decision
		.candidate_tools
		.iter()
		.map(String::as_str)
		.filter(|tool_name| tool_visible(loop_state, tool_name))
		.collect::<Vec<_>>();
	let lookup_index = shortlist
		.iter()
		.position(|tool_name| *tool_name == lookup_tool)?;
	shortlist[..lookup_index]
		.iter()
		.rev()
		.find_map(|tool_name| lookup_consumer_tool(tool_name))
		.or_else(|| {
			shortlist
				.get(lookup_index + 1)
				.and_then(|tool_name| lookup_consumer_tool(tool_name))
		})
}

fn lookup_consumer_tool(tool_name: &str) -> Option<&str> {
	matches!(
		tool_name,
		"fs.read_text"
			| "fs.inspect"
			| "fs.list_dir"
			| "table.inspect"
			| "table.preview"
			| "table.schema"
			| "table.list_sheets"
	)
	.then_some(tool_name)
}

fn arguments_for_resolved_lookup_consumer(
	tool_name: &str,
	resolved_path: &str,
	goal: &str,
) -> Value {
	let mut arguments = json!({ "path": resolved_path });
	if tool_name == "table.preview" {
		arguments["rows"] = Value::from(extract_row_limit(goal).unwrap_or(5_u64));
	}
	if matches!(
		tool_name,
		"table.inspect" | "table.list_sheets" | "table.preview" | "table.schema"
	) && let Some(sheet) = extract_sheet_name(goal)
	{
		arguments["sheet"] = Value::String(sheet);
	}
	arguments
}

fn should_force_ask_user_for_ambiguous_stagnation(
	loop_state: &LoopState,
	user_reply: Option<&str>,
) -> bool {
	loop_state.awaiting_user.is_none()
		&& loop_state.ambiguity_requires_ask_user(user_reply.unwrap_or(&loop_state.goal))
}

fn missing_argument_message(tool_name: &str) -> String {
	match tool_name {
		"fs.exists" => "I need a concrete path before I can check whether it exists.".to_string(),
		"fs.inspect" => {
			"I need a concrete path before I can inspect that filesystem target.".to_string()
		}
		"fs.list_dir" => {
			"I need a concrete directory path before I can list that directory.".to_string()
		}
		"fs.read_text" => "I need a concrete file path before I can read that file.".to_string(),
		"fs.find" => {
			"I need a concrete file or directory name before I can search for it.".to_string()
		}
		"fs.glob" => {
			"I need a concrete glob pattern before I can search the filesystem.".to_string()
		}
		"table.inspect" | "table.list_sheets" | "table.preview" | "table.schema" => {
			"I need a concrete csv/tsv/xlsx path before I can inspect that table.".to_string()
		}
		"web.search" => "I need a concrete search query before I can search the web.".to_string(),
		"command.run" => {
			"Please send one explicit shell command in inline code or a fenced bash block."
				.to_string()
		}
		"python.run" => {
			"Please send explicit Python code in a fenced block or inline code snippet.".to_string()
		}
		"skill.install" | "skill.ensure_installed" => {
			"I need a concrete skill source URL before I can install that skill.".to_string()
		}
		other => {
			format!("I need more concrete input before I can call the selected tool `{other}`.")
		}
	}
}

fn resume_from_awaiting_user_contract(
	loop_state: &LoopState,
	user_reply: Option<&str>,
) -> Option<NextStepDecision> {
	let user_reply = user_reply?.trim();
	let payload = loop_state.awaiting_user.as_ref()?;
	let selected_candidate = payload.selected_candidate(user_reply)?;
	let directive = payload.resume_directive.as_ref()?;
	match directive {
		crate::runtime_loop::AskUserResumeDirective::RepeatToolWithSelectedCandidate {
			tool_name,
			argument_key,
		} => {
			if !tool_visible(loop_state, tool_name) {
				return None;
			}
			Some(call_tool(
				tool_name,
				json!({ argument_key.clone(): selected_candidate }),
				"Resume the paused loop by replaying the same grounded tool with the user-selected candidate.",
			))
		}
	}
}

pub(crate) fn tool_required_argument_keys(
	tool_name: &str,
	catalog: Option<&ResourceCatalog>,
) -> Vec<String> {
	if let Some(grounding) = catalog.and_then(|c| c.lookup_grounding_metadata(tool_name))
		&& grounding.grounding_strategy != GroundingStrategy::None
	{
		return grounding
			.grounding_argument
			.as_ref()
			.map(|arg| vec![arg.clone()])
			.unwrap_or_default();
	}
	match tool_name {
		"skill.install" | "skill.ensure_installed" => vec!["source_url".to_string()],
		_ => Vec::new(),
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

fn log_tool_loop_warning(message: &str, fields: impl IntoIterator<Item = (&'static str, String)>) {
	let mut record = LogRecord::new("roku-agent-runtime", LogLevel::Warn, message);
	for (key, value) in fields {
		record = record.with_field(key, value);
	}
	let _ = emit_global_log(record);
}

fn truncate_for_log(value: &str, max_chars: usize) -> String {
	let char_count = value.chars().count();
	if char_count <= max_chars {
		return value.to_string();
	}
	let truncated = value
		.chars()
		.take(max_chars.saturating_sub(3))
		.collect::<String>();
	format!("{truncated}...")
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

	use roku_common_types::{ResourceSelector, RuntimeMemorySections};
	use roku_plugin_catalog::ResourceCatalog;
	use roku_plugin_llm::{
		GenerationRequest, LlmProvider, LlmRouter, ModelProfile, ProviderCallError,
		ProviderResponse, RiskTier, RoutingPolicy,
	};
	use roku_plugin_skills::SkillRegistry;
	use roku_plugin_tools::{ToolCatalogConfig, build_resource_catalog};
	use serde_json::{Value, json};

	use super::{decide_tool_loop_next_step, tool_loop_prompt};
	use crate::NextStepRuntimeConfig;
	use crate::router::{IntentFamily, RouteDecision, RouteRisk};
	use crate::runtime_loop::{
		ContextProjection, LoopContext, LoopState, NextStepAction, NextStepDecision,
		StepObservation, ToolObservation, build_context_projection,
		loop_state::AmbiguityStagnation, step_record::StepRecord,
	};

	fn test_catalog() -> ResourceCatalog {
		build_resource_catalog(&SkillRegistry::disabled(), &ToolCatalogConfig::default())
	}

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
		LoopState::new("loop-req-1", &context)
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
		LoopState::new("loop-fs-1", &context)
	}

	fn sample_bootstrap_loop_state(
		goal: &str,
		candidate_tools: Vec<&str>,
		visible_tools: Vec<&str>,
	) -> LoopState {
		let cwd = env::current_dir()
			.expect("cwd should resolve for tests")
			.display()
			.to_string();
		let context = LoopContext {
			request_id: "req-bootstrap-1".to_string(),
			session_id: "session-bootstrap-1".to_string(),
			goal: goal.to_string(),
			workspace_root: cwd.clone(),
			working_directory: cwd,
			visible_tools: visible_tools.into_iter().map(str::to_string).collect(),
			bound_resources: Vec::new(),
			route_decision: RouteDecision::new(
				IntentFamily::Unknown,
				0.81,
				false,
				RouteRisk::Low,
				candidate_tools.into_iter().map(str::to_string).collect(),
				Vec::new(),
				Vec::new(),
				"bootstrap bias test",
			),
			last_observation: None,
		};
		LoopState::new("loop-bootstrap-1", &context)
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
	fn prompt_projects_working_summary_separately_from_history_digest() {
		let mut loop_state = sample_loop_state();
		loop_state.working_summary =
			"PROMPT_WORKING_SUMMARY_ONLY::pending repo blocker".to_string();
		let observation = ToolObservation {
			ok: true,
			tool_name: "inventory.describe".to_string(),
			error_type: None,
			terminal: false,
			data: json!({ "runtime_mode": "deterministic" }),
			message: "deterministic placeholder only: inventory summary was not generated by a live runtime".to_string(),
		};
		let interpreted =
			crate::runtime_loop::interpret_observation(&loop_state, observation.clone(), None);
		loop_state.record_step(StepRecord::tool_call(
			1,
			NextStepDecision {
				action: NextStepAction::CallTool,
				tool_name: Some("inventory.describe".to_string()),
				arguments: Some(json!({})),
				reason: "Use the inventory tool first.".to_string(),
				final_message: None,
			},
			loop_state.visible_tools.clone(),
			loop_state.bound_resources.clone(),
			json!({
				"ok": true,
				"terminal": false,
				"message": "deterministic placeholder only: inventory summary was not generated by a live runtime",
				"data": observation.data.clone(),
			}),
			StepObservation::Tool(observation),
			interpreted.clone(),
			Some(15),
			interpreted.remaining_step_budget,
			interpreted.remaining_recovery_budget,
			"/workspace",
		));
		let projection = build_context_projection(&loop_state, &RuntimeMemorySections::default());
		let prompt = tool_loop_prompt(&projection, None);

		assert!(
			prompt.contains("## Prior Work Summary"),
			"non-empty working_summary should produce a Prior Work Summary section"
		);
		assert!(
			prompt.contains("PROMPT_WORKING_SUMMARY_ONLY::pending repo blocker"),
			"working_summary content should appear in the prompt"
		);
		assert!(prompt.contains("\"working_summary\":"));
		assert!(prompt.contains("\"history_digest\":"));
		assert!(prompt.contains("step 1"));
		assert!(prompt.contains("inventory.describe"));
		assert!(!prompt.contains("\"started_at\""));
		assert!(!prompt.contains("\"finished_at\""));
		assert!(!prompt.contains("Visible tool hints:"));
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
		let projection: ContextProjection =
			build_context_projection(&loop_state, &RuntimeMemorySections::default());
		let (router, prompts) = router_with_responses(vec![json!({
			"action": "call_tool",
			"tool_name": "inventory.describe",
			"arguments": {},
			"reason": "Collect one more grounded inventory observation.",
			"final_message": null
		})]);

		let decision = decide_tool_loop_next_step(
			&loop_state,
			&projection,
			Some(&router),
			None,
			&NextStepRuntimeConfig::default(),
			Some(&test_catalog()),
		);

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
	fn resolved_filesystem_find_can_continue_into_the_consumer_tool() {
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
		let projection = build_context_projection(&loop_state, &RuntimeMemorySections::default());

		let decision = decide_tool_loop_next_step(
			&loop_state,
			&projection,
			None,
			None,
			&NextStepRuntimeConfig::default(),
			Some(&test_catalog()),
		);

		assert_eq!(
			decision.action,
			crate::runtime_loop::NextStepAction::CallTool
		);
		assert_eq!(decision.tool_name.as_deref(), Some("fs.read_text"));
		assert!(
			decision
				.reason
				.contains("Reuse the unique `fs.find` result")
		);
	}

	#[test]
	fn multiple_candidate_filesystem_match_asks_user_without_a_live_router() {
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
		let projection = build_context_projection(&loop_state, &RuntimeMemorySections::default());

		let decision = decide_tool_loop_next_step(
			&loop_state,
			&projection,
			None,
			None,
			&NextStepRuntimeConfig::default(),
			Some(&test_catalog()),
		);

		assert_eq!(
			decision.action,
			crate::runtime_loop::NextStepAction::AskUser
		);
		assert!(
			decision
				.final_message
				.as_deref()
				.is_some_and(|message| message.contains("Cargo.toml"))
		);
	}

	#[test]
	fn live_loop_forces_ask_user_after_ambiguous_lookup_stagnates() {
		let mut loop_state = sample_filesystem_loop_state();
		let cwd = env::current_dir().expect("cwd should resolve for tests");
		let root_manifest = cwd.join("Cargo.toml").display().to_string();
		let nested_manifest = cwd
			.join("crates/roku-agent-runtime/Cargo.toml")
			.display()
			.to_string();
		let goal = loop_state.goal.clone();
		loop_state.note_grounding_input(&goal);
		loop_state.last_observation = Some(ToolObservation {
			ok: false,
			tool_name: "fs.find".to_string(),
			error_type: Some("multiple_candidates".to_string()),
			terminal: false,
			data: json!({
				"name": "Cargo.toml",
				"match_count": 2,
				"matches": [root_manifest.clone(), nested_manifest.clone()],
				"exact_match_count": 2,
				"fuzzy_match_count": 0,
				"match_mode": "ambiguous_exact",
			}),
			message: "Found 2 matching candidates for `Cargo.toml`.".to_string(),
		});
		loop_state.ambiguity_stagnation = Some(AmbiguityStagnation {
			tool_name: "fs.find".to_string(),
			candidate_fingerprint: [nested_manifest, root_manifest].join("|"),
			match_count: 2,
			explicit_grounding_fingerprint: loop_state
				.latest_explicit_grounding_fingerprint
				.clone(),
			streak: 2,
		});
		let projection = build_context_projection(&loop_state, &RuntimeMemorySections::default());
		let (router, _prompts) = router_with_responses(vec![json!({
			"action": "call_tool",
			"tool_name": "general.execute",
			"arguments": {},
			"reason": "Try to explain anyway.",
			"final_message": null
		})]);

		let decision = decide_tool_loop_next_step(
			&loop_state,
			&projection,
			Some(&router),
			None,
			&NextStepRuntimeConfig::default(),
			Some(&test_catalog()),
		);

		assert_eq!(
			decision.action,
			crate::runtime_loop::NextStepAction::AskUser
		);
		assert!(
			decision
				.final_message
				.as_deref()
				.is_some_and(|message| message.contains("Cargo.toml"))
		);
	}

	#[test]
	fn bootstrap_respects_seed_order_without_natural_language_tool_override() {
		let loop_state = sample_bootstrap_loop_state(
			"Read Cargo.toml and summarize the workspace layout.",
			vec!["fs.find", "fs.read_text"],
			vec!["fs.find", "fs.read_text", "general.execute"],
		);
		let projection = build_context_projection(&loop_state, &RuntimeMemorySections::default());

		let decision = decide_tool_loop_next_step(
			&loop_state,
			&projection,
			None,
			None,
			&NextStepRuntimeConfig::default(),
			Some(&test_catalog()),
		);

		assert_eq!(
			decision.action,
			crate::runtime_loop::NextStepAction::CallTool
		);
		assert_eq!(decision.tool_name.as_deref(), Some("fs.find"));
	}

	#[test]
	fn bootstrap_does_not_amplify_explanatory_python_seed_into_execution() {
		let loop_state = sample_bootstrap_loop_state(
			"Explain what this Python code does: `print(1)`",
			vec!["python.run"],
			vec!["python.run", "general.execute"],
		);
		let projection = build_context_projection(&loop_state, &RuntimeMemorySections::default());

		let decision = decide_tool_loop_next_step(
			&loop_state,
			&projection,
			None,
			None,
			&NextStepRuntimeConfig::default(),
			Some(&test_catalog()),
		);

		assert_eq!(
			decision.action,
			crate::runtime_loop::NextStepAction::CallTool
		);
		assert_eq!(decision.tool_name.as_deref(), Some("general.execute"));
	}

	#[test]
	fn bootstrap_prefers_inspect_for_explicit_paths_without_a_clear_action() {
		let loop_state = sample_bootstrap_loop_state(
			"Cargo.toml 这个文件帮我看看情况。",
			vec!["fs.inspect", "fs.read_text", "fs.list_dir"],
			vec![
				"fs.inspect",
				"fs.read_text",
				"fs.list_dir",
				"general.execute",
			],
		);
		let projection = build_context_projection(&loop_state, &RuntimeMemorySections::default());

		let decision = decide_tool_loop_next_step(
			&loop_state,
			&projection,
			None,
			None,
			&NextStepRuntimeConfig::default(),
			Some(&test_catalog()),
		);

		assert_eq!(
			decision.action,
			crate::runtime_loop::NextStepAction::CallTool
		);
		assert_eq!(decision.tool_name.as_deref(), Some("fs.inspect"));
	}

	#[test]
	fn bootstrap_uses_lookup_first_seed_for_non_concrete_basenames() {
		let loop_state = sample_bootstrap_loop_state(
			"我是说，帮我看看cmd那个crate下的runtime.rs，里面的第100行是什么内容？输出出来",
			vec!["fs.find", "fs.glob", "fs.inspect"],
			vec!["fs.find", "fs.glob", "fs.inspect", "general.execute"],
		);
		let projection = build_context_projection(&loop_state, &RuntimeMemorySections::default());

		let decision = decide_tool_loop_next_step(
			&loop_state,
			&projection,
			None,
			None,
			&NextStepRuntimeConfig::default(),
			Some(&test_catalog()),
		);

		assert_eq!(
			decision.action,
			crate::runtime_loop::NextStepAction::CallTool
		);
		assert_eq!(decision.tool_name.as_deref(), Some("fs.find"));
	}

	#[test]
	fn router_aligned_lookup_arguments_prefer_explicit_resource_tokens() {
		let loop_state = sample_bootstrap_loop_state(
			"我是说，帮我看看cmd那个crate下的runtime.rs，里面的第100行是什么内容？输出出来",
			vec!["fs.find", "fs.glob", "fs.inspect"],
			vec![
				"fs.find",
				"fs.glob",
				"fs.inspect",
				"fs.read_text",
				"general.execute",
			],
		);
		let projection = build_context_projection(&loop_state, &RuntimeMemorySections::default());
		let (router, _prompts) = router_with_responses(vec![json!({
			"action": "call_tool",
			"tool_name": "fs.find",
			"arguments": { "name": "cmd", "kind": "any" },
			"reason": "Find the cmd crate path first.",
			"final_message": null
		})]);

		let decision = decide_tool_loop_next_step(
			&loop_state,
			&projection,
			Some(&router),
			None,
			&NextStepRuntimeConfig::default(),
			Some(&test_catalog()),
		);

		assert_eq!(
			decision.action,
			crate::runtime_loop::NextStepAction::CallTool
		);
		assert_eq!(decision.tool_name.as_deref(), Some("fs.find"));
		assert_eq!(
			decision
				.arguments
				.as_ref()
				.and_then(|value| value.get("name"))
				.and_then(Value::as_str),
			Some("runtime.rs")
		);
	}

	#[test]
	fn router_rejects_consumer_reads_for_unresolved_basenames() {
		let loop_state = sample_bootstrap_loop_state(
			"帮我看看 grounding.rs 这个文件主要是做啥用的吧，一句话总结下",
			vec!["fs.find", "fs.glob", "fs.inspect"],
			vec![
				"fs.find",
				"fs.glob",
				"fs.inspect",
				"fs.read_text",
				"general.execute",
			],
		);
		let projection = build_context_projection(&loop_state, &RuntimeMemorySections::default());
		let (router, _prompts) = router_with_responses(vec![
			json!({
				"action": "call_tool",
				"tool_name": "fs.read_text",
				"arguments": { "path": "/Users/jojo/cjj_project/Roku/grounding.rs" },
				"reason": "Read the file directly.",
				"final_message": null
			}),
			json!({
				"action": "call_tool",
				"tool_name": "fs.find",
				"arguments": { "name": "grounding.rs", "kind": "any" },
				"reason": "Resolve the basename first.",
				"final_message": null
			}),
		]);

		let decision = decide_tool_loop_next_step(
			&loop_state,
			&projection,
			Some(&router),
			None,
			&NextStepRuntimeConfig::default(),
			Some(&test_catalog()),
		);

		assert_eq!(
			decision.action,
			crate::runtime_loop::NextStepAction::CallTool
		);
		assert_eq!(decision.tool_name.as_deref(), Some("fs.find"));
	}

	#[test]
	fn bootstrap_keeps_python_run_for_explicit_execution_requests() {
		let loop_state = sample_bootstrap_loop_state(
			"Run this Python code: `print(1)`",
			vec!["python.run"],
			vec!["python.run", "general.execute"],
		);
		let projection = build_context_projection(&loop_state, &RuntimeMemorySections::default());

		let decision = decide_tool_loop_next_step(
			&loop_state,
			&projection,
			None,
			None,
			&NextStepRuntimeConfig::default(),
			Some(&test_catalog()),
		);

		assert_eq!(
			decision.action,
			crate::runtime_loop::NextStepAction::CallTool
		);
		assert_eq!(decision.tool_name.as_deref(), Some("python.run"));
	}

	#[test]
	fn bootstrap_does_not_amplify_explanatory_shell_seed_into_execution() {
		let loop_state = sample_bootstrap_loop_state(
			"Explain what the shell command `pwd` does, but do not run it.",
			vec!["command.run"],
			vec!["command.run", "general.execute"],
		);
		let projection = build_context_projection(&loop_state, &RuntimeMemorySections::default());

		let decision = decide_tool_loop_next_step(
			&loop_state,
			&projection,
			None,
			None,
			&NextStepRuntimeConfig::default(),
			Some(&test_catalog()),
		);

		assert_eq!(
			decision.action,
			crate::runtime_loop::NextStepAction::CallTool
		);
		assert_eq!(decision.tool_name.as_deref(), Some("general.execute"));
	}
}
