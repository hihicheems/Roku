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
use std::path::PathBuf;

use roku_common_types::{RequestEnvelope, ResourceSelector};
use roku_plugin_catalog::{CatalogDescriptor, CatalogMatch, ResourceCatalog, ResourceKind};
use roku_plugin_core::PluginRegistrySnapshot;
use roku_plugin_llm::{GenerationRequest, LlmRouter, RiskTier, StructuredGenerationError};
use serde_json::json;

use crate::router::{
	DirectRouteKind, DirectRoutePlan, EscalationAction, EscalationReason, FsCommandStep,
	IntentFamily, RouteDecision, RouteDecisionResult, RouteEscalationPlan, RouteRisk,
};
use crate::runtime_loop::{
	extract_path_candidates as shared_extract_path_candidates, extract_skill_source_url,
};
use crate::tool_config::{BuiltinToolRole, ToolCatalogConfig};

const MIN_TOOL_SCORE: f32 = 0.60;
const MIN_SKILL_SCORE: f32 = 0.72;
const ROUTE_CONFIDENCE_FLOOR: f32 = 0.65;

pub(crate) struct RouteClassifierContext<'a> {
	pub(crate) catalog: &'a ResourceCatalog,
	pub(crate) tool_config: &'a ToolCatalogConfig,
	pub(crate) plugin_snapshot: &'a PluginRegistrySnapshot,
	pub(crate) route_router: Option<&'a LlmRouter>,
	pub(crate) skill_execution_available: bool,
}

pub(crate) fn classify_request(
	context: RouteClassifierContext<'_>,
	request: &RequestEnvelope,
) -> RouteDecisionResult {
	if let Some(result) = deterministic_pre_classify(&context, request) {
		return result;
	}

	match context.route_router {
		Some(router) => classify_with_llm(&context, request, router),
		None => unresolved_without_route_model(),
	}
}

fn deterministic_pre_classify(
	context: &RouteClassifierContext<'_>,
	request: &RequestEnvelope,
) -> Option<RouteDecisionResult> {
	if let Some(result) = classify_shell_like_fs_sequence(context, request) {
		return Some(result);
	}

	if let Some(result) = classify_structured_multi_step_request(context, request) {
		return Some(result);
	}

	if extract_skill_source_url(&request.goal).is_some() {
		let decision = RouteDecision::new(
			IntentFamily::TextTransform,
			0.98,
			false,
			RouteRisk::Medium,
			vec![tool_name_for_role(
				context.tool_config,
				BuiltinToolRole::SkillInstall,
			)],
			vec![
				"builtin-tools".to_string(),
				"skill-source-local".to_string(),
			],
			Vec::new(),
			"explicit skill install url detected in user request",
		);
		return Some(RouteDecisionResult::Direct(DirectRoutePlan {
			decision,
			kind: DirectRouteKind::ToolLoop,
			bound_resources: Vec::new(),
		}));
	}

	if let Some(result) = classify_grounded_direct_route(context, request) {
		return Some(result);
	}

	let query = request.goal.trim();
	let tool_matches =
		discoverable_tool_matches(context.catalog.retrieve(query, Some(ResourceKind::Tool), 8));
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

	if let Some(result) = classify_catalog_selected_route(context, request, &tool_matches) {
		return Some(result);
	}

	let selected_tools = best_tool_selectors(&tool_matches);
	if selected_tools.len() > 1 {
		if context.route_router.is_some() {
			return None;
		}
		let decision = RouteDecision::new(
			IntentFamily::MultiStep,
			0.72,
			true,
			RouteRisk::Medium,
			selected_tools
				.iter()
				.filter_map(|selector| context.catalog.descriptor(selector))
				.map(|descriptor| descriptor.name.clone())
				.collect(),
			vec!["builtin-tools".to_string()],
			Vec::new(),
			"request likely needs multi-step coordination beyond a direct single-tool route",
		);
		return Some(build_loop_hint_route(context, decision));
	}

	if let Some(result) = classify_structural_fallback(context, request) {
		return Some(result);
	}

	if context.route_router.is_none() && tool_selector(context.catalog, "general.execute").is_some()
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
	let explicit_paths = extract_path_candidates(&request.goal);
	let has_explicit_code = extract_explicit_python_code(&request.goal).is_some();
	let looks_multi_step =
		has_shell_command_chain(&request.goal) || (!has_explicit_code && explicit_paths.len() > 1);
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

fn classify_grounded_direct_route(
	context: &RouteClassifierContext<'_>,
	request: &RequestEnvelope,
) -> Option<RouteDecisionResult> {
	if let Some(result) = classify_shell_like_fs_command(context, request) {
		return Some(result);
	}

	if let Some(code) = extract_explicit_python_code(&request.goal)
		&& let Some(selector) = tool_selector(context.catalog, "python.run")
	{
		let decision = RouteDecision::new(
			IntentFamily::CodeExec,
			0.93,
			false,
			RouteRisk::Medium,
			vec!["python.run".to_string()],
			candidate_plugins_for_tool(context.plugin_snapshot, "python.run"),
			Vec::new(),
			"grounded explicit Python code allows a direct `python.run` route",
		);
		let _ = selector;
		let _ = code;
		return Some(build_tool_loop_route(
			context,
			decision,
			Some("python.run"),
			Vec::new(),
		));
	}

	if extract_glob_pattern(&request.goal).is_some()
		&& tool_selector(context.catalog, "fs.glob").is_some()
	{
		let decision = RouteDecision::new(
			IntentFamily::FilesystemRead,
			0.92,
			false,
			RouteRisk::Low,
			vec!["fs.glob".to_string()],
			candidate_plugins_for_tool(context.plugin_snapshot, "fs.glob"),
			Vec::new(),
			"grounded filesystem glob allows a direct `fs.glob` route",
		);
		return Some(build_filesystem_loop_route(
			context,
			request,
			decision,
			Some("fs.glob"),
			None,
		));
	}

	if extract_table_path(&request.goal).is_some()
		&& has_enabled_tool_with_prefix(context.catalog, "table.")
	{
		return None;
	}

	let explicit_paths = extract_path_candidates(&request.goal);
	let path = single_explicit_path(&explicit_paths)?;
	let resolved_path = resolve_grounded_path_candidate(path).unwrap_or_else(|| path.to_string());
	let tool_name = grounded_fs_tool_name(&resolved_path)?;
	let decision = RouteDecision::new(
		IntentFamily::FilesystemRead,
		0.91,
		false,
		RouteRisk::Low,
		vec![tool_name.to_string()],
		candidate_plugins_for_tool(context.plugin_snapshot, tool_name),
		Vec::new(),
		format!("grounded filesystem target resolved to direct `{tool_name}`"),
	);
	Some(build_filesystem_loop_route(
		context,
		request,
		decision,
		Some(tool_name),
		None,
	))
}

fn classify_shell_like_fs_command(
	context: &RouteClassifierContext<'_>,
	request: &RequestEnvelope,
) -> Option<RouteDecisionResult> {
	let command = parse_shell_like_fs_command(&request.goal)?;
	tool_selector(context.catalog, command.tool_name)?;
	let decision = RouteDecision::new(
		IntentFamily::FilesystemRead,
		0.94,
		false,
		RouteRisk::Low,
		vec![command.tool_name.to_string()],
		candidate_plugins_for_tool(context.plugin_snapshot, command.tool_name),
		Vec::new(),
		format!(
			"structured shell-style filesystem command resolved to a filesystem loop shortlist led by `{}`",
			command.tool_name
		),
	);
	Some(build_filesystem_loop_route(
		context,
		request,
		decision,
		Some(command.tool_name),
		None,
	))
}

fn classify_shell_like_fs_sequence(
	context: &RouteClassifierContext<'_>,
	request: &RequestEnvelope,
) -> Option<RouteDecisionResult> {
	let commands = parse_shell_like_fs_sequence(&request.goal)?;
	if commands.len() < 2 && !matches!(commands.as_slice(), [FsCommandStep::PrintWorkingDir]) {
		return None;
	}
	let mut candidate_tools = Vec::new();
	for command in &commands {
		let tool_name = match command {
			FsCommandStep::ChangeDir { .. } | FsCommandStep::ListDir { .. } => "fs.list_dir",
			FsCommandStep::ReadText { .. } => "fs.read_text",
			FsCommandStep::PrintWorkingDir | FsCommandStep::Inspect { .. } => "fs.inspect",
			FsCommandStep::Exists { .. } => "fs.exists",
		};
		if !candidate_tools.iter().any(|existing| existing == tool_name) {
			candidate_tools.push(tool_name.to_string());
		}
	}
	let decision = RouteDecision::new(
		IntentFamily::FilesystemRead,
		0.95,
		false,
		RouteRisk::Low,
		candidate_tools,
		candidate_plugins_for_tool(context.plugin_snapshot, "fs.list_dir"),
		Vec::new(),
		"structured shell-style filesystem command sequence resolved to a bounded direct route",
	);
	Some(build_filesystem_loop_route(
		context,
		request,
		decision,
		Some("fs.list_dir"),
		Some(commands),
	))
}

struct ShellLikeFsCommand {
	tool_name: &'static str,
}

fn parse_shell_like_fs_command(goal: &str) -> Option<ShellLikeFsCommand> {
	let tokens = goal.split_whitespace().map(clean_token).collect::<Vec<_>>();
	let first = tokens.first()?.as_str();
	match first {
		"ls" | "dir" => Some(ShellLikeFsCommand {
			tool_name: "fs.list_dir",
		}),
		"cat" | "more" => command_path_argument(goal, &tokens).map(|_| ShellLikeFsCommand {
			tool_name: "fs.read_text",
		}),
		"pwd" => Some(ShellLikeFsCommand {
			tool_name: "fs.inspect",
		}),
		"stat" => command_path_argument(goal, &tokens).map(|_| ShellLikeFsCommand {
			tool_name: "fs.inspect",
		}),
		"exists" => command_path_argument(goal, &tokens).map(|_| ShellLikeFsCommand {
			tool_name: "fs.exists",
		}),
		"test" => {
			if tokens.get(1).is_some_and(|flag| flag == "-e") {
				command_path_argument(goal, &tokens[1..]).map(|_| ShellLikeFsCommand {
					tool_name: "fs.exists",
				})
			} else {
				None
			}
		}
		_ => None,
	}
}

fn parse_shell_like_fs_sequence(goal: &str) -> Option<Vec<FsCommandStep>> {
	let segments = split_shell_like_segments(goal);
	if segments.is_empty() {
		return None;
	}
	let mut commands = Vec::new();
	for segment in segments {
		commands.push(parse_shell_like_fs_step(&segment)?);
	}
	Some(commands)
}

fn split_shell_like_segments(goal: &str) -> Vec<String> {
	goal.replace("\r\n", "\n")
		.replace("&&", "\n")
		.replace("||", "\n")
		.replace(';', "\n")
		.replace("；", "\n")
		.replace("，然后", "\n")
		.replace(",然后", "\n")
		.replace("然后", "\n")
		.replace("，", "\n")
		.lines()
		.filter_map(normalize_shell_segment)
		.collect()
}

fn normalize_shell_segment(segment: &str) -> Option<String> {
	let trimmed = segment.trim();
	if trimmed.is_empty() {
		return None;
	}
	let mut candidates = vec![trimmed.to_string()];
	if let Some((_, suffix)) = trimmed.rsplit_once(':') {
		candidates.push(suffix.trim().to_string());
	}
	if let Some((_, suffix)) = trimmed.rsplit_once('：') {
		candidates.push(suffix.trim().to_string());
	}
	candidates.into_iter().find(|candidate| {
		shell_command_name(candidate.split_whitespace().next().unwrap_or_default()).is_some()
	})
}

fn parse_shell_like_fs_step(segment: &str) -> Option<FsCommandStep> {
	let tokens = segment
		.split_whitespace()
		.map(clean_token)
		.filter(|token| !token.is_empty())
		.collect::<Vec<_>>();
	let first = tokens.first()?.as_str();
	match shell_command_name(first)? {
		"cd" => {
			command_path_argument(segment, &tokens).map(|path| FsCommandStep::ChangeDir { path })
		}
		"ls" => Some(FsCommandStep::ListDir {
			path: command_path_argument(segment, &tokens),
		}),
		"cat" => {
			command_path_argument(segment, &tokens).map(|path| FsCommandStep::ReadText { path })
		}
		"pwd" => Some(FsCommandStep::PrintWorkingDir),
		"stat" => {
			command_path_argument(segment, &tokens).map(|path| FsCommandStep::Inspect { path })
		}
		"exists" => {
			command_path_argument(segment, &tokens).map(|path| FsCommandStep::Exists { path })
		}
		_ => None,
	}
}

fn shell_command_name(token: &str) -> Option<&'static str> {
	match token {
		"cd" => Some("cd"),
		"ls" | "ll" | "dir" => Some("ls"),
		"cat" | "more" => Some("cat"),
		"pwd" => Some("pwd"),
		"stat" => Some("stat"),
		"exists" => Some("exists"),
		"test" => Some("exists"),
		_ => None,
	}
}

fn command_path_argument(goal: &str, tokens: &[String]) -> Option<String> {
	let explicit_paths = extract_path_candidates(goal);
	if let Some(path) = explicit_paths.first() {
		return Some(resolve_grounded_path_candidate(path).unwrap_or_else(|| path.clone()));
	}
	tokens
		.iter()
		.skip(1)
		.filter(|token| !token.is_empty() && !token.starts_with('-'))
		.find(|token| looks_like_path_candidate(token))
		.map(|token| resolve_grounded_path_candidate(token).unwrap_or_else(|| token.clone()))
}

fn grounded_fs_tool_name(path: &str) -> Option<&'static str> {
	if path == "." || path == ".." || path.ends_with('/') || path.ends_with('\\') {
		return Some("fs.list_dir");
	}
	let candidate = std::env::current_dir().ok()?.join(path);
	if let Ok(metadata) = std::fs::metadata(&candidate) {
		if metadata.is_dir() {
			return Some("fs.list_dir");
		}
		if metadata.is_file() {
			return Some("fs.read_text");
		}
	}
	Some("fs.inspect")
}

fn resolve_grounded_path_candidate(raw: &str) -> Option<String> {
	let path = PathBuf::from(raw);
	if path.is_absolute() {
		return path
			.exists()
			.then(|| path.canonicalize().ok())
			.flatten()
			.map(|resolved| resolved.display().to_string());
	}
	let cwd = std::env::current_dir().ok()?;
	let candidate = cwd.join(&path);
	if candidate.exists() {
		return candidate
			.canonicalize()
			.ok()
			.map(|resolved| resolved.display().to_string());
	}
	if raw.contains('/') || raw.contains('\\') || raw == "." || raw == ".." {
		return Some(candidate.display().to_string());
	}
	find_unique_workspace_match(&cwd, raw).map(|resolved| resolved.display().to_string())
}

fn find_unique_workspace_match(root: &std::path::Path, target_name: &str) -> Option<PathBuf> {
	let mut stack = vec![root.to_path_buf()];
	let mut visited = 0_usize;
	let mut matches = Vec::new();
	while let Some(directory) = stack.pop() {
		let entries = fs::read_dir(&directory).ok()?;
		for entry in entries.filter_map(Result::ok) {
			visited += 1;
			if visited > 8_000 {
				return None;
			}
			let path = entry.path();
			let name = entry.file_name().to_string_lossy().to_string();
			if name == target_name {
				matches.push(path.clone());
				if matches.len() > 1 {
					return None;
				}
			}
			if path.is_dir() && !should_skip_workspace_search_dir(&name) {
				stack.push(path);
			}
		}
	}
	matches.into_iter().next()
}

fn should_skip_workspace_search_dir(name: &str) -> bool {
	matches!(
		name,
		".git" | ".roku" | "target" | "node_modules" | "dist" | "build"
	)
}

fn classify_with_llm(
	context: &RouteClassifierContext<'_>,
	request: &RequestEnvelope,
	router: &LlmRouter,
) -> RouteDecisionResult {
	let tool_matches = discoverable_tool_matches(context.catalog.retrieve(
		&request.goal,
		Some(ResourceKind::Tool),
		8,
	));
	let skill_matches = context
		.catalog
		.retrieve(&request.goal, Some(ResourceKind::Skill), 4);
	let candidates = llm_candidates(context.catalog);
	let response = router.generate_json_value(&GenerationRequest {
		system_prompt: Some(
			"You are Roku's route classifier. Return only valid JSON matching the requested schema."
				.to_string(),
		),
		prompt: route_classifier_prompt(request, &candidates),
		expected_output_tokens: 220,
		risk_tier: RiskTier::Low,
		preferred_provider: None,
		budget_tokens_remaining: 2_000,
		budget_cost_remaining_usd: 0.1,
	});
	let value = match response {
		Ok(response) => response.value,
		Err(error) => {
			return llm_classifier_failure_route(context, error);
		}
	};
	let decision = match RouteDecision::from_json_value(&value) {
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
	if !decision.missing_arguments.is_empty() {
		return build_loop_hint_route(context, decision);
	}
	if decision.confidence_score() < ROUTE_CONFIDENCE_FLOOR {
		return build_loop_hint_route(context, decision);
	}
	if decision.requires_multi_step || decision.intent_family == IntentFamily::MultiStep {
		return build_loop_hint_route(context, decision);
	}
	if let Some(result) = classify_skill_route_from_decision(
		context,
		request,
		&decision,
		&skill_matches,
		&tool_matches,
	) {
		return result;
	}
	if decision.intent_family == IntentFamily::Chat {
		return build_tool_loop_route(context, decision, Some("general.execute"), Vec::new());
	}
	if decision.intent_family == IntentFamily::FilesystemRead
		&& has_enabled_tool_with_prefix(context.catalog, "fs.")
	{
		let preferred_tool = decision
			.candidate_tools
			.iter()
			.find(|tool_name| tool_name.starts_with("fs."))
			.cloned();
		return build_filesystem_loop_route(
			context,
			request,
			decision,
			preferred_tool.as_deref(),
			None,
		);
	}
	if matches!(
		decision.intent_family,
		IntentFamily::TableRead | IntentFamily::WebLookup | IntentFamily::CodeExec
	) {
		let preferred_tool = decision.candidate_tools.first().cloned();
		return build_tool_loop_route(context, decision, preferred_tool.as_deref(), Vec::new());
	}
	if matches!(
		decision.intent_family,
		IntentFamily::MultiStep | IntentFamily::Unknown
	) {
		return build_loop_hint_route(context, decision);
	}
	if let Some(selector) = select_tool_from_candidates(context.catalog, &decision.candidate_tools)
	{
		return build_direct_tool_plan(context, request, decision, selector);
	}

	RouteDecisionResult::Escalate(RouteEscalationPlan {
		decision,
		reason: EscalationReason::NoEnabledRouteTarget,
		action: EscalationAction::FallbackAnswer,
	})
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
		"deterministic pre-classifier did not find a stable direct route and no route model is available",
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

fn llm_candidates(catalog: &ResourceCatalog) -> Vec<serde_json::Value> {
	let mut entries = catalog
		.entries()
		.iter()
		.map(|entry| {
			json!({
				"selector": entry.selector.display_key(),
				"kind": format!("{:?}", entry.kind),
				"name": entry.name,
				"description": entry.description,
				"summary": entry.summary,
				"examples": entry.examples,
			})
		})
		.collect::<Vec<_>>();
	entries.truncate(10);
	entries
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
- Use only candidate tool names from the provided inventory if you name tools.
- Base the decision on the current user goal and the current inventory. Do not inherit intent from prior conversation turns unless the current goal explicitly restates it.
- Use `chat` for greetings or direct assistant conversation.
- Use `filesystem_read`, `table_read`, `web_lookup`, or `code_exec` when the intent clearly asks for those families even if no tool is available yet.
- Use `multi_step` when the request obviously needs a planning-heavy workflow.
- Use `skill.execute` only when the user is asking to actually run an installed script-backed skill and perform side effects.
- If the user is asking to summarize, explain, describe, list, or quote guidance from an installed skill, do not select `skill.execute`; prefer an advisory route with no execution tool.
- Leave `candidate_tools` empty if no current direct tool is safe.
- `missing_arguments` should be empty unless the user must provide something concrete first.

User goal:
{goal}

Current inventory:
{candidates}"#,
		goal = request.goal,
		candidates = serde_json::to_string_pretty(candidates).unwrap_or_default(),
	)
}

fn select_tool_from_candidates(
	catalog: &ResourceCatalog,
	candidate_tools: &[String],
) -> Option<ResourceSelector> {
	candidate_tools.iter().find_map(|tool_name| {
		catalog
			.entries()
			.iter()
			.find(|entry| entry.kind == ResourceKind::Tool && entry.name == *tool_name)
			.map(|entry| entry.selector.clone())
	})
}

fn classify_catalog_selected_route(
	context: &RouteClassifierContext<'_>,
	request: &RequestEnvelope,
	tool_matches: &[CatalogMatch],
) -> Option<RouteDecisionResult> {
	let primary = preferred_catalog_tool_match(tool_matches)?;
	let primary = prefer_pathless_directory_fs_match(request, tool_matches, primary);
	let descriptor = context.catalog.descriptor(&primary.descriptor.selector)?;
	if descriptor.name.starts_with("fs.")
		&& !allow_catalog_filesystem_route(request, &descriptor.name, primary.score)
	{
		return None;
	}
	if context.route_router.is_some()
		&& matches!(
			descriptor.name.as_str(),
			"inventory.describe" | "general.execute"
		) {
		return None;
	}
	let decision = RouteDecision::new(
		intent_family_for_tool(&descriptor.name),
		stable_tool_confidence(primary.score),
		false,
		resource_risk(descriptor),
		vec![descriptor.name.clone()],
		candidate_plugins_for_tool(context.plugin_snapshot, &descriptor.name),
		Vec::new(),
		format!(
			"catalog retrieval selected stable direct route `{}`",
			descriptor.name
		),
	);
	match descriptor.name.as_str() {
		"inventory.describe" => Some(RouteDecisionResult::Direct(DirectRoutePlan {
			decision,
			kind: DirectRouteKind::ToolLoop,
			bound_resources: Vec::new(),
		})),
		"general.execute" => Some(build_tool_loop_route(
			context,
			decision,
			Some("general.execute"),
			Vec::new(),
		)),
		_ => Some(build_direct_tool_plan(
			context,
			request,
			decision,
			descriptor.selector.clone(),
		)),
	}
}

fn prefer_pathless_directory_fs_match<'a>(
	request: &RequestEnvelope,
	matches: &'a [CatalogMatch],
	primary: &'a CatalogMatch,
) -> &'a CatalogMatch {
	if has_grounded_filesystem_input(&request.goal) {
		return primary;
	}

	let directory_semantic_match = matches.iter().find(|candidate| {
		matches!(
			candidate.descriptor.name.as_str(),
			"fs.list_dir" | "fs.inspect"
		) && candidate.score >= 0.46
			&& candidate.score >= primary.score * 0.55
	});

	match primary.descriptor.name.as_str() {
		"inventory.describe" | "general.execute" => directory_semantic_match.unwrap_or(primary),
		"fs.find" | "fs.read_text" | "fs.exists" => directory_semantic_match
			.filter(|candidate| candidate.score >= MIN_TOOL_SCORE)
			.unwrap_or(primary),
		_ => primary,
	}
}

fn allow_catalog_filesystem_route(request: &RequestEnvelope, tool_name: &str, score: f32) -> bool {
	if has_grounded_filesystem_input(&request.goal) {
		return true;
	}

	matches!(tool_name, "fs.list_dir" | "fs.inspect") && score >= 0.72
}

fn has_grounded_filesystem_input(goal: &str) -> bool {
	parse_shell_like_fs_command(goal).is_some()
		|| extract_glob_pattern(goal).is_some()
		|| !extract_path_candidates(goal).is_empty()
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

fn preferred_catalog_tool_match(matches: &[CatalogMatch]) -> Option<&CatalogMatch> {
	let primary = stable_primary_tool_match(matches)?;
	if !matches!(
		primary.descriptor.name.as_str(),
		"inventory.describe" | "general.execute"
	) {
		return Some(primary);
	}
	let secondary = matches.get(1)?;
	let secondary_threshold = direct_route_threshold(&secondary.descriptor.name);
	let secondary_is_specific_tool =
		intent_family_for_tool(&secondary.descriptor.name) != IntentFamily::Chat;
	(secondary_is_specific_tool
		&& secondary.score >= secondary_threshold
		&& secondary.score >= primary.score * 0.85)
		.then_some(secondary)
		.or(Some(primary))
}

fn stable_primary_tool_match(matches: &[CatalogMatch]) -> Option<&CatalogMatch> {
	let first = matches.first()?;
	if first.score < direct_route_threshold(&first.descriptor.name) {
		return None;
	}
	if let Some(second) = matches.get(1) {
		let second_threshold = direct_route_threshold(&second.descriptor.name);
		if second.score >= second_threshold && first.score < second.score * 1.08 {
			return None;
		}
	}
	Some(first)
}

fn direct_route_threshold(tool_name: &str) -> f32 {
	match tool_name {
		"inventory.describe" => 0.42,
		"general.execute" => 0.28,
		_ => MIN_TOOL_SCORE,
	}
}

fn stable_tool_confidence(score: f32) -> f32 {
	(score + 0.20).clamp(0.72, 0.96)
}

fn intent_family_for_tool(tool_name: &str) -> IntentFamily {
	match tool_name {
		"inventory.describe" | "general.execute" => IntentFamily::Chat,
		name if name.starts_with("fs.") => IntentFamily::FilesystemRead,
		name if name.starts_with("table.") => IntentFamily::TableRead,
		name if name.starts_with("web.") => IntentFamily::WebLookup,
		name if name.starts_with("python.") => IntentFamily::CodeExec,
		_ => IntentFamily::TextTransform,
	}
}

fn classify_structural_fallback(
	context: &RouteClassifierContext<'_>,
	request: &RequestEnvelope,
) -> Option<RouteDecisionResult> {
	let goal = request.goal.trim();
	if extract_explicit_python_code(goal).is_some()
		&& tool_selector(context.catalog, "python.run").is_none()
	{
		return Some(unavailable_family_route(
			IntentFamily::CodeExec,
			"explicit Python code was provided, but `python.run` is not enabled in the current runtime inventory",
		));
	}
	if extract_table_path(goal).is_some()
		&& !has_enabled_tool_with_prefix(context.catalog, "table.")
	{
		return Some(unavailable_family_route(
			IntentFamily::TableRead,
			"explicit table input was provided, but core table tools are not enabled in the current runtime inventory",
		));
	}
	if (extract_glob_pattern(goal).is_some() || !extract_path_candidates(goal).is_empty())
		&& !has_enabled_tool_with_prefix(context.catalog, "fs.")
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

fn has_enabled_tool_with_prefix(catalog: &ResourceCatalog, prefix: &str) -> bool {
	catalog
		.entries()
		.iter()
		.any(|entry| entry.kind == ResourceKind::Tool && entry.name.starts_with(prefix))
}

fn build_direct_tool_plan(
	context: &RouteClassifierContext<'_>,
	request: &RequestEnvelope,
	decision: RouteDecision,
	selector: ResourceSelector,
) -> RouteDecisionResult {
	let tool_name = selector.name().to_string();
	if tool_name.starts_with("fs.") {
		return build_filesystem_loop_route(context, request, decision, Some(&tool_name), None);
	}
	build_tool_loop_route(context, decision, Some(&tool_name), Vec::new())
}

fn build_filesystem_loop_route(
	context: &RouteClassifierContext<'_>,
	request: &RequestEnvelope,
	mut decision: RouteDecision,
	preferred_tool: Option<&str>,
	commands: Option<Vec<FsCommandStep>>,
) -> RouteDecisionResult {
	decision.intent_family = IntentFamily::FilesystemRead;
	decision.candidate_tools =
		filesystem_loop_candidate_tools(context.catalog, request, preferred_tool);
	decision.candidate_plugins = if context.plugin_snapshot.is_plugin_enabled("core-fs") {
		vec!["core-fs".to_string()]
	} else {
		Vec::new()
	};
	RouteDecisionResult::Direct(DirectRoutePlan {
		decision,
		kind: DirectRouteKind::FilesystemLoop { commands },
		bound_resources: Vec::new(),
	})
}

fn build_tool_loop_route(
	context: &RouteClassifierContext<'_>,
	mut decision: RouteDecision,
	preferred_tool: Option<&str>,
	bound_resources: Vec<ResourceSelector>,
) -> RouteDecisionResult {
	let seeded_tools = decision.candidate_tools.clone();
	decision.candidate_tools = tool_loop_candidate_tools(
		context.catalog,
		decision.intent_family,
		preferred_tool,
		&seeded_tools,
	);
	if decision.candidate_tools.is_empty() {
		return RouteDecisionResult::Escalate(RouteEscalationPlan {
			decision,
			reason: EscalationReason::NoEnabledRouteTarget,
			action: EscalationAction::FallbackAnswer,
		});
	}
	if decision.candidate_plugins.is_empty() {
		decision.candidate_plugins = decision
			.candidate_tools
			.first()
			.map(|tool_name| candidate_plugins_for_tool(context.plugin_snapshot, tool_name))
			.unwrap_or_default();
	}
	RouteDecisionResult::Direct(DirectRoutePlan {
		decision,
		kind: DirectRouteKind::ToolLoop,
		bound_resources,
	})
}

fn build_loop_hint_route(
	context: &RouteClassifierContext<'_>,
	decision: RouteDecision,
) -> RouteDecisionResult {
	build_tool_loop_route(context, decision, None, Vec::new())
}

fn filesystem_loop_candidate_tools(
	catalog: &ResourceCatalog,
	request: &RequestEnvelope,
	preferred_tool: Option<&str>,
) -> Vec<String> {
	let basename_only = single_explicit_path(&extract_path_candidates(&request.goal))
		.is_some_and(is_basename_reference);
	let preferred_order = match preferred_tool {
		Some("fs.list_dir") => ["fs.list_dir", "fs.inspect", "fs.exists", "fs.read_text"],
		Some("fs.inspect") => ["fs.inspect", "fs.list_dir", "fs.exists", "fs.read_text"],
		Some("fs.exists") => ["fs.exists", "fs.inspect", "fs.read_text", "fs.list_dir"],
		Some("fs.read_text") => ["fs.read_text", "fs.inspect", "fs.exists", "fs.list_dir"],
		_ => ["fs.read_text", "fs.list_dir", "fs.inspect", "fs.exists"],
	};
	let mut tools = Vec::new();
	if basename_only && tool_selector(catalog, "fs.find").is_some() {
		tools.push("fs.find".to_string());
	}
	for tool_name in preferred_order {
		if tool_selector(catalog, tool_name).is_some()
			&& !tools.iter().any(|existing| existing == tool_name)
		{
			tools.push(tool_name.to_string());
		}
	}
	if tool_selector(catalog, "fs.glob").is_some()
		&& !tools.iter().any(|existing| existing == "fs.glob")
	{
		tools.push("fs.glob".to_string());
	}
	tools
}

fn tool_loop_candidate_tools(
	catalog: &ResourceCatalog,
	intent_family: IntentFamily,
	preferred_tool: Option<&str>,
	seed_tools: &[String],
) -> Vec<String> {
	let family_tools = match intent_family {
		IntentFamily::Chat => vec!["general.execute"],
		IntentFamily::TableRead => {
			vec![
				"table.preview",
				"table.inspect",
				"table.list_sheets",
				"table.schema",
			]
		}
		IntentFamily::WebLookup => vec!["web.search"],
		IntentFamily::CodeExec => vec!["python.run"],
		IntentFamily::TextTransform | IntentFamily::MultiStep | IntentFamily::Unknown => {
			vec!["general.execute"]
		}
		IntentFamily::FilesystemRead => Vec::new(),
	};
	let mut tools = Vec::new();
	if let Some(preferred_tool) = preferred_tool
		&& tool_selector(catalog, preferred_tool).is_some()
	{
		tools.push(preferred_tool.to_string());
	}
	for tool_name in family_tools {
		if tool_selector(catalog, tool_name).is_some()
			&& !tools.iter().any(|existing| existing == tool_name)
		{
			tools.push(tool_name.to_string());
		}
	}
	for tool_name in seed_tools {
		if tool_selector(catalog, tool_name).is_some()
			&& !tools.iter().any(|existing| existing == tool_name)
		{
			tools.push(tool_name.clone());
		}
	}
	tools
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
	RouteDecisionResult::Direct(DirectRoutePlan {
		decision,
		kind: DirectRouteKind::ToolLoop,
		bound_resources: vec![selector],
	})
}

fn tool_selector(catalog: &ResourceCatalog, tool_name: &str) -> Option<ResourceSelector> {
	catalog
		.entries()
		.iter()
		.find(|entry| entry.kind == ResourceKind::Tool && entry.name == tool_name)
		.map(|entry| entry.selector.clone())
}

fn discoverable_tool_matches(matches: Vec<CatalogMatch>) -> Vec<CatalogMatch> {
	matches
		.into_iter()
		.filter(|entry| entry.descriptor.discoverable)
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

fn extract_path_candidates(goal: &str) -> Vec<String> {
	shared_extract_path_candidates(goal)
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

fn extract_table_path(goal: &str) -> Option<String> {
	extract_path_candidates(goal).into_iter().find(|path| {
		let normalized = path.to_ascii_lowercase();
		normalized.ends_with(".csv")
			|| normalized.ends_with(".tsv")
			|| normalized.ends_with(".xlsx")
	})
}

fn extract_glob_pattern(goal: &str) -> Option<String> {
	goal.split_whitespace()
		.map(clean_token)
		.find(|token| token.contains('*') || token.contains('?') || token.contains('['))
}

fn extract_explicit_python_code(goal: &str) -> Option<String> {
	if let Some(code) = extract_fenced_python_code(goal) {
		return Some(code);
	}
	if let Some(code) = extract_inline_code(goal) {
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

fn is_basename_reference(path: &str) -> bool {
	!path.is_empty()
		&& path != "."
		&& path != ".."
		&& !PathBuf::from(path).is_absolute()
		&& !path.contains('/')
		&& !path.contains('\\')
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
		|| token.rsplit_once('.').is_some_and(|(_, ext)| {
			!ext.is_empty()
				&& ext
					.chars()
					.all(|character| character.is_ascii_alphanumeric())
		})
}

fn single_explicit_path(paths: &[String]) -> Option<&str> {
	(paths.len() == 1).then(|| paths[0].as_str())
}

fn best_tool_selectors(matches: &[CatalogMatch]) -> Vec<ResourceSelector> {
	let mut selectors = matches
		.iter()
		.filter(|entry| entry.score >= MIN_TOOL_SCORE)
		.take(2)
		.map(|entry| entry.descriptor.selector.clone())
		.collect::<Vec<_>>();
	selectors.dedup();
	selectors
}

fn skill_descriptor_is_executable(descriptor: &CatalogDescriptor) -> bool {
	descriptor
		.tags
		.iter()
		.any(|tag| tag == "executable-skill" || tag == "has-scripts")
		|| descriptor
			.key_commands
			.iter()
			.chain(descriptor.examples.iter())
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
