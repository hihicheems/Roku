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

use std::path::PathBuf;

use roku_common_types::{ConversationRole, RequestEnvelope, ResourceSelector};
use roku_plugin_catalog::{CatalogDescriptor, CatalogMatch, ResourceCatalog, ResourceKind};
use roku_plugin_core::PluginRegistrySnapshot;
use roku_plugin_llm::{GenerationRequest, LlmRouter, RiskTier, StructuredGenerationError};
use roku_plugin_skills::SkillSource;
use serde_json::{Value, json};

use crate::router::{
	DirectRouteKind, DirectRoutePlan, EscalationAction, EscalationReason, IntentFamily,
	RouteDecision, RouteDecisionResult, RouteEscalationPlan, RouteRisk,
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
	if let Some(source_url) = extract_skill_source_url(&request.goal) {
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
			kind: DirectRouteKind::SkillInstall { source_url },
		}));
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
		return Some(RouteDecisionResult::Escalate(RouteEscalationPlan {
			decision,
			reason: EscalationReason::RequiresMultiStep,
			action: EscalationAction::EnterLimitedPlanning,
		}));
	}

	if let Some(result) = classify_structural_fallback(context, request) {
		return Some(result);
	}

	None
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
			return llm_classifier_failure_decision(error);
		}
	};
	let decision = match RouteDecision::from_json_value(&value) {
		Ok(decision) => decision,
		Err(error) => {
			return RouteDecisionResult::Escalate(RouteEscalationPlan {
				decision: RouteDecision::new(
					IntentFamily::Unknown,
					0.0,
					false,
					RouteRisk::Low,
					Vec::new(),
					Vec::new(),
					Vec::new(),
					format!("route classifier returned invalid schema: {error}"),
				),
				reason: EscalationReason::RouteClassifierFailure,
				action: EscalationAction::EnterLimitedPlanning,
			});
		}
	};
	if decision.confidence_score() < ROUTE_CONFIDENCE_FLOOR {
		return RouteDecisionResult::Escalate(RouteEscalationPlan {
			decision,
			reason: EscalationReason::LowConfidence,
			action: EscalationAction::EnterLimitedPlanning,
		});
	}
	if !decision.missing_arguments.is_empty() {
		return RouteDecisionResult::Escalate(RouteEscalationPlan {
			decision,
			reason: EscalationReason::MissingArguments,
			action: EscalationAction::AskForMoreInfo,
		});
	}
	if decision.requires_multi_step || decision.intent_family == IntentFamily::MultiStep {
		return RouteDecisionResult::Escalate(RouteEscalationPlan {
			decision,
			reason: EscalationReason::RequiresMultiStep,
			action: EscalationAction::EnterLimitedPlanning,
		});
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
		return RouteDecisionResult::Direct(DirectRoutePlan {
			decision,
			kind: DirectRouteKind::Conversation,
		});
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

fn llm_classifier_failure_decision(error: StructuredGenerationError) -> RouteDecisionResult {
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
	RouteDecisionResult::Escalate(RouteEscalationPlan {
		decision: RouteDecision::new(
			IntentFamily::Unknown,
			0.0,
			false,
			RouteRisk::Low,
			Vec::new(),
			Vec::new(),
			Vec::new(),
			reason,
		),
		reason: escalation_reason,
		action: EscalationAction::EnterLimitedPlanning,
	})
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
	let history = request
		.conversation_history
		.iter()
		.rev()
		.take(4)
		.map(|turn| format!("{}: {}", role_label(turn.role), turn.content))
		.collect::<Vec<_>>()
		.join("\n");

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
- Use `chat` for greetings or direct assistant conversation.
- Use `filesystem_read`, `table_read`, `web_lookup`, or `code_exec` when the intent clearly asks for those families even if no tool is available yet.
- Use `multi_step` when the request obviously needs a planning-heavy workflow.
- Use `skill.execute` only when the user is asking to actually run an installed script-backed skill and perform side effects.
- If the user is asking to summarize, explain, describe, list, or quote guidance from an installed skill, do not select `skill.execute`; prefer an advisory route with no execution tool.
- Leave `candidate_tools` empty if no current direct tool is safe.
- `missing_arguments` should be empty unless the user must provide something concrete first.

User goal:
{goal}

Recent history:
{history}

Current inventory:
{candidates}"#,
		goal = request.goal,
		history = if history.is_empty() {
			"(none)"
		} else {
			&history
		},
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
	let primary = stable_primary_tool_match(tool_matches)?;
	let descriptor = context.catalog.descriptor(&primary.descriptor.selector)?;
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
			kind: DirectRouteKind::Inventory,
		})),
		"general.execute" => Some(RouteDecisionResult::Direct(DirectRoutePlan {
			decision,
			kind: DirectRouteKind::Conversation,
		})),
		_ => Some(build_direct_tool_plan(
			context,
			request,
			decision,
			descriptor.selector.clone(),
		)),
	}
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
		"inventory.describe" | "general.execute" => 0.42,
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
	match tool_name.as_str() {
		"fs.inspect" => extract_path_candidates(&request.goal)
			.into_iter()
			.next()
			.map(|path| {
				build_generic_tool_route(
					context,
					request,
					&tool_name,
					decision,
					json!({ "path": path }),
					Vec::new(),
				)
			})
			.unwrap_or_else(|| {
				missing_argument_route(
					IntentFamily::FilesystemRead,
					"filesystem inspect request needs an explicit path",
					"path",
				)
			}),
		"fs.list_dir" => {
			let path = extract_path_candidates(&request.goal)
				.into_iter()
				.next()
				.unwrap_or_else(|| ".".to_string());
			build_generic_tool_route(
				context,
				request,
				&tool_name,
				decision,
				json!({ "path": path }),
				Vec::new(),
			)
		}
		"fs.read_text" => extract_path_candidates(&request.goal)
			.into_iter()
			.next()
			.map(|path| {
				build_generic_tool_route(
					context,
					request,
					&tool_name,
					decision,
					json!({ "path": path, "max_bytes": 4096_u64 }),
					Vec::new(),
				)
			})
			.unwrap_or_else(|| {
				missing_argument_route(
					IntentFamily::FilesystemRead,
					"filesystem read request needs an explicit path",
					"path",
				)
			}),
		"fs.glob" => extract_glob_pattern(&request.goal)
			.map(|pattern| {
				build_generic_tool_route(
					context,
					request,
					&tool_name,
					decision,
					json!({ "pattern": pattern }),
					Vec::new(),
				)
			})
			.unwrap_or_else(|| {
				missing_argument_route(
					IntentFamily::FilesystemRead,
					"filesystem glob request needs an explicit pattern",
					"pattern",
				)
			}),
		"fs.exists" => extract_path_candidates(&request.goal)
			.into_iter()
			.next()
			.map(|path| {
				build_generic_tool_route(
					context,
					request,
					&tool_name,
					decision,
					json!({ "path": path }),
					Vec::new(),
				)
			})
			.unwrap_or_else(|| {
				missing_argument_route(
					IntentFamily::FilesystemRead,
					"filesystem existence check needs an explicit path",
					"path",
				)
			}),
		"table.inspect" | "table.list_sheets" | "table.preview" | "table.schema" => {
			let Some(path) = extract_table_path(&request.goal) else {
				return missing_argument_route(
					IntentFamily::TableRead,
					"table request needs an explicit csv/tsv/xlsx path",
					"path",
				);
			};
			let mut arguments = json!({ "path": path });
			if tool_name == "table.preview" {
				arguments["rows"] = Value::from(extract_row_limit(&request.goal).unwrap_or(5_u64));
			}
			if let Some(sheet) = extract_sheet_name(&request.goal) {
				arguments["sheet"] = Value::String(sheet);
			}
			build_generic_tool_route(
				context,
				request,
				&tool_name,
				decision,
				arguments,
				Vec::new(),
			)
		}
		"web.search" => extract_web_query(&request.goal)
			.map(|query| {
				build_generic_tool_route(
					context,
					request,
					&tool_name,
					decision,
					json!({ "query": query, "top_k": 5_u64 }),
					Vec::new(),
				)
			})
			.unwrap_or_else(|| {
				missing_argument_route(
					IntentFamily::WebLookup,
					"web search request needs a concrete query",
					"query",
				)
			}),
		"python.run" => extract_explicit_python_code(&request.goal)
			.map(|code| {
				let attachments = extract_path_candidates(&request.goal)
					.into_iter()
					.map(PathBuf::from)
					.collect::<Vec<_>>();
				build_generic_tool_route(
					context,
					request,
					&tool_name,
					decision,
					json!({ "code": code }),
					attachments,
				)
			})
			.unwrap_or_else(|| {
				missing_argument_route(
					IntentFamily::CodeExec,
					"python.run only accepts explicit code blocks or inline code in Phase 4",
					"code",
				)
			}),
		_ => build_generic_tool_route(
			context,
			request,
			&tool_name,
			decision,
			json!({}),
			Vec::new(),
		),
	}
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
		kind: if executable {
			DirectRouteKind::SkillExecutable { selector }
		} else {
			DirectRouteKind::SkillAdvisory { selector }
		},
	})
}

fn build_generic_tool_route(
	context: &RouteClassifierContext<'_>,
	_request: &RequestEnvelope,
	tool_name: &str,
	decision: RouteDecision,
	arguments: Value,
	attachments: Vec<PathBuf>,
) -> RouteDecisionResult {
	let Some(selector) = tool_selector(context.catalog, tool_name) else {
		return RouteDecisionResult::Escalate(RouteEscalationPlan {
			decision,
			reason: EscalationReason::NoEnabledRouteTarget,
			action: EscalationAction::FallbackAnswer,
		});
	};
	RouteDecisionResult::Direct(DirectRoutePlan {
		decision,
		kind: DirectRouteKind::ToolInvocation {
			selector,
			arguments,
			attachments,
		},
	})
}

fn missing_argument_route(
	intent_family: IntentFamily,
	reason: impl Into<String>,
	argument: &str,
) -> RouteDecisionResult {
	RouteDecisionResult::Escalate(RouteEscalationPlan {
		decision: RouteDecision::new(
			intent_family,
			0.72,
			false,
			RouteRisk::Low,
			Vec::new(),
			Vec::new(),
			vec![argument.to_string()],
			reason,
		),
		reason: EscalationReason::MissingArguments,
		action: EscalationAction::AskForMoreInfo,
	})
}

fn tool_selector(catalog: &ResourceCatalog, tool_name: &str) -> Option<ResourceSelector> {
	catalog
		.entries()
		.iter()
		.find(|entry| entry.kind == ResourceKind::Tool && entry.name == tool_name)
		.map(|entry| entry.selector.clone())
}

fn role_label(role: ConversationRole) -> &'static str {
	match role {
		ConversationRole::User => "user",
		ConversationRole::Assistant => "assistant",
		ConversationRole::System => "system",
	}
}

fn discoverable_tool_matches(matches: Vec<CatalogMatch>) -> Vec<CatalogMatch> {
	matches
		.into_iter()
		.filter(|entry| entry.descriptor.discoverable)
		.collect()
}

fn extract_skill_source_url(goal: &str) -> Option<String> {
	goal.split_whitespace().find_map(|token| {
		SkillSource::parse(token.trim_matches(|character: char| {
			matches!(
				character,
				'(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | '"' | '\'' | ',' | ';' | '.'
			)
		}))
		.ok()
		.map(|source| source.original_url().to_string())
	})
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
	let mut paths = goal
		.split_whitespace()
		.map(clean_token)
		.filter(|token| looks_like_path_candidate(token))
		.filter(|token| !token.starts_with("http://") && !token.starts_with("https://"))
		.collect::<Vec<_>>();
	paths.dedup();
	paths
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

fn extract_sheet_name(goal: &str) -> Option<String> {
	let lower = goal.to_ascii_lowercase();
	let marker = "sheet ";
	let index = lower.find(marker)?;
	let suffix = goal.get(index + marker.len()..)?.trim();
	let name = suffix
		.trim_matches(|character: char| matches!(character, '"' | '\'' | '`' | ',' | '.' | ';'));
	(!name.is_empty() && !name.contains(' ')).then(|| name.to_string())
}

fn extract_row_limit(goal: &str) -> Option<u64> {
	let digits = goal
		.split_whitespace()
		.find_map(|part| clean_token(part).parse::<u64>().ok())?;
	Some(digits.clamp(1, 50))
}

fn extract_web_query(goal: &str) -> Option<String> {
	let query = goal
		.trim()
		.trim_matches(|character: char| matches!(character, '"' | '\'' | '.' | '!' | '?'));
	(!query.is_empty()).then(|| query.to_string())
}

fn extract_explicit_python_code(goal: &str) -> Option<String> {
	if let Some(code) = extract_fenced_python_code(goal) {
		return Some(code);
	}
	extract_inline_code(goal)
}

fn clean_token(token: &str) -> String {
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
		|| token.rsplit_once('.').is_some_and(|(_, ext)| {
			!ext.is_empty()
				&& ext
					.chars()
					.all(|character| character.is_ascii_alphanumeric())
		})
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
