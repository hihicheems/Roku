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

use std::collections::HashMap;

use roku_common_types::{ExtractionHint, GroundingStrategy};
use roku_common_types::{LogLevel, LogRecord, emit_global_log};
use roku_plugin_llm::{
	GenerationRequest, LlmRouter, RiskTier, StreamChunk, ToolCallBlock, ToolDefinition,
};
use roku_plugin_tools::ResourceCatalog;
use serde_json::{Value, json};

use crate::runtime_config::NextStepRuntimeConfig;
use crate::runtime_loop::grounding::{
	explanatory_python_code_request, explanatory_shell_command_request,
	extract_concrete_path_candidates, extract_concrete_table_path,
	extract_explicit_path_candidates, extract_explicit_python_code, extract_explicit_shell_command,
	extract_fetch_url, extract_glob_pattern, extract_grep_pattern, extract_path_candidates,
	extract_row_limit, extract_sheet_name, extract_skill_source_url, extract_web_query,
};
use crate::runtime_loop::{
	ContextProjection, LoopEvent, LoopEventSender, LoopState, NextStepAction, NextStepDecision,
	ToolObservation, ask_user::ask_user_from_observation, file_name_from_path,
	summarize_observation,
};

pub(crate) async fn decide_tool_loop_next_step(
	loop_state: &LoopState,
	context_projection: &ContextProjection,
	router: Option<&LlmRouter>,
	user_reply: Option<&str>,
	config: &NextStepRuntimeConfig,
	catalog: Option<&ResourceCatalog>,
	event_sender: Option<&LoopEventSender>,
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
			event_sender,
		)
		.await
	{
		return decision;
	}
	deterministic_next_step(loop_state, user_reply, catalog)
}

async fn decide_with_router(
	loop_state: &LoopState,
	context_projection: &ContextProjection,
	router: &LlmRouter,
	user_reply: Option<&str>,
	config: &NextStepRuntimeConfig,
	catalog: Option<&ResourceCatalog>,
	event_sender: Option<&LoopEventSender>,
) -> Option<NextStepDecision> {
	let tool_definitions = build_tool_definitions(&context_projection.visible_tools, catalog);
	let env_context = crate::runtime_loop::environment::format_environment_context(
		crate::runtime_loop::environment::probe_environment(),
	);
	let system_prompt = format!(
		"You are Roku, a coding assistant. Use the provided tools to accomplish the user's task. \
		 Call final_answer when the task is complete.\n\n\
		 # Environment\n{env_context}"
	);
	let request = GenerationRequest {
		system_prompt: Some(system_prompt),
		prompt: tool_loop_prompt(context_projection, user_reply),
		expected_output_tokens: config.expected_output_tokens,
		risk_tier: RiskTier::Low,
		preferred_provider: None,
		budget_tokens_remaining: config.budget_tokens_remaining,
		budget_cost_remaining_usd: config.budget_cost_remaining_usd,
		tools: if tool_definitions.is_empty() {
			None
		} else {
			Some(tool_definitions)
		},
	};

	if let Some(sender) = event_sender {
		// Streaming path: accumulate tool_use chunks from the stream.
		let step = loop_state.step_index;
		let (tx, mut rx) = tokio::sync::mpsc::channel::<StreamChunk>(64);

		let event_tx = sender.clone();
		let accumulator = tokio::spawn(async move {
			let mut accumulated_tool_calls: Vec<ToolCallBlock> = Vec::new();
			let mut pending_by_id: HashMap<String, (String, String)> = HashMap::new();
			while let Some(chunk) = rx.recv().await {
				match chunk {
					StreamChunk::TextDelta { text } => {
						let _ = event_tx.send(LoopEvent::LlmTextDelta { step, text });
					}
					StreamChunk::ToolCallStart { id, name } => {
						pending_by_id.insert(id, (name, String::new()));
					}
					StreamChunk::ToolCallDelta {
						id,
						arguments_chunk,
					} => {
						if let Some((_, args)) = pending_by_id.get_mut(&id) {
							args.push_str(&arguments_chunk);
						}
					}
					StreamChunk::ToolCallDone { id } => {
						if let Some((name, args_str)) = pending_by_id.remove(&id) {
							let arguments = serde_json::from_str(&args_str).unwrap_or(Value::Null);
							accumulated_tool_calls.push(ToolCallBlock {
								id,
								name,
								arguments,
							});
						}
					}
					StreamChunk::Done { .. } => {}
				}
			}
			accumulated_tool_calls
		});

		let llm_result = router.generate_streaming(&request, tx).await;
		let accumulated_tool_calls = accumulator.await.unwrap_or_default();
		let _ = sender.send(LoopEvent::LlmDecisionComplete { step });

		let llm_response = match llm_result {
			Ok(r) if r.finish_reason.as_deref() == Some("length") => {
				log_tool_loop_warning(
					"streaming response truncated (finish_reason=length)",
					[("run_id", loop_state.run_id.clone())],
				);
				return None;
			}
			Ok(r) => r,
			Err(error) => {
				log_tool_loop_warning(
					"next-step model did not return a usable response",
					[
						("run_id", loop_state.run_id.clone()),
						("error", error.to_string()),
					],
				);
				return None;
			}
		};
		let _ = llm_response;

		if let Some(decision) = NextStepDecision::from_tool_calls(&accumulated_tool_calls) {
			let decision = align_router_tool_arguments(
				decision,
				user_reply.unwrap_or(&loop_state.goal),
				catalog,
			);
			return match validate_router_decision(loop_state, decision, catalog) {
				Ok(decision) => Some(decision),
				Err(reason) => {
					log_tool_loop_warning(
						"streaming tool_use decision rejected by validation",
						[("run_id", loop_state.run_id.clone()), ("reason", reason)],
					);
					None
				}
			};
		}

		log_tool_loop_warning(
			"streaming response contained no usable tool_use blocks",
			[("run_id", loop_state.run_id.clone())],
		);
		return None;
	}

	// Non-streaming path: use generate() which returns native tool_calls.
	let llm_response = match router.generate(&request).await {
		Ok(r) => r,
		Err(error) => {
			log_tool_loop_warning(
				"next-step model did not return a usable response",
				[
					("run_id", loop_state.run_id.clone()),
					("error", error.to_string()),
				],
			);
			return None;
		}
	};

	if let Some(tool_calls) = &llm_response.tool_calls
		&& !tool_calls.is_empty()
		&& let Some(decision) = NextStepDecision::from_tool_calls(tool_calls)
	{
		let decision =
			align_router_tool_arguments(decision, user_reply.unwrap_or(&loop_state.goal), catalog);
		return match validate_router_decision(loop_state, decision, catalog) {
			Ok(decision) => Some(decision),
			Err(reason) => {
				log_tool_loop_warning(
					"native tool_use decision rejected by validation",
					[("run_id", loop_state.run_id.clone()), ("reason", reason)],
				);
				None
			}
		};
	}

	log_tool_loop_warning(
		"next-step model did not return a usable tool_use response",
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
		],
	);
	None
}

fn align_router_tool_arguments(
	decision: NextStepDecision,
	_grounding_input: &str,
	_catalog: Option<&ResourceCatalog>,
) -> NextStepDecision {
	decision
}

fn resolve_extraction_hint(grounding: &roku_common_types::ToolGroundingContract) -> ExtractionHint {
	if grounding.extraction_hint != ExtractionHint::Default {
		return grounding.extraction_hint;
	}
	// Infer from strategy for backwards compat.
	match grounding.grounding_strategy {
		GroundingStrategy::PathBased => ExtractionHint::ConcretePath,
		GroundingStrategy::PatternBased => ExtractionHint::GlobPattern,
		GroundingStrategy::UrlBased => ExtractionHint::FetchUrl,
		GroundingStrategy::CommandBased => ExtractionHint::ShellCommand,
		GroundingStrategy::None => ExtractionHint::Default,
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
			Ok(decision)
		}
		NextStepAction::CallTools => {
			let Some(tool_calls) = decision.tool_calls.as_ref() else {
				return Err("call_tools decision omitted tool_calls array".to_string());
			};
			for entry in tool_calls {
				if !tool_visible(loop_state, &entry.tool_name) {
					return Err(format!(
						"tool `{}` is not visible in this round ({})",
						entry.tool_name,
						loop_state.visible_tools.join(", ")
					));
				}
			}
			Ok(decision)
		}
		NextStepAction::FinalAnswer => Ok(decision),
		NextStepAction::AskUser | NextStepAction::Fail => Ok(decision),
	}
}

pub(crate) fn tool_loop_prompt(
	context_projection: &ContextProjection,
	user_reply: Option<&str>,
) -> String {
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
		r#"{prior_work_section}Context projection:
{projection_json}

Current user follow-up:
{user_reply}
"#,
		prior_work_section = prior_work_section,
		projection_json = projection_json,
		user_reply = user_reply.unwrap_or("null"),
	)
}

/// Build native tool definitions from visible tools for the LLM provider.
///
/// Converts CatalogDescriptor data into ToolDefinition objects and appends
/// special pseudo-tools (final_answer, ask_user, fail) so the model can
/// express all NextStepAction variants through native tool_use.
fn build_tool_definitions(
	visible_tools: &[String],
	catalog: Option<&ResourceCatalog>,
) -> Vec<ToolDefinition> {
	let mut definitions = Vec::new();

	// Real tools from catalog.
	if let Some(catalog) = catalog {
		for tool_name in visible_tools {
			let selector = roku_common_types::ResourceSelector::tool(tool_name);
			if let Some(descriptor) = catalog.descriptor(&selector) {
				let parameters = if descriptor.input_schema.is_empty() {
					json!({"type": "object", "properties": {}})
				} else {
					let mut properties = serde_json::Map::new();
					for key in &descriptor.input_schema {
						properties.insert(key.clone(), json!({"type": "string"}));
					}
					json!({
						"type": "object",
						"properties": properties,
						"required": descriptor.input_schema.iter()
							.filter(|key| *key != "cwd" && *key != "timeout_ms")
							.collect::<Vec<_>>(),
					})
				};
				definitions.push(ToolDefinition {
					name: tool_name.clone(),
					description: descriptor.selection_hint.clone(),
					parameters,
				});
			}
		}
	}

	// Special pseudo-tools for non-tool actions.
	definitions.push(ToolDefinition {
		name: "final_answer".to_string(),
		description: "Provide the final answer to the user when the task is complete.".to_string(),
		parameters: json!({
			"type": "object",
			"properties": {
				"message": {"type": "string", "description": "The final response message to the user."}
			},
			"required": ["message"]
		}),
	});
	definitions.push(ToolDefinition {
		name: "ask_user".to_string(),
		description: "Ask the user for clarification or additional information.".to_string(),
		parameters: json!({
			"type": "object",
			"properties": {
				"question": {"type": "string", "description": "The question to ask the user."}
			},
			"required": ["question"]
		}),
	});
	definitions.push(ToolDefinition {
		name: "fail".to_string(),
		description: "Report that the task cannot be completed.".to_string(),
		parameters: json!({
			"type": "object",
			"properties": {
				"reason": {"type": "string", "description": "Why the task cannot be completed."}
			},
			"required": ["reason"]
		}),
	});

	definitions
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
	// For explanatory requests ("explain this command/code"), prefer non-execution
	// tools so the bootstrap does not amplify an explanation into execution.
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
	if let Some(tool_name) = shortlisted_tools
		.iter()
		.copied()
		.find(|tool_name| bootstrap_tool_matches_request(tool_name, grounding_input, catalog))
	{
		return Some(tool_name);
	}
	shortlisted_tools
		.into_iter()
		.find(|tool_name| bootstrap_tool_is_groundable(tool_name, grounding_input, catalog))
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
		let hint = resolve_extraction_hint(grounding);
		return match hint {
			ExtractionHint::ExplicitPath => {
				!extract_explicit_path_candidates(grounding_input).is_empty()
					&& ground_tool_arguments(tool_name, grounding_input).is_some()
			}
			ExtractionHint::ConcretePath => {
				!extract_concrete_path_candidates(grounding_input).is_empty()
					&& ground_tool_arguments(tool_name, grounding_input).is_some()
			}
			ExtractionHint::TablePath => {
				extract_concrete_table_path(grounding_input).is_some()
					&& ground_tool_arguments(tool_name, grounding_input).is_some()
			}
			ExtractionHint::GlobPattern => extract_glob_pattern(grounding_input).is_some(),
			ExtractionHint::GrepPattern => extract_grep_pattern(grounding_input).is_some(),
			ExtractionHint::WebQuery => extract_web_query(grounding_input).is_some(),
			ExtractionHint::FetchUrl => extract_fetch_url(grounding_input).is_some(),
			ExtractionHint::ShellCommand => {
				ground_tool_arguments(tool_name, grounding_input).is_some()
			}
			ExtractionHint::PythonCode => {
				ground_tool_arguments(tool_name, grounding_input).is_some()
			}
			ExtractionHint::Default => {
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
		let hint = resolve_extraction_hint(grounding);
		return match hint {
			ExtractionHint::ExplicitPath => {
				!extract_explicit_path_candidates(grounding_input).is_empty()
					&& ground_tool_arguments(tool_name, grounding_input).is_some()
			}
			ExtractionHint::ConcretePath => {
				!extract_concrete_path_candidates(grounding_input).is_empty()
					&& ground_tool_arguments(tool_name, grounding_input).is_some()
			}
			ExtractionHint::TablePath => {
				extract_concrete_table_path(grounding_input).is_some()
					&& ground_tool_arguments(tool_name, grounding_input).is_some()
			}
			ExtractionHint::GlobPattern => extract_glob_pattern(grounding_input).is_some(),
			ExtractionHint::GrepPattern => extract_grep_pattern(grounding_input).is_some(),
			ExtractionHint::WebQuery => extract_web_query(grounding_input).is_some(),
			ExtractionHint::FetchUrl => extract_fetch_url(grounding_input).is_some(),
			ExtractionHint::ShellCommand => {
				ground_tool_arguments(tool_name, grounding_input).is_some()
			}
			ExtractionHint::PythonCode => {
				ground_tool_arguments(tool_name, grounding_input).is_some()
			}
			ExtractionHint::Default => {
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
			"I need to know what command to run. Please describe the task or provide a shell command."
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
		tool_calls: None,
		reason: reason.into(),
		final_message: None,
	}
}

fn ask_user(final_message: String) -> NextStepDecision {
	NextStepDecision {
		action: NextStepAction::AskUser,
		tool_name: None,
		arguments: None,
		tool_calls: None,
		reason: "The loop needs more concrete user input before the next tool step.".to_string(),
		final_message: Some(final_message),
	}
}

fn final_answer(final_message: String) -> NextStepDecision {
	NextStepDecision {
		action: NextStepAction::FinalAnswer,
		tool_name: None,
		arguments: None,
		tool_calls: None,
		reason: "The latest observation is sufficient to answer the user.".to_string(),
		final_message: Some(final_message),
	}
}

fn fail(message: String) -> NextStepDecision {
	NextStepDecision {
		action: NextStepAction::Fail,
		tool_name: None,
		arguments: None,
		tool_calls: None,
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

	use async_trait::async_trait;
	use roku_common_types::{ResourceSelector, RuntimeMemorySections};
	use roku_plugin_llm::{
		GenerationRequest, LlmProvider, LlmRouter, ModelProfile, ProviderCallError,
		ProviderResponse, RiskTier, RoutingPolicy,
	};
	use roku_plugin_skills::SkillRegistry;
	use roku_plugin_tools::ResourceCatalog;
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

	#[async_trait]
	impl LlmProvider for PromptRecordingProvider {
		fn provider_name(&self) -> &'static str {
			"tool-loop-test-provider"
		}

		async fn complete(
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
			// Convert legacy JSON-in-text test fixtures into native tool_calls so
			// that tests work with the tool_use-only dispatch path.
			let tool_calls = if let Ok(v) = serde_json::from_str::<Value>(&output) {
				let action = v.get("action").and_then(Value::as_str).unwrap_or("");
				let tool_name = v.get("tool_name").and_then(Value::as_str);
				match (action, tool_name) {
					("call_tool", Some(name)) => {
						let arguments = v
							.get("arguments")
							.cloned()
							.filter(|a| !a.is_null())
							.unwrap_or(json!({}));
						Some(vec![roku_plugin_llm::ToolCallBlock {
							id: "test-call-1".to_string(),
							name: name.to_string(),
							arguments,
						}])
					}
					("final_answer", _) => {
						let message = v
							.get("final_message")
							.and_then(Value::as_str)
							.unwrap_or("")
							.to_string();
						Some(vec![roku_plugin_llm::ToolCallBlock {
							id: "test-call-1".to_string(),
							name: "final_answer".to_string(),
							arguments: json!({ "message": message }),
						}])
					}
					_ => None,
				}
			} else {
				None
			};
			Ok(ProviderResponse {
				output,
				finish_reason: None,
				prompt_tokens: 24,
				output_tokens: 18,
				latency_ms: 10,
				tool_calls,
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
				tool_calls: None,
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

		// decide_tool_loop_next_step is async; bridge via block_on so that the LlmRouter
		// (which holds a blocking_runtime) is dropped in sync scope rather than async scope.
		let decision = tokio::runtime::Builder::new_current_thread()
			.enable_all()
			.build()
			.expect("tokio runtime for decide-next-step bridge should build")
			.block_on(decide_tool_loop_next_step(
				&loop_state,
				&projection,
				Some(&router),
				None,
				&NextStepRuntimeConfig::default(),
				Some(&test_catalog()),
				None,
			));

		assert_eq!(
			decision.action,
			crate::runtime_loop::NextStepAction::CallTool
		);
		assert_eq!(decision.tool_name.as_deref(), Some("inventory.describe"));
		let prompts = prompts.lock().expect("prompt lock should succeed");
		assert_eq!(prompts.len(), 1);
		assert!(prompts[0].contains("partial inventory summary"));
	}

	#[tokio::test]
	async fn resolved_filesystem_find_can_continue_into_the_consumer_tool() {
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
			None,
		)
		.await;

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

	#[tokio::test]
	async fn multiple_candidate_filesystem_match_asks_user_without_a_live_router() {
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
			None,
		)
		.await;

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
			"tool_name": "fs.read_text",
			"arguments": {},
			"reason": "Try to read a file anyway.",
			"final_message": null
		})]);

		// decide_tool_loop_next_step is async; bridge via block_on so that the LlmRouter
		// (which holds a blocking_runtime) is dropped in sync scope rather than async scope.
		let decision = tokio::runtime::Builder::new_current_thread()
			.enable_all()
			.build()
			.expect("tokio runtime for decide-next-step bridge should build")
			.block_on(decide_tool_loop_next_step(
				&loop_state,
				&projection,
				Some(&router),
				None,
				&NextStepRuntimeConfig::default(),
				Some(&test_catalog()),
				None,
			));

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

	#[tokio::test]
	async fn bootstrap_respects_seed_order_without_natural_language_tool_override() {
		let loop_state = sample_bootstrap_loop_state(
			"Read Cargo.toml and summarize the workspace layout.",
			vec!["fs.find", "fs.read_text"],
			vec!["fs.find", "fs.read_text"],
		);
		let projection = build_context_projection(&loop_state, &RuntimeMemorySections::default());

		let decision = decide_tool_loop_next_step(
			&loop_state,
			&projection,
			None,
			None,
			&NextStepRuntimeConfig::default(),
			Some(&test_catalog()),
			None,
		)
		.await;

		assert_eq!(
			decision.action,
			crate::runtime_loop::NextStepAction::CallTool
		);
		assert_eq!(decision.tool_name.as_deref(), Some("fs.find"));
	}

	#[tokio::test]
	async fn bootstrap_does_not_amplify_explanatory_python_seed_into_execution() {
		let loop_state = sample_bootstrap_loop_state(
			"Explain what this Python code does: `print(1)`",
			vec!["python.run"],
			vec!["python.run"],
		);
		let projection = build_context_projection(&loop_state, &RuntimeMemorySections::default());

		let decision = decide_tool_loop_next_step(
			&loop_state,
			&projection,
			None,
			None,
			&NextStepRuntimeConfig::default(),
			Some(&test_catalog()),
			None,
		)
		.await;

		// Explanatory requests must not execute python.run; the loop should fail gracefully
		// (no visible non-execution tool) rather than running execution code for an explanation.
		assert_ne!(decision.tool_name.as_deref(), Some("python.run"));
	}

	#[tokio::test]
	async fn bootstrap_prefers_inspect_for_explicit_paths_without_a_clear_action() {
		let loop_state = sample_bootstrap_loop_state(
			"Cargo.toml 这个文件帮我看看情况。",
			vec!["fs.inspect", "fs.read_text", "fs.list_dir"],
			vec!["fs.inspect", "fs.read_text", "fs.list_dir"],
		);
		let projection = build_context_projection(&loop_state, &RuntimeMemorySections::default());

		let decision = decide_tool_loop_next_step(
			&loop_state,
			&projection,
			None,
			None,
			&NextStepRuntimeConfig::default(),
			Some(&test_catalog()),
			None,
		)
		.await;

		assert_eq!(
			decision.action,
			crate::runtime_loop::NextStepAction::CallTool
		);
		assert_eq!(decision.tool_name.as_deref(), Some("fs.inspect"));
	}

	#[tokio::test]
	async fn bootstrap_uses_lookup_first_seed_for_non_concrete_basenames() {
		let loop_state = sample_bootstrap_loop_state(
			"我是说，帮我看看cmd那个crate下的runtime.rs，里面的第100行是什么内容？输出出来",
			vec!["fs.find", "fs.glob", "fs.inspect"],
			vec!["fs.find", "fs.glob", "fs.inspect"],
		);
		let projection = build_context_projection(&loop_state, &RuntimeMemorySections::default());

		let decision = decide_tool_loop_next_step(
			&loop_state,
			&projection,
			None,
			None,
			&NextStepRuntimeConfig::default(),
			Some(&test_catalog()),
			None,
		)
		.await;

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
			vec!["fs.find", "fs.glob", "fs.inspect", "fs.read_text"],
		);
		let projection = build_context_projection(&loop_state, &RuntimeMemorySections::default());
		let (router, _prompts) = router_with_responses(vec![json!({
			"action": "call_tool",
			"tool_name": "fs.find",
			"arguments": { "name": "cmd", "kind": "any" },
			"reason": "Find the cmd crate path first.",
			"final_message": null
		})]);

		// decide_tool_loop_next_step is async; bridge via block_on so that the LlmRouter
		// (which holds a blocking_runtime) is dropped in sync scope rather than async scope.
		let decision = tokio::runtime::Builder::new_current_thread()
			.enable_all()
			.build()
			.expect("tokio runtime for decide-next-step bridge should build")
			.block_on(decide_tool_loop_next_step(
				&loop_state,
				&projection,
				Some(&router),
				None,
				&NextStepRuntimeConfig::default(),
				Some(&test_catalog()),
				None,
			));

		assert_eq!(
			decision.action,
			crate::runtime_loop::NextStepAction::CallTool
		);
		assert_eq!(decision.tool_name.as_deref(), Some("fs.find"));
		// align_router_tool_arguments is now a passthrough; the LLM's own argument is preserved.
		assert_eq!(
			decision
				.arguments
				.as_ref()
				.and_then(|value| value.get("name"))
				.and_then(Value::as_str),
			Some("cmd")
		);
	}

	#[test]
	fn router_returns_first_valid_tool_call_without_path_gate() {
		// The ungrounded path gate was removed; the LLM's first valid call is accepted directly.
		let loop_state = sample_bootstrap_loop_state(
			"帮我看看 grounding.rs 这个文件主要是做啥用的吧，一句话总结下",
			vec!["fs.find", "fs.glob", "fs.inspect"],
			vec!["fs.find", "fs.glob", "fs.inspect", "fs.read_text"],
		);
		let projection = build_context_projection(&loop_state, &RuntimeMemorySections::default());
		let (router, _prompts) = router_with_responses(vec![json!({
			"action": "call_tool",
			"tool_name": "fs.read_text",
			"arguments": { "path": "/Users/jojo/cjj_project/Roku/grounding.rs" },
			"reason": "Read the file directly.",
			"final_message": null
		})]);

		// decide_tool_loop_next_step is async; bridge via block_on so that the LlmRouter
		// (which holds a blocking_runtime) is dropped in sync scope rather than async scope.
		let decision = tokio::runtime::Builder::new_current_thread()
			.enable_all()
			.build()
			.expect("tokio runtime for decide-next-step bridge should build")
			.block_on(decide_tool_loop_next_step(
				&loop_state,
				&projection,
				Some(&router),
				None,
				&NextStepRuntimeConfig::default(),
				Some(&test_catalog()),
				None,
			));

		assert_eq!(
			decision.action,
			crate::runtime_loop::NextStepAction::CallTool
		);
		// Without the path gate, fs.read_text is accepted directly.
		assert_eq!(decision.tool_name.as_deref(), Some("fs.read_text"));
	}

	#[tokio::test]
	async fn bootstrap_keeps_python_run_for_explicit_execution_requests() {
		let loop_state = sample_bootstrap_loop_state(
			"Run this Python code: `print(1)`",
			vec!["python.run"],
			vec!["python.run"],
		);
		let projection = build_context_projection(&loop_state, &RuntimeMemorySections::default());

		let decision = decide_tool_loop_next_step(
			&loop_state,
			&projection,
			None,
			None,
			&NextStepRuntimeConfig::default(),
			Some(&test_catalog()),
			None,
		)
		.await;

		assert_eq!(
			decision.action,
			crate::runtime_loop::NextStepAction::CallTool
		);
		assert_eq!(decision.tool_name.as_deref(), Some("python.run"));
	}

	#[tokio::test]
	async fn bootstrap_does_not_amplify_explanatory_shell_seed_into_execution() {
		let loop_state = sample_bootstrap_loop_state(
			"Explain what the shell command `pwd` does, but do not run it.",
			vec!["command.run"],
			vec!["command.run"],
		);
		let projection = build_context_projection(&loop_state, &RuntimeMemorySections::default());

		let decision = decide_tool_loop_next_step(
			&loop_state,
			&projection,
			None,
			None,
			&NextStepRuntimeConfig::default(),
			Some(&test_catalog()),
			None,
		)
		.await;

		// Explanatory requests must not execute command.run; the loop should not shortlist
		// the execution tool for a plain explanation request.
		assert_ne!(decision.tool_name.as_deref(), Some("command.run"));
	}
}
