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

//! Request-level route classification for the direct runtime entry path.
//!
//! ## Overview
//!
//! This module decides how a **new user request** should enter the runtime:
//!
//! - which coarse intent family it belongs to
//! - whether the request should start as a direct tool-loop route or an escalation
//! - which tools are safe to expose as initial `candidate_tools`
//! - which weak inventory / plugin signals should accompany that initial route seed
//!
//! The classifier is intentionally an **entry-stage router**, not the ReAct loop itself. Its
//! output is a [`RouteDecision`] / [`RouteDecisionResult`] that seeds the runtime. After that
//! point, the loop, grounded observations, and next-step logic own the round-by-round behavior.
//!
//! ## Responsibilities
//!
//! This module is responsible for:
//!
//! - detecting coarse request families such as chat, filesystem, table, web, code, or multi-step
//! - applying deterministic pre-classification when the current turn already contains strong,
//!   explicit grounding signals
//! - falling back to an LLM route classifier when deterministic hints are insufficient
//! - filtering route seeds against the currently enabled inventory
//! - emitting **weak** tool shortlists (`candidate_tools`) that the runtime loop can start from
//! - surfacing unavailable-family and route-model failure conditions as explicit escalations
//!
//! ## Non-Goals
//!
//! This module must **not**:
//!
//! - decide the loop's later follow-up steps after a tool observation
//! - interpret `ToolObservation` or `InterpretedObservation`
//! - decide terminal branches such as `ask_user`, `final_answer`, or `fail`
//! - own recovery, retry, or budget policy
//! - encode a hidden multi-round workflow for specific tools
//! - turn `candidate_tools` into authoritative single-tool bindings unless the request is already
//!   explicitly grounded to a concrete one-shot capability
//!
//! In particular, this module should not become a "static decision center" that steals semantic
//! control from the unified ReAct loop. If a behavior depends on **what happened after a tool
//! call**, it belongs somewhere downstream of classification.
//!
//! ## Design Constraints
//!
//! - Prefer family-level or weak tool-level hints over hard-coded execution flows.
//! - Keep deterministic rules scoped to observable current-turn structure: paths, code blocks,
//!   shell commands, URLs, explicit installed tool references, and similar grounded signals.
//! - Treat `candidate_tools` as an initial shortlist for visibility and bootstrap, not as proof
//!   that no later tool switch should happen.
//! - When adding tool-specific logic here, it should stay in the category of **grounded hint
//!   extraction**, not runtime semantic recovery.
//!
//! ## Position in the Runtime Chain
//!
//! This module sits at the **request-entry classification stage** of the direct runtime path.
//! A simplified chain looks like:
//!
//! 1. request intake
//! 2. route classification (**this module**)
//! 3. loop initialization / visible tool seeding
//! 4. tool execution
//! 5. `ToolObservation` / `InterpretedObservation` consumption
//! 6. next-step decision, including tool switching, `ask_user`, `fail`, or `final_answer`
//!
//! That means this module is **upstream of the ReAct loop**. It is allowed to shape the initial
//! entry conditions, but it is downstream modules that own live observation-driven adaptation.
//!
//! ## Interaction with the Runtime Loop
//!
//! The intended control split is:
//!
//! 1. `classifier.rs` decides how the request enters the runtime.
//! 2. `runtime_loop` consumes that seed plus live observations.
//! 3. `tool_loop` decides whether to continue, switch tools, ask the user, fail, or finish.
//!
//! If a future change starts making this module answer step 3, that is a design smell and should
//! be treated as architecture drift.
//!
use roku_common_types::{RequestEnvelope, ResourceSelector};
use roku_plugin_catalog::{CatalogDescriptor, CatalogMatch, ResourceCatalog, ResourceKind};
use roku_plugin_core::PluginRegistrySnapshot;
use roku_plugin_llm::{GenerationRequest, LlmRouter, RiskTier, StructuredGenerationError};
use roku_plugin_tools::RuntimeVisibleToolAvailabilitySnapshot;
use serde_json::json;

use crate::AgentRuntimeConfig;
use crate::router::{
	DirectRoutePlan, EscalationAction, EscalationReason, IntentFamily, RouteDecision,
	RouteDecisionResult, RouteEscalationPlan, RouteRisk,
};
use crate::runtime_loop::{
	explanatory_python_code_request, explanatory_shell_command_request,
	extract_concrete_path_candidates as shared_extract_concrete_path_candidates,
	extract_concrete_table_path as shared_extract_concrete_table_path,
	extract_explicit_path_candidates as shared_extract_explicit_path_candidates,
	extract_explicit_shell_command as shared_extract_explicit_shell_command,
	extract_explicit_table_path, extract_glob_pattern as shared_extract_glob_pattern,
	extract_skill_source_url, extract_web_query, goal_requests_python_execution,
	goal_requests_web_lookup, ground_tool_arguments, grounded_python_code_allows_execution,
	grounded_shell_command_allows_execution, tool_required_argument_keys,
};
use crate::tool_config::{BuiltinToolRole, ToolCatalogConfig};

const MIN_SKILL_SCORE: f32 = 0.72;
const ROUTE_CONFIDENCE_FLOOR: f32 = 0.65;
const DETERMINISTIC_TOOL_MATCH_SCORE_FLOOR: f32 = 0.18;
const DETERMINISTIC_TOOL_MATCH_MARGIN_RATIO: f32 = 1.05;

pub(crate) struct RouteClassifierContext<'a> {
	pub(crate) catalog: &'a ResourceCatalog,
	pub(crate) tool_config: &'a ToolCatalogConfig,
	pub(crate) agent_runtime_config: &'a AgentRuntimeConfig,
	pub(crate) plugin_snapshot: &'a PluginRegistrySnapshot,
	pub(crate) availability_snapshot: &'a RuntimeVisibleToolAvailabilitySnapshot,
	pub(crate) route_router: Option<&'a LlmRouter>,
	pub(crate) skill_execution_available: bool,
}

pub(crate) async fn classify_request(
	context: RouteClassifierContext<'_>,
	request: &RequestEnvelope,
) -> RouteDecisionResult {
	if let Some(result) = deterministic_pre_classify(&context, request) {
		return result;
	}

	match context.route_router {
		Some(router) => classify_with_llm(&context, request, router).await,
		None => unresolved_without_route_model(),
	}
}

fn deterministic_pre_classify(
	context: &RouteClassifierContext<'_>,
	request: &RequestEnvelope,
) -> Option<RouteDecisionResult> {
	if let Some(result) = classify_structured_multi_step_request(context, request) {
		return Some(result);
	}

	if explanatory_shell_command_request(&request.goal)
		&& context
			.availability_snapshot
			.is_tool_enabled("general.execute")
	{
		let decision = RouteDecision::new(
			IntentFamily::Chat,
			0.81,
			false,
			RouteRisk::Low,
			vec!["general.execute".to_string()],
			candidate_plugins_for_tool(context.plugin_snapshot, "general.execute"),
			Vec::new(),
			"explicit shell command is referenced for explanation rather than execution; keep the request on the general assistant path",
		);
		return Some(build_tool_loop_route(
			context,
			decision,
			Some("general.execute"),
			Vec::new(),
		));
	}

	if explanatory_python_code_request(&request.goal)
		&& context
			.availability_snapshot
			.is_tool_enabled("general.execute")
	{
		let decision = RouteDecision::new(
			IntentFamily::Chat,
			0.8,
			false,
			RouteRisk::Low,
			vec!["general.execute".to_string()],
			candidate_plugins_for_tool(context.plugin_snapshot, "general.execute"),
			Vec::new(),
			"explicit Python code is referenced for explanation rather than execution; keep the request on the general assistant path",
		);
		return Some(build_tool_loop_route(
			context,
			decision,
			Some("general.execute"),
			Vec::new(),
		));
	}

	if goal_requests_python_execution(&request.goal)
		&& context.availability_snapshot.is_tool_enabled("python.run")
		&& extract_explicit_python_code(&request.goal).is_none()
	{
		return Some(missing_argument_route(
			IntentFamily::CodeExec,
			vec!["code".to_string()],
			"the request clearly asks to run Python code, but no explicit Python snippet is present in the current turn",
		));
	}

	if goal_requests_web_lookup(&request.goal)
		&& context.availability_snapshot.is_tool_enabled("web.search")
		&& extract_web_query(&request.goal).is_none()
	{
		return Some(missing_argument_route(
			IntentFamily::WebLookup,
			vec!["query".to_string()],
			"the request clearly asks for a web lookup, but no concrete search query is present in the current turn",
		));
	}

	if let Some(result) = classify_contract_level_grounded_hint(context, request) {
		return Some(result);
	}

	if extract_skill_source_url(&request.goal).is_some()
		&& goal_implies_install_intent(&request.goal)
	{
		let install_tool_name =
			tool_name_for_role(context.tool_config, BuiltinToolRole::SkillInstall);
		let decision = RouteDecision::new(
			IntentFamily::TextTransform,
			0.98,
			false,
			RouteRisk::Medium,
			vec![install_tool_name.clone()],
			vec![
				"builtin-tools".to_string(),
				"skill-source-local".to_string(),
			],
			Vec::new(),
			"explicit skill install url with install intent detected in user request",
		);
		return Some(build_tool_loop_route(
			context,
			decision,
			Some(&install_tool_name),
			Vec::new(),
		));
	}

	if let Some(selector) = explicit_skill_selector(context.catalog, &request.goal) {
		if context.route_router.is_none() {
			let descriptor = context.catalog.descriptor(&selector)?;
			return Some(build_skill_route_result(
				context,
				selector,
				descriptor,
				false,
				"explicit installed skill reference matched deterministic classifier",
			));
		}
		return None;
	}

	if let Some(result) = classify_structural_fallback(context, request) {
		return Some(result);
	}

	if let Some(result) = classify_deterministic_contract_tool_match(context, request) {
		return Some(result);
	}

	if context.route_router.is_none()
		&& context
			.availability_snapshot
			.is_tool_enabled("general.execute")
	{
		let decision = RouteDecision::new(
			IntentFamily::Chat,
			0.68,
			false,
			RouteRisk::Low,
			vec!["general.execute".to_string()],
			candidate_plugins_for_tool(context.plugin_snapshot, "general.execute"),
			Vec::new(),
			"no grounded direct tool route matched; fall back to the general assistant loop in deterministic mode",
		);
		return Some(build_tool_loop_route(
			context,
			decision,
			Some("general.execute"),
			Vec::new(),
		));
	}

	None
}

fn classify_structured_multi_step_request(
	context: &RouteClassifierContext<'_>,
	request: &RequestEnvelope,
) -> Option<RouteDecisionResult> {
	let explicit_paths = extract_explicit_path_candidates(&request.goal);
	let has_explicit_code = extract_explicit_python_code(&request.goal).is_some();
	let has_glob_pattern = extract_glob_pattern(&request.goal).is_some();
	let looks_multi_step = has_shell_command_chain(&request.goal)
		|| (!has_explicit_code && !has_glob_pattern && explicit_paths.len() > 1);
	if !looks_multi_step {
		return None;
	}
	let decision = RouteDecision::new(
		IntentFamily::MultiStep,
		0.9,
		true,
		RouteRisk::Medium,
		Vec::new(),
		Vec::new(),
		Vec::new(),
		"structured request contains multiple grounded targets or chained commands that cannot be satisfied by a single direct tool invocation",
	);
	Some(build_loop_hint_route(context, decision))
}

fn has_shell_command_chain(goal: &str) -> bool {
	goal.contains("&&") || goal.contains("||") || goal.contains(';')
}

fn classify_contract_level_grounded_hint(
	context: &RouteClassifierContext<'_>,
	request: &RequestEnvelope,
) -> Option<RouteDecisionResult> {
	if let Some(selector) = explicit_tool_selector(context, &request.goal) {
		let descriptor = context.catalog.descriptor(&selector)?;
		let tool_name = descriptor.name.clone();
		if !explicit_tool_hint_is_grounded(&tool_name, &request.goal) {
			return None;
		}
		let decision = RouteDecision::new(
			coarse_intent_hint_for_tool(&tool_name),
			0.9,
			false,
			resource_risk(descriptor),
			vec![tool_name.clone()],
			candidate_plugins_for_tool(context.plugin_snapshot, &tool_name),
			Vec::new(),
			format!(
				"explicit installed tool reference provides a non-authoritative `{tool_name}` hint"
			),
		);
		return Some(build_tool_loop_route(
			context,
			decision,
			Some(&tool_name),
			Vec::new(),
		));
	}

	if grounded_python_code_allows_execution(&request.goal)
		&& let Some(code) = extract_explicit_python_code(&request.goal)
		&& context.availability_snapshot.is_tool_enabled("python.run")
	{
		let decision = RouteDecision::new(
			IntentFamily::CodeExec,
			0.93,
			false,
			RouteRisk::Medium,
			vec!["python.run".to_string()],
			candidate_plugins_for_tool(context.plugin_snapshot, "python.run"),
			Vec::new(),
			"contract-level grounded Python code provides a non-authoritative `python.run` hint",
		);
		let _ = code;
		return Some(build_tool_loop_route(
			context,
			decision,
			Some("python.run"),
			Vec::new(),
		));
	}

	if grounded_shell_command_allows_execution(&request.goal)
		&& let Some(command) = extract_explicit_shell_command(&request.goal)
		&& context.availability_snapshot.is_tool_enabled("command.run")
	{
		let decision = RouteDecision::new(
			IntentFamily::CodeExec,
			0.94,
			false,
			RouteRisk::Medium,
			vec!["command.run".to_string()],
			candidate_plugins_for_tool(context.plugin_snapshot, "command.run"),
			Vec::new(),
			"contract-level grounded shell command provides a non-authoritative `command.run` hint",
		);
		let _ = command;
		return Some(build_tool_loop_route(
			context,
			decision,
			Some("command.run"),
			Vec::new(),
		));
	}

	if goal_requests_web_lookup(&request.goal)
		&& let Some(query) = extract_web_query(&request.goal)
		&& context.availability_snapshot.is_tool_enabled("web.search")
	{
		let decision = RouteDecision::new(
			IntentFamily::WebLookup,
			0.9,
			false,
			RouteRisk::Low,
			vec!["web.search".to_string()],
			candidate_plugins_for_tool(context.plugin_snapshot, "web.search"),
			Vec::new(),
			"contract-level grounded web lookup provides a non-authoritative `web.search` hint",
		);
		let _ = query;
		return Some(build_tool_loop_route(
			context,
			decision,
			Some("web.search"),
			Vec::new(),
		));
	}

	if extract_glob_pattern(&request.goal).is_some()
		&& context.availability_snapshot.is_tool_enabled("fs.glob")
	{
		let decision = RouteDecision::new(
			IntentFamily::FilesystemRead,
			0.92,
			false,
			RouteRisk::Low,
			vec!["fs.glob".to_string()],
			candidate_plugins_for_tool(context.plugin_snapshot, "fs.glob"),
			Vec::new(),
			"contract-level grounded glob pattern provides a non-authoritative `fs.glob` hint",
		);
		return Some(build_tool_loop_route(
			context,
			decision,
			Some("fs.glob"),
			Vec::new(),
		));
	}

	if extract_explicit_table_path(&request.goal).is_some()
		&& context
			.availability_snapshot
			.has_enabled_tool_with_prefix("table.")
	{
		if extract_concrete_table_path(&request.goal).is_some()
			&& let Some(tool_name) = explicit_table_action_tool(&request.goal)
			&& context.availability_snapshot.is_tool_enabled(tool_name)
		{
			let decision = RouteDecision::new(
				IntentFamily::TableRead,
				0.9,
				false,
				RouteRisk::Low,
				vec![tool_name.to_string()],
				candidate_plugins_for_tool(context.plugin_snapshot, tool_name),
				Vec::new(),
				format!(
					"explicit table path plus a concrete `{tool_name}` action provides a grounded direct start"
				),
			);
			return Some(build_tool_loop_route(
				context,
				decision,
				Some(tool_name),
				Vec::new(),
			));
		}
		let candidate_tools = broad_table_candidate_tools();
		let decision = RouteDecision::new(
			IntentFamily::TableRead,
			0.86,
			false,
			RouteRisk::Low,
			candidate_tools,
			Vec::new(),
			Vec::new(),
			"contract-level grounded table input provides a non-authoritative table-family hint",
		);
		return Some(build_loop_hint_route(context, decision));
	}

	if !extract_explicit_path_candidates(&request.goal).is_empty()
		&& context
			.availability_snapshot
			.has_enabled_tool_with_prefix("fs.")
	{
		if !extract_concrete_path_candidates(&request.goal).is_empty()
			&& let Some(tool_name) = explicit_filesystem_action_tool(&request.goal)
			&& context.availability_snapshot.is_tool_enabled(tool_name)
		{
			let decision = RouteDecision::new(
				IntentFamily::FilesystemRead,
				0.89,
				false,
				RouteRisk::Low,
				vec![tool_name.to_string()],
				candidate_plugins_for_tool(context.plugin_snapshot, tool_name),
				Vec::new(),
				format!(
					"explicit filesystem path plus a concrete `{tool_name}` action provides a grounded direct start"
				),
			);
			return Some(build_tool_loop_route(
				context,
				decision,
				Some(tool_name),
				Vec::new(),
			));
		}
		let decision = RouteDecision::new(
			IntentFamily::FilesystemRead,
			0.82,
			false,
			RouteRisk::Low,
			broad_filesystem_candidate_tools(&request.goal),
			Vec::new(),
			Vec::new(),
			"explicit filesystem resource is present, but the requested action is still broad enough that the live loop should choose among a controlled starter set",
		);
		return Some(build_loop_hint_route(context, decision));
	}

	None
}

fn explicit_tool_selector(
	context: &RouteClassifierContext<'_>,
	goal: &str,
) -> Option<ResourceSelector> {
	let explicit_tokens = explicit_skill_tokens(goal);
	let mut entries = context
		.catalog
		.entries()
		.iter()
		.filter(|entry| {
			entry.kind == ResourceKind::Tool
				&& context.availability_snapshot.is_tool_enabled(&entry.name)
		})
		.collect::<Vec<_>>();
	entries.sort_by(|left, right| right.name.len().cmp(&left.name.len()));
	entries.into_iter().find_map(|entry| {
		let normalized_name = normalize(&entry.name);
		(normalized_name.len() > 2
			&& explicit_tokens
				.iter()
				.any(|token| token == &normalized_name))
		.then(|| entry.selector.clone())
	})
}

fn coarse_intent_hint_for_tool(tool_name: &str) -> IntentFamily {
	match tool_name {
		"inventory.describe" | "general.execute" => IntentFamily::Chat,
		name if name.starts_with("fs.") => IntentFamily::FilesystemRead,
		name if name.starts_with("table.") => IntentFamily::TableRead,
		name if name.starts_with("web.") => IntentFamily::WebLookup,
		name if name.starts_with("python.") || name.starts_with("command.") => {
			IntentFamily::CodeExec
		}
		_ => IntentFamily::TextTransform,
	}
}

fn explicit_tool_hint_is_grounded(tool_name: &str, goal: &str) -> bool {
	match tool_name {
		"fs.exists" | "fs.inspect" | "fs.list_dir" | "fs.read_text" => {
			!extract_concrete_path_candidates(goal).is_empty()
		}
		"fs.find" => !extract_explicit_path_candidates(goal).is_empty(),
		"fs.glob" => extract_glob_pattern(goal).is_some(),
		"table.inspect" | "table.list_sheets" | "table.preview" | "table.schema" => {
			extract_concrete_table_path(goal).is_some()
		}
		"web.search" => goal_requests_web_lookup(goal) && extract_web_query(goal).is_some(),
		"command.run" => grounded_shell_command_allows_execution(goal),
		"python.run" => grounded_python_code_allows_execution(goal),
		"skill.install" | "skill.ensure_installed" => extract_skill_source_url(goal).is_some(),
		_ => false,
	}
}

async fn classify_with_llm(
	context: &RouteClassifierContext<'_>,
	request: &RequestEnvelope,
	router: &LlmRouter,
) -> RouteDecisionResult {
	let tool_matches = discoverable_tool_matches(
		context
			.catalog
			.retrieve(&request.goal, Some(ResourceKind::Tool), 8),
		context.availability_snapshot,
	);
	let skill_matches = context
		.catalog
		.retrieve(&request.goal, Some(ResourceKind::Skill), 4);
	let candidates = llm_candidates(
		context.catalog,
		context.availability_snapshot,
		context.agent_runtime_config,
	);
	let response = router
		.generate_json_value(&GenerationRequest {
			system_prompt: Some(
				"You are Roku's route classifier. Return only valid JSON matching the requested schema."
					.to_string(),
			),
			prompt: route_classifier_prompt(request, &candidates),
			expected_output_tokens: context.agent_runtime_config.router.expected_output_tokens,
			risk_tier: RiskTier::Low,
			preferred_provider: None,
			budget_tokens_remaining: context.agent_runtime_config.router.budget_tokens_remaining,
			budget_cost_remaining_usd: context
				.agent_runtime_config
				.router
				.budget_cost_remaining_usd,
		})
		.await;
	let value = match response {
		Ok(response) => response.value,
		Err(error) => {
			return llm_classifier_failure_route(context, error);
		}
	};
	let mut decision = match RouteDecision::from_json_value(&value) {
		Ok(decision) => decision,
		Err(error) => {
			return build_loop_hint_route(
				context,
				RouteDecision::new(
					IntentFamily::Unknown,
					0.0,
					false,
					RouteRisk::Low,
					Vec::new(),
					Vec::new(),
					Vec::new(),
					format!("route classifier returned invalid schema: {error}"),
				),
			);
		}
	};
	if let Some(result) = classify_skill_route_from_decision(
		context,
		request,
		&decision,
		&skill_matches,
		&tool_matches,
	) {
		return result;
	}
	if !decision.missing_arguments.is_empty() {
		return build_loop_hint_route(context, decision);
	}
	if decision.confidence_score() < ROUTE_CONFIDENCE_FLOOR {
		return build_loop_hint_route(context, decision);
	}
	if decision.requires_multi_step || decision.intent_family == IntentFamily::MultiStep {
		return build_loop_hint_route(context, decision);
	}
	normalize_direct_route_seed(&request.goal, &mut decision);
	build_tool_loop_route(context, decision, None, Vec::new())
}

fn unresolved_without_route_model() -> RouteDecisionResult {
	let decision = RouteDecision::new(
		IntentFamily::Unknown,
		0.0,
		false,
		RouteRisk::Low,
		Vec::new(),
		Vec::new(),
		Vec::new(),
		"deterministic pre-classifier did not find a stable contract-level hint and no route model is available",
	);
	RouteDecisionResult::Escalate(RouteEscalationPlan {
		decision,
		reason: EscalationReason::RouteModelUnavailable,
		action: EscalationAction::FallbackAnswer,
	})
}

fn llm_classifier_failure_route(
	context: &RouteClassifierContext<'_>,
	error: StructuredGenerationError,
) -> RouteDecisionResult {
	let (reason, escalation_reason) = match error {
		StructuredGenerationError::ParseGuard(error) => (
			format!("route classifier parse guard rejected provider output: {error}"),
			EscalationReason::RouteParseGuardFailure,
		),
		StructuredGenerationError::Llm(error) => (
			format!("route classifier failed before producing a usable decision: {error}"),
			EscalationReason::RouteClassifierFailure,
		),
	};
	let mut decision = RouteDecision::new(
		IntentFamily::Unknown,
		0.0,
		false,
		RouteRisk::Low,
		Vec::new(),
		Vec::new(),
		Vec::new(),
		reason,
	);
	if matches!(escalation_reason, EscalationReason::RouteParseGuardFailure) {
		decision.requires_multi_step = false;
	}
	build_loop_hint_route(context, decision)
}

fn llm_candidates(
	catalog: &ResourceCatalog,
	availability_snapshot: &RuntimeVisibleToolAvailabilitySnapshot,
	config: &AgentRuntimeConfig,
) -> Vec<serde_json::Value> {
	let mut entries = catalog
		.entries()
		.iter()
		.filter(|entry| {
			entry.kind != ResourceKind::Tool || availability_snapshot.is_tool_enabled(&entry.name)
		})
		.map(|entry| {
			json!({
				"selector": entry.selector.display_key(),
				"kind": format!("{:?}", entry.kind),
				"name": entry.name,
				"selection_hint": compact_selection_hint_text(
					entry.effective_selection_hint(),
					config.prompts.candidate_description_max_chars,
				),
				"required_arguments": tool_required_argument_keys(&entry.name, Some(catalog)),
			})
		})
		.collect::<Vec<_>>();
	entries.truncate(config.router.candidate_inventory_limit);
	entries
}

fn compact_selection_hint_text(value: &str, max_chars: usize) -> String {
	let trimmed = value.trim();
	if trimmed.chars().count() <= max_chars {
		return trimmed.to_string();
	}
	let truncated = trimmed
		.chars()
		.take(max_chars.saturating_sub(3))
		.collect::<String>();
	format!("{truncated}...")
}

fn route_classifier_prompt(request: &RequestEnvelope, candidates: &[serde_json::Value]) -> String {
	format!(
		r#"Return only JSON with exactly these keys:
{{
  "intent_family": "chat | filesystem_read | table_read | web_lookup | code_exec | text_transform | multi_step | unknown",
  "confidence": 0.0,
  "requires_multi_step": false,
  "risk": "low | medium | high",
  "candidate_tools": ["tool names"],
  "candidate_plugins": ["plugin ids"],
  "missing_arguments": ["argument names"],
  "reason": "short explanation"
}}

Rules:
- Use only candidate tool names from the provided selection inventory if you name tools.
- Base the decision on the current user goal and the current selection inventory. Do not inherit intent from prior conversation turns unless the current goal explicitly restates it.
- Use `chat` for greetings or direct assistant conversation.
- Use `filesystem_read`, `table_read`, `web_lookup`, or `code_exec` when the intent clearly asks for those families even if no tool is available yet.
- Use `multi_step` when the request obviously needs a planning-heavy workflow.
- Use `skill.execute` only when the user is asking to actually run an installed script-backed skill and perform side effects.
- If the user is asking to summarize, explain, describe, list, or quote guidance from an installed skill, do not select `skill.execute`; prefer an advisory route with no execution tool.
- For natural-language filesystem or table requests that do not already contain one explicit tool-ready resource argument, avoid collapsing the route to one narrow tool path. Prefer an empty shortlist or a broad family shortlist instead of pre-binding to a single finder/reader tool.
- Leave `candidate_tools` empty if no current direct tool is safe.
- `missing_arguments` should be empty unless the user must provide something concrete first.

User goal:
{goal}

Current selection inventory:
{candidates}"#,
		goal = request.goal,
		candidates = serde_json::to_string_pretty(candidates).unwrap_or_default(),
	)
}

fn classify_skill_route_from_decision(
	context: &RouteClassifierContext<'_>,
	request: &RequestEnvelope,
	decision: &RouteDecision,
	skill_matches: &[CatalogMatch],
	tool_matches: &[CatalogMatch],
) -> Option<RouteDecisionResult> {
	let explicit_selector = explicit_skill_selector(context.catalog, &request.goal);
	let should_consider_skill =
		explicit_selector.is_some() || decision_requests_skill_local_context(context, decision);
	if !should_consider_skill {
		return None;
	}
	let selector =
		explicit_selector.or_else(|| best_skill_selector(skill_matches, tool_matches))?;
	let descriptor = context.catalog.descriptor(&selector)?;
	Some(build_skill_route_result(
		context,
		selector,
		descriptor,
		decision_requests_skill_execution(context, decision),
		format!(
			"route classifier selected installed skill `{}`",
			descriptor.name
		),
	))
}

fn classify_structural_fallback(
	context: &RouteClassifierContext<'_>,
	request: &RequestEnvelope,
) -> Option<RouteDecisionResult> {
	let goal = request.goal.trim();
	if grounded_python_code_allows_execution(goal)
		&& !context.availability_snapshot.is_tool_enabled("python.run")
	{
		return Some(unavailable_family_route(
			IntentFamily::CodeExec,
			"explicit Python code was provided, but `python.run` is not enabled in the current runtime inventory",
		));
	}
	if grounded_shell_command_allows_execution(goal)
		&& !context.availability_snapshot.is_tool_enabled("command.run")
	{
		return Some(unavailable_family_route(
			IntentFamily::CodeExec,
			"explicit shell command was provided, but `command.run` is not enabled in the current runtime inventory",
		));
	}
	if goal_requests_web_lookup(goal)
		&& !context.availability_snapshot.is_tool_enabled("web.search")
	{
		return Some(unavailable_family_route(
			IntentFamily::WebLookup,
			"an explicit web lookup request was provided, but `web.search` is not enabled in the current runtime inventory",
		));
	}
	if extract_explicit_table_path(goal).is_some()
		&& !context
			.availability_snapshot
			.has_enabled_tool_with_prefix("table.")
	{
		return Some(unavailable_family_route(
			IntentFamily::TableRead,
			"explicit table input was provided, but core table tools are not enabled in the current runtime inventory",
		));
	}
	if (extract_glob_pattern(goal).is_some() || !extract_explicit_path_candidates(goal).is_empty())
		&& !context
			.availability_snapshot
			.has_enabled_tool_with_prefix("fs.")
	{
		return Some(unavailable_family_route(
			IntentFamily::FilesystemRead,
			"explicit filesystem input was provided, but core filesystem tools are not enabled in the current runtime inventory",
		));
	}
	None
}

fn unavailable_family_route(
	intent_family: IntentFamily,
	reason: impl Into<String>,
) -> RouteDecisionResult {
	RouteDecisionResult::Escalate(RouteEscalationPlan {
		decision: RouteDecision::new(
			intent_family,
			0.72,
			false,
			RouteRisk::Low,
			Vec::new(),
			Vec::new(),
			Vec::new(),
			reason,
		),
		reason: EscalationReason::NoEnabledRouteTarget,
		action: EscalationAction::FallbackAnswer,
	})
}

fn missing_argument_route(
	intent_family: IntentFamily,
	missing_arguments: Vec<String>,
	reason: impl Into<String>,
) -> RouteDecisionResult {
	RouteDecisionResult::Escalate(RouteEscalationPlan {
		decision: RouteDecision::new(
			intent_family,
			0.78,
			false,
			RouteRisk::Low,
			Vec::new(),
			Vec::new(),
			missing_arguments,
			reason,
		),
		reason: EscalationReason::MissingArguments,
		action: EscalationAction::AskForMoreInfo,
	})
}

fn classify_deterministic_contract_tool_match(
	context: &RouteClassifierContext<'_>,
	request: &RequestEnvelope,
) -> Option<RouteDecisionResult> {
	if context.route_router.is_some() {
		return None;
	}

	let tool_matches = discoverable_tool_matches(
		context
			.catalog
			.retrieve(&request.goal, Some(ResourceKind::Tool), 6),
		context.availability_snapshot,
	);
	let best_match = select_deterministic_contract_tool_match(
		&tool_matches,
		&request.goal,
		None,
		context.catalog,
	)?;
	if let Some(selector) = explicit_tool_selector(context, &request.goal)
		&& let Some(descriptor) = context.catalog.descriptor(&selector)
		&& !explicit_tool_hint_is_grounded(&descriptor.name, &request.goal)
	{
		return None;
	}
	let tool_name = best_match.descriptor.name.clone();
	let decision = RouteDecision::new(
		coarse_intent_hint_for_tool(&tool_name),
		0.82,
		false,
		resource_risk(&best_match.descriptor),
		deterministic_seed_candidate_tools(&tool_name, &request.goal),
		candidate_plugins_for_tool(context.plugin_snapshot, &tool_name),
		Vec::new(),
		format!(
			"deterministic catalog retrieval matched contract-backed tool `{tool_name}` and the current goal can already ground its required inputs"
		),
	);
	let mut decision = decision;
	normalize_direct_route_seed(&request.goal, &mut decision);
	Some(build_loop_hint_route(context, decision))
}

fn select_deterministic_contract_tool_match<'a>(
	tool_matches: &'a [CatalogMatch],
	goal: &str,
	intent_family: Option<IntentFamily>,
	catalog: &ResourceCatalog,
) -> Option<&'a CatalogMatch> {
	let grounded_intent = intent_family.or_else(|| deterministic_grounded_intent(goal));
	let grounded_matches = tool_matches
		.iter()
		.filter(|entry| entry.descriptor.contract.is_some())
		.filter(|entry| deterministic_match_can_start(&entry.descriptor.name, goal, catalog))
		.filter(|entry| {
			grounded_intent
				.is_none_or(|intent| coarse_intent_hint_for_tool(&entry.descriptor.name) == intent)
		})
		.collect::<Vec<_>>();
	let [best_match, rest @ ..] = grounded_matches.as_slice() else {
		return None;
	};
	let next_score = rest.first().map(|entry| entry.score).unwrap_or_default();
	let margin_ok =
		next_score == 0.0 || best_match.score >= next_score * DETERMINISTIC_TOOL_MATCH_MARGIN_RATIO;
	(best_match.score >= DETERMINISTIC_TOOL_MATCH_SCORE_FLOOR && margin_ok).then_some(*best_match)
}

fn deterministic_match_can_start(tool_name: &str, goal: &str, catalog: &ResourceCatalog) -> bool {
	match tool_name {
		"python.run" => {
			grounded_python_code_allows_execution(goal)
				&& ground_tool_arguments(tool_name, goal).is_some()
		}
		"command.run" => {
			grounded_shell_command_allows_execution(goal)
				&& ground_tool_arguments(tool_name, goal).is_some()
		}
		"web.search" => goal_requests_web_lookup(goal) && extract_web_query(goal).is_some(),
		"fs.glob" => extract_glob_pattern(goal).is_some(),
		"fs.find" => {
			!extract_explicit_path_candidates(goal).is_empty()
				&& explicit_filesystem_action_tool(goal) == Some("fs.find")
		}
		"fs.exists" => {
			!extract_concrete_path_candidates(goal).is_empty()
				&& explicit_filesystem_action_tool(goal) == Some("fs.exists")
		}
		"fs.inspect" => {
			!extract_concrete_path_candidates(goal).is_empty()
				&& explicit_filesystem_action_tool(goal) == Some("fs.inspect")
		}
		"fs.list_dir" => {
			!extract_concrete_path_candidates(goal).is_empty()
				&& explicit_filesystem_action_tool(goal) == Some("fs.list_dir")
		}
		"fs.read_text" => {
			!extract_concrete_path_candidates(goal).is_empty()
				&& explicit_filesystem_action_tool(goal) == Some("fs.read_text")
		}
		"table.inspect" => {
			extract_concrete_table_path(goal).is_some()
				&& explicit_table_action_tool(goal) == Some("table.inspect")
		}
		"table.list_sheets" => {
			extract_concrete_table_path(goal).is_some()
				&& explicit_table_action_tool(goal) == Some("table.list_sheets")
		}
		"table.preview" => {
			extract_concrete_table_path(goal).is_some()
				&& explicit_table_action_tool(goal) == Some("table.preview")
		}
		"table.schema" => {
			extract_concrete_table_path(goal).is_some()
				&& explicit_table_action_tool(goal) == Some("table.schema")
		}
		_ => {
			tool_required_argument_keys(tool_name, Some(catalog)).is_empty()
				|| ground_tool_arguments(tool_name, goal).is_some()
		}
	}
}

fn deterministic_grounded_intent(goal: &str) -> Option<IntentFamily> {
	if grounded_python_code_allows_execution(goal) || grounded_shell_command_allows_execution(goal)
	{
		return Some(IntentFamily::CodeExec);
	}
	if goal_requests_web_lookup(goal) && extract_web_query(goal).is_some() {
		return Some(IntentFamily::WebLookup);
	}
	if extract_explicit_table_path(goal).is_some() {
		return Some(IntentFamily::TableRead);
	}
	if extract_glob_pattern(goal).is_some() || !extract_explicit_path_candidates(goal).is_empty() {
		return Some(IntentFamily::FilesystemRead);
	}
	None
}

fn explicit_filesystem_action_tool(goal: &str) -> Option<&'static str> {
	let lower = action_text_without_explicit_paths(goal);
	if contains_any(
		&lower,
		&[
			"does ",
			" exist",
			"exists",
			"is there ",
			"whether ",
			"存在吗",
			"是否存在",
		],
	) {
		return Some("fs.exists");
	}
	if contains_any(
		&lower,
		&[
			"find ",
			"locate ",
			"where is",
			"where are",
			"在哪",
			"在哪里",
			"path to",
			"路径",
		],
	) {
		return Some("fs.find");
	}
	if contains_any(
		&lower,
		&[
			"inspect",
			"metadata",
			"stat ",
			"file info",
			"details for",
			"元数据",
		],
	) {
		return Some("fs.inspect");
	}
	if contains_any(
		&lower,
		&[
			"list ",
			"show files",
			"show directories",
			"directory contents",
			"folder contents",
			"contents of the directory",
			"列出",
			"目录内容",
		],
	) {
		return Some("fs.list_dir");
	}
	if contains_any(
		&lower,
		&[
			"read ",
			"open ",
			"print ",
			"output ",
			"contents of",
			"file contents",
			"first part",
			"first lines",
			"line ",
			"读取",
			"打开",
			"输出",
			"内容",
		],
	) || (goal.contains('第') && goal.contains('行'))
	{
		return Some("fs.read_text");
	}
	None
}

fn explicit_table_action_tool(goal: &str) -> Option<&'static str> {
	let lower = action_text_without_explicit_paths(goal);
	if contains_any(&lower, &["schema", "columns", "column names", "列", "表头"]) {
		return Some("table.schema");
	}
	if contains_any(
		&lower,
		&[
			"list sheets",
			"show sheets",
			"which sheets",
			"sheet names",
			"工作表",
		],
	) {
		return Some("table.list_sheets");
	}
	if contains_any(
		&lower,
		&[
			"preview",
			"first rows",
			"top rows",
			"rows of",
			"前几行",
			"预览",
		],
	) {
		return Some("table.preview");
	}
	if contains_any(
		&lower,
		&["inspect", "metadata", "details", "summary of the table"],
	) {
		return Some("table.inspect");
	}
	None
}

fn contains_any(goal: &str, markers: &[&str]) -> bool {
	markers.iter().any(|marker| goal.contains(marker))
}

/// Check whether the user goal text expresses an intent to install or use a skill,
/// as opposed to merely asking about a URL.
fn goal_implies_install_intent(goal: &str) -> bool {
	let lowered = goal.to_ascii_lowercase();
	const INSTALL_MARKERS: &[&str] = &[
		"install",
		"安装",
		"添加",
		"add skill",
		"use skill",
		"用这个skill",
		"用这个 skill",
		"enable",
		"启用",
	];
	INSTALL_MARKERS
		.iter()
		.any(|marker| lowered.contains(marker))
}

fn action_text_without_explicit_paths(goal: &str) -> String {
	let mut lowered = goal.to_ascii_lowercase();
	for path in extract_explicit_path_candidates(goal) {
		let path = path.to_ascii_lowercase();
		lowered = lowered.replace(&path, " ");
	}
	lowered
}

fn broad_filesystem_candidate_tools(goal: &str) -> Vec<String> {
	if extract_glob_pattern(goal).is_some() {
		return vec![
			"fs.glob".to_string(),
			"fs.find".to_string(),
			"fs.inspect".to_string(),
		];
	}
	if !extract_concrete_path_candidates(goal).is_empty() {
		return vec![
			"fs.inspect".to_string(),
			"fs.read_text".to_string(),
			"fs.list_dir".to_string(),
		];
	}
	vec![
		"fs.find".to_string(),
		"fs.glob".to_string(),
		"fs.inspect".to_string(),
	]
}

fn broad_table_candidate_tools() -> Vec<String> {
	vec![
		"table.inspect".to_string(),
		"table.preview".to_string(),
		"table.list_sheets".to_string(),
	]
}

fn deterministic_seed_candidate_tools(tool_name: &str, goal: &str) -> Vec<String> {
	match tool_name {
		name if name.starts_with("fs.") || name.starts_with("table.") => {
			let _ = goal;
			vec![tool_name.to_string()]
		}
		_ => vec![tool_name.to_string()],
	}
}

fn normalize_direct_route_seed(goal: &str, decision: &mut RouteDecision) {
	match decision.intent_family {
		IntentFamily::FilesystemRead => {
			decision.candidate_tools = merge_preserving_seed(
				decision.candidate_tools.clone(),
				broad_filesystem_candidate_tools(goal),
			);
		}
		IntentFamily::TableRead => {
			decision.candidate_tools = merge_preserving_seed(
				decision.candidate_tools.clone(),
				broad_table_candidate_tools(),
			);
		}
		_ => {}
	}
}

fn merge_preserving_seed(seeds: Vec<String>, starter_set: Vec<String>) -> Vec<String> {
	let mut tools = dedup_tools([seeds, starter_set].concat());
	tools.truncate(3);
	tools
}

fn dedup_tools(tools: Vec<String>) -> Vec<String> {
	let mut deduped = Vec::new();
	for tool in tools {
		if !deduped.iter().any(|existing| existing == &tool) {
			deduped.push(tool);
		}
	}
	deduped
}

fn build_tool_loop_route(
	context: &RouteClassifierContext<'_>,
	mut decision: RouteDecision,
	preferred_tool: Option<&str>,
	bound_resources: Vec<ResourceSelector>,
) -> RouteDecisionResult {
	let seeded_tools = decision.candidate_tools.clone();
	decision.candidate_tools = context
		.availability_snapshot
		.filter_candidate_tools(preferred_tool, &seeded_tools);
	if decision.candidate_plugins.is_empty() {
		decision.candidate_plugins = decision
			.candidate_tools
			.first()
			.map(|tool_name| candidate_plugins_for_tool(context.plugin_snapshot, tool_name))
			.unwrap_or_default();
	}
	RouteDecisionResult::Direct(DirectRoutePlan {
		decision,
		bound_resources,
	})
}

fn build_loop_hint_route(
	context: &RouteClassifierContext<'_>,
	decision: RouteDecision,
) -> RouteDecisionResult {
	build_tool_loop_route(context, decision, None, Vec::new())
}

fn build_skill_route_result(
	context: &RouteClassifierContext<'_>,
	selector: ResourceSelector,
	descriptor: &CatalogDescriptor,
	execution_requested: bool,
	reason: impl Into<String>,
) -> RouteDecisionResult {
	let executable_skill = skill_descriptor_is_executable(descriptor);
	if executable_skill && execution_requested && !context.skill_execution_available {
		return RouteDecisionResult::Escalate(RouteEscalationPlan {
			decision: RouteDecision::new(
				IntentFamily::TextTransform,
				0.88,
				false,
				resource_risk(descriptor),
				Vec::new(),
				vec!["skill-source-local".to_string()],
				Vec::new(),
				format!(
					"installed skill `{}` matched, but executable skill routes are unavailable in this runtime",
					selector.name()
				),
			),
			reason: EscalationReason::NoEnabledRouteTarget,
			action: EscalationAction::FallbackAnswer,
		});
	}

	let executable = executable_skill && execution_requested;
	let decision = RouteDecision::new(
		IntentFamily::TextTransform,
		0.94,
		false,
		resource_risk(descriptor),
		vec![if executable {
			tool_name_for_role(context.tool_config, BuiltinToolRole::SkillExecute)
		} else {
			"general.execute".to_string()
		}],
		if executable {
			vec![
				"builtin-tools".to_string(),
				"skill-source-local".to_string(),
			]
		} else {
			vec!["skill-source-local".to_string()]
		},
		Vec::new(),
		reason,
	);
	let preferred_tool = decision.candidate_tools.first().cloned();
	build_tool_loop_route(context, decision, preferred_tool.as_deref(), vec![selector])
}

fn discoverable_tool_matches(
	matches: Vec<CatalogMatch>,
	availability_snapshot: &RuntimeVisibleToolAvailabilitySnapshot,
) -> Vec<CatalogMatch> {
	matches
		.into_iter()
		.filter(|entry| {
			entry.descriptor.discoverable
				&& availability_snapshot.is_tool_enabled(&entry.descriptor.name)
		})
		.collect()
}

fn explicit_skill_selector(catalog: &ResourceCatalog, goal: &str) -> Option<ResourceSelector> {
	let explicit_tokens = explicit_skill_tokens(goal);
	let mut entries = catalog
		.entries()
		.iter()
		.filter(|entry| entry.kind == ResourceKind::Skill)
		.collect::<Vec<_>>();
	entries.sort_by(|left, right| right.name.len().cmp(&left.name.len()));
	entries.into_iter().find_map(|entry| {
		let normalized_name = normalize(&entry.name);
		(normalized_name.len() > 2
			&& explicit_tokens
				.iter()
				.any(|token| token == &normalized_name))
		.then(|| entry.selector.clone())
	})
}

fn best_skill_selector(
	skill_matches: &[CatalogMatch],
	tool_matches: &[CatalogMatch],
) -> Option<ResourceSelector> {
	let skill = skill_matches.first()?;
	let tool_score = tool_matches
		.first()
		.map(|entry| entry.score)
		.unwrap_or_default();
	(skill.score >= MIN_SKILL_SCORE && skill.score >= tool_score * 1.10)
		.then(|| skill.descriptor.selector.clone())
}

fn extract_explicit_path_candidates(goal: &str) -> Vec<String> {
	shared_extract_explicit_path_candidates(goal)
}

fn extract_concrete_path_candidates(goal: &str) -> Vec<String> {
	shared_extract_concrete_path_candidates(goal)
}

fn extract_concrete_table_path(goal: &str) -> Option<String> {
	shared_extract_concrete_table_path(goal)
}

fn extract_explicit_shell_command(goal: &str) -> Option<String> {
	shared_extract_explicit_shell_command(goal)
}

fn explicit_skill_tokens(goal: &str) -> Vec<String> {
	let mut tokens = goal
		.split_whitespace()
		.map(clean_token)
		.filter(|token| !token.is_empty())
		.filter(|token| !looks_like_path_candidate(token))
		.filter(|token| !token.starts_with("http://") && !token.starts_with("https://"))
		.map(|token| normalize(&token))
		.filter(|token| !token.is_empty())
		.collect::<Vec<_>>();
	tokens.dedup();
	tokens
}

fn extract_glob_pattern(goal: &str) -> Option<String> {
	shared_extract_glob_pattern(goal)
}

fn extract_explicit_python_code(goal: &str) -> Option<String> {
	if let Some(code) = extract_fenced_python_code(goal) {
		return Some(code);
	}
	if let Some(code) = extract_inline_code(goal).filter(|code| is_probable_python_snippet(code)) {
		return Some(code);
	}
	extract_line_or_block_python_code(goal)
}

fn clean_token(token: &str) -> String {
	if matches!(token, "." | "..") {
		return token.to_string();
	}
	token
		.trim_matches(|character: char| {
			matches!(
				character,
				'"' | '\'' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | ',' | ';' | '!' | '?'
			)
		})
		.trim_end_matches(':')
		.trim_end_matches('.')
		.to_string()
}

fn looks_like_path_candidate(token: &str) -> bool {
	if token.is_empty() {
		return false;
	}
	if token == "." || token == ".." {
		return true;
	}
	token.contains('/')
		|| token.contains('\\')
		|| token.rsplit_once('.').is_some_and(|(stem, ext)| {
			!ext.is_empty()
				&& ext
					.chars()
					.all(|character| character.is_ascii_alphanumeric())
				&& (looks_like_known_file_extension(ext)
					|| stem.contains('-')
					|| stem.contains('_')
					|| stem.chars().any(|character| character.is_ascii_uppercase())
					|| stem.chars().any(|character| character.is_ascii_digit()))
		})
}

fn looks_like_known_file_extension(extension: &str) -> bool {
	matches!(
		extension.to_ascii_lowercase().as_str(),
		"txt"
			| "md" | "markdown"
			| "rs" | "toml"
			| "json" | "yaml"
			| "yml" | "csv"
			| "tsv" | "xlsx"
			| "xls" | "env"
			| "lock" | "log"
			| "py" | "js"
			| "ts" | "jsx"
			| "tsx" | "html"
			| "css" | "sh"
			| "bash" | "zsh"
			| "sql"
	)
}

fn skill_descriptor_is_executable(descriptor: &CatalogDescriptor) -> bool {
	descriptor
		.tags
		.iter()
		.any(|tag| tag == "executable-skill" || tag == "has-scripts")
		|| descriptor
			.key_commands
			.iter()
			.any(|value| looks_like_script_reference(value))
}

fn looks_like_script_reference(value: &str) -> bool {
	let normalized = value.trim().to_ascii_lowercase();
	normalized.contains("scripts/") || normalized.starts_with("./scripts/")
}

fn normalize(value: &str) -> String {
	value
		.chars()
		.filter(|character| character.is_ascii_alphanumeric())
		.collect::<String>()
		.to_ascii_lowercase()
}

fn decision_requests_skill_local_context(
	context: &RouteClassifierContext<'_>,
	decision: &RouteDecision,
) -> bool {
	let execute_tool_name = tool_name_for_role(context.tool_config, BuiltinToolRole::SkillExecute);
	decision
		.candidate_tools
		.iter()
		.any(|tool_name| tool_name == &execute_tool_name || tool_name == "skill.execute")
		|| decision
			.candidate_plugins
			.iter()
			.any(|plugin_id| plugin_id == "skill-source-local")
}

fn decision_requests_skill_execution(
	context: &RouteClassifierContext<'_>,
	decision: &RouteDecision,
) -> bool {
	let execute_tool_name = tool_name_for_role(context.tool_config, BuiltinToolRole::SkillExecute);
	decision
		.candidate_tools
		.iter()
		.any(|tool_name| tool_name == &execute_tool_name || tool_name == "skill.execute")
}

fn extract_fenced_python_code(goal: &str) -> Option<String> {
	let fenced = goal.find("```")?;
	let rest = goal.get(fenced + 3..)?;
	let rest = rest.strip_prefix("python").unwrap_or(rest);
	let rest = rest.strip_prefix('\n').unwrap_or(rest);
	let end = rest.find("```")?;
	let code = rest.get(..end)?.trim();
	(!code.is_empty()).then(|| code.to_string())
}

fn extract_inline_code(goal: &str) -> Option<String> {
	let start = goal.find('`')?;
	let rest = goal.get(start + 1..)?;
	let end = rest.find('`')?;
	let code = rest.get(..end)?.trim();
	(!code.is_empty()).then(|| code.to_string())
}

fn extract_line_or_block_python_code(goal: &str) -> Option<String> {
	let trimmed = goal.trim();
	if let Some(suffix) = extract_python_suffix_after_separator(trimmed) {
		return Some(suffix);
	}
	if is_probable_python_snippet(trimmed) {
		return Some(trimmed.to_string());
	}

	let mut lines = trimmed.lines().map(str::trim_end).collect::<Vec<_>>();
	while lines.first().is_some_and(|line| line.trim().is_empty()) {
		lines.remove(0);
	}
	while lines.last().is_some_and(|line| line.trim().is_empty()) {
		lines.pop();
	}
	if lines.len() < 2 {
		return None;
	}
	let block_start = lines
		.iter()
		.position(|line| is_probable_python_snippet(line.trim()))
		.unwrap_or(1);
	let code = lines
		.iter()
		.skip(block_start)
		.copied()
		.collect::<Vec<_>>()
		.join("\n")
		.trim()
		.to_string();
	(!code.is_empty() && is_probable_python_snippet(code.lines().next().unwrap_or_default()))
		.then_some(code)
}

fn extract_python_suffix_after_separator(value: &str) -> Option<String> {
	value
		.match_indices([':', '：'])
		.filter_map(|(index, _)| {
			let prefix = value.get(..index)?.trim();
			let suffix = value.get(index + 1..)?.trim();
			(!suffix.is_empty()
				&& is_probable_python_snippet(suffix)
				&& !is_probable_python_snippet(prefix))
			.then(|| suffix.to_string())
		})
		.next_back()
}

fn is_probable_python_snippet(value: &str) -> bool {
	let trimmed = value.trim();
	if trimmed.is_empty() {
		return false;
	}
	if trimmed.lines().count() > 1 {
		return trimmed.lines().all(|line| {
			let line = line.trim();
			line.is_empty() || is_probable_python_statement(line)
		});
	}
	is_probable_python_statement(trimmed)
}

fn is_probable_python_statement(line: &str) -> bool {
	let trimmed = line.trim();
	if trimmed.is_empty() {
		return false;
	}
	let punctuation_score = [
		trimmed.contains('('),
		trimmed.contains(')'),
		trimmed.contains(':'),
		trimmed.contains('='),
		trimmed.contains('['),
		trimmed.contains(']'),
	]
	.into_iter()
	.filter(|flag| *flag)
	.count();
	let keyword_score = [
		trimmed.starts_with("print"),
		trimmed.starts_with("for "),
		trimmed.starts_with("if "),
		trimmed.starts_with("while "),
		trimmed.starts_with("def "),
		trimmed.starts_with("class "),
		trimmed.starts_with("import "),
		trimmed.starts_with("from "),
		trimmed.starts_with("return "),
	]
	.into_iter()
	.filter(|flag| *flag)
	.count();
	(keyword_score > 0 || punctuation_score >= 2)
		&& !trimmed.contains("://")
		&& !trimmed.contains('，')
}

fn tool_name_for_role(tool_config: &ToolCatalogConfig, role: BuiltinToolRole) -> String {
	tool_config
		.tool_for_role(role)
		.map(|tool| tool.name.clone())
		.unwrap_or_else(|| role.as_str().to_string())
}

fn candidate_plugins_for_tool(
	plugin_snapshot: &PluginRegistrySnapshot,
	tool_name: &str,
) -> Vec<String> {
	let mut plugins = Vec::new();
	if matches!(
		tool_name,
		"skill.install"
			| "skill.ensure_installed"
			| "skill.execute"
			| "inventory.describe"
			| "research.synthesize"
			| "data.execute"
			| "review.assess"
			| "general.execute"
	) && plugin_snapshot.is_plugin_enabled("builtin-tools")
	{
		plugins.push("builtin-tools".to_string());
	}
	if tool_name.starts_with("skill.") && plugin_snapshot.is_plugin_enabled("skill-source-local") {
		plugins.push("skill-source-local".to_string());
	}
	if tool_name.starts_with("fs.") && plugin_snapshot.is_plugin_enabled("core-fs") {
		plugins.push("core-fs".to_string());
	}
	if tool_name.starts_with("table.") && plugin_snapshot.is_plugin_enabled("core-table") {
		plugins.push("core-table".to_string());
	}
	if tool_name.starts_with("web.") && plugin_snapshot.is_plugin_enabled("core-web") {
		plugins.push("core-web".to_string());
	}
	if tool_name.starts_with("command.") && plugin_snapshot.is_plugin_enabled("core-command") {
		plugins.push("core-command".to_string());
	}
	if tool_name.starts_with("python.") && plugin_snapshot.is_plugin_enabled("core-python") {
		plugins.push("core-python".to_string());
	}
	plugins
}

fn resource_risk(descriptor: &CatalogDescriptor) -> RouteRisk {
	match descriptor.risk {
		roku_plugin_catalog::ResourceRisk::Low => RouteRisk::Low,
		roku_plugin_catalog::ResourceRisk::Medium => RouteRisk::Medium,
		roku_plugin_catalog::ResourceRisk::High => RouteRisk::High,
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::collections::BTreeSet;

	use roku_plugin_catalog::{ResourceCost, ResourceRisk};
	use roku_plugin_tools::RuntimeVisibleToolAvailabilitySnapshot;

	fn tool_descriptor(name: &str) -> CatalogDescriptor {
		CatalogDescriptor {
			selector: ResourceSelector::tool(name),
			kind: ResourceKind::Tool,
			name: name.to_string(),
			role: Some("test".to_string()),
			description: "Canonical descriptor that should not be exposed in the route inventory."
				.to_string(),
			selection_hint: "Compact selection hint.".to_string(),
			discoverable: true,
			tags: vec!["test".to_string()],
			examples: vec!["Cold-path example".to_string()],
			input_schema: vec!["query".to_string()],
			risk: ResourceRisk::Low,
			cost: ResourceCost::default(),
			required_capabilities: Vec::new(),
			summary: "Compact selection hint.".to_string(),
			key_commands: Vec::new(),
			use_cases: Vec::new(),
			contract: Some(roku_common_types::ToolContract {
				grounding: roku_common_types::ToolGroundingContract {
					required_argument_keys: vec!["query".to_string()],
					grounding_strategy: roku_common_types::GroundingStrategy::PatternBased,
					grounding_argument: Some("query".to_string()),
					requires_grounded_path: false,
					bootstrap_matchable: true,
					missing_argument_hint: None,
					extraction_hint: roku_common_types::ExtractionHint::Default,
					static_extra_arguments: serde_json::Map::new(),
				},
				..roku_common_types::ToolContract::default()
			}),
		}
	}

	#[test]
	fn llm_candidates_emit_compact_selection_inventory_without_examples() {
		let catalog = ResourceCatalog::new(vec![tool_descriptor("web.search")]);
		let availability_snapshot = RuntimeVisibleToolAvailabilitySnapshot {
			enabled_tools: ["web.search".to_string()]
				.into_iter()
				.collect::<BTreeSet<_>>(),
			baseline_visible_tools: Vec::new(),
		};
		let candidates = llm_candidates(
			&catalog,
			&availability_snapshot,
			&AgentRuntimeConfig::default(),
		);
		let candidate = candidates
			.first()
			.expect("selection inventory should include one candidate");

		assert_eq!(
			candidate
				.get("selection_hint")
				.and_then(|value| value.as_str()),
			Some("Compact selection hint.")
		);
		assert!(candidate.get("description").is_none());
		assert!(candidate.get("example").is_none());
		assert_eq!(
			candidate
				.get("required_arguments")
				.and_then(|value| value.as_array())
				.expect("required arguments should serialize"),
			&vec![serde_json::Value::String("query".to_string())]
		);
	}

	#[test]
	fn executable_skill_detection_ignores_examples_only_signal() {
		let descriptor = CatalogDescriptor {
			selector: ResourceSelector::skill("skill-creator"),
			kind: ResourceKind::Skill,
			name: "skill-creator".to_string(),
			role: None,
			description: "Create skills".to_string(),
			selection_hint: "Create skills".to_string(),
			discoverable: true,
			tags: Vec::new(),
			examples: vec!["./scripts/run.sh".to_string()],
			input_schema: Vec::new(),
			risk: ResourceRisk::Low,
			cost: ResourceCost::default(),
			required_capabilities: Vec::new(),
			summary: "Create skills".to_string(),
			key_commands: Vec::new(),
			use_cases: Vec::new(),
			contract: None,
		};

		assert!(!skill_descriptor_is_executable(&descriptor));
	}

	#[test]
	fn build_tool_loop_route_filters_shortlist_from_availability_snapshot() {
		let catalog = ResourceCatalog::new(vec![
			tool_descriptor("general.execute"),
			tool_descriptor("web.search"),
		]);
		let tool_config = ToolCatalogConfig::default();
		let agent_runtime_config = AgentRuntimeConfig::default();
		let plugin_snapshot = PluginRegistrySnapshot::permissive();
		let availability_snapshot = RuntimeVisibleToolAvailabilitySnapshot {
			enabled_tools: ["general.execute".to_string()]
				.into_iter()
				.collect::<BTreeSet<_>>(),
			baseline_visible_tools: Vec::new(),
		};
		let context = RouteClassifierContext {
			catalog: &catalog,
			tool_config: &tool_config,
			agent_runtime_config: &agent_runtime_config,
			plugin_snapshot: &plugin_snapshot,
			availability_snapshot: &availability_snapshot,
			route_router: None,
			skill_execution_available: false,
		};
		let decision = RouteDecision::new(
			IntentFamily::WebLookup,
			0.91,
			false,
			RouteRisk::Low,
			vec!["general.execute".to_string(), "web.search".to_string()],
			Vec::new(),
			Vec::new(),
			"snapshot-owned shortlist filtering",
		);

		let RouteDecisionResult::Direct(plan) =
			build_tool_loop_route(&context, decision, Some("web.search"), Vec::new())
		else {
			panic!("expected direct tool-loop route");
		};

		assert_eq!(
			plan.decision.candidate_tools,
			vec!["general.execute".to_string()]
		);
	}
}
