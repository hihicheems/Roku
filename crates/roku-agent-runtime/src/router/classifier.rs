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

use roku_common_types::{ConversationRole, RequestEnvelope, ResourceSelector};
use roku_plugin_catalog::{CatalogDescriptor, CatalogMatch, ResourceCatalog, ResourceKind};
use roku_plugin_core::PluginRegistrySnapshot;
use roku_plugin_llm::{GenerationRequest, LlmRouter, RiskTier, StructuredGenerationError};
use roku_plugin_skills::SkillSource;
use serde_json::json;

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

	if wants_inventory_overview(&request.goal) {
		let decision = RouteDecision::new(
			IntentFamily::Chat,
			0.96,
			false,
			RouteRisk::Low,
			vec![tool_name_for_role(
				context.tool_config,
				BuiltinToolRole::Inventory,
			)],
			vec![
				"builtin-tools".to_string(),
				"skill-source-local".to_string(),
			],
			Vec::new(),
			"inventory overview request matches stable direct inventory route",
		);
		return Some(RouteDecisionResult::Direct(DirectRoutePlan {
			decision,
			kind: DirectRouteKind::Inventory,
		}));
	}

	let query = request.goal.trim();
	let tool_matches =
		discoverable_tool_matches(context.catalog.retrieve(query, Some(ResourceKind::Tool), 4));
	let skill_matches = context
		.catalog
		.retrieve(query, Some(ResourceKind::Skill), 4);
	if is_conversation(request, &tool_matches, &skill_matches) {
		let decision = RouteDecision::new(
			IntentFamily::Chat,
			0.92,
			false,
			RouteRisk::Low,
			vec![tool_name_for_role(
				context.tool_config,
				BuiltinToolRole::General,
			)],
			candidate_plugins_for_tool(context.plugin_snapshot, "general.execute"),
			Vec::new(),
			"conversation request should bypass planning and external tool routing",
		);
		return Some(RouteDecisionResult::Direct(DirectRoutePlan {
			decision,
			kind: DirectRouteKind::Conversation,
		}));
	}

	if let Some(selector) = explicit_skill_selector(context.catalog, &request.goal)
		.or_else(|| best_skill_authoring_selector(context.catalog, &request.goal))
		.or_else(|| best_skill_selector(&skill_matches, &tool_matches))
	{
		let descriptor = context.catalog.descriptor(&selector)?;
		let executable = skill_descriptor_is_executable(descriptor)
			&& request_wants_skill_execution(&request.goal);
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
			format!(
				"installed skill `{}` matched deterministic classifier",
				selector.name()
			),
		);
		return Some(RouteDecisionResult::Direct(DirectRoutePlan {
			decision,
			kind: if executable {
				DirectRouteKind::SkillExecutable { selector }
			} else {
				DirectRouteKind::SkillAdvisory { selector }
			},
		}));
	}

	if let Some((intent_family, missing_arguments, reason)) = classify_unsupported_family(query) {
		let action = if !missing_arguments.is_empty() {
			EscalationAction::AskForMoreInfo
		} else {
			EscalationAction::FallbackAnswer
		};
		let decision = RouteDecision::new(
			intent_family,
			0.74,
			false,
			RouteRisk::Low,
			Vec::new(),
			Vec::new(),
			missing_arguments,
			reason,
		);
		return Some(RouteDecisionResult::Escalate(RouteEscalationPlan {
			decision,
			reason: if action == EscalationAction::AskForMoreInfo {
				EscalationReason::MissingArguments
			} else {
				EscalationReason::NoEnabledRouteTarget
			},
			action,
		}));
	}

	let selected_tools = best_tool_selectors(&tool_matches);
	if selected_tools.len() == 1 {
		let selector = selected_tools.into_iter().next()?;
		let descriptor = context.catalog.descriptor(&selector)?;
		let decision = RouteDecision::new(
			IntentFamily::TextTransform,
			0.89,
			false,
			resource_risk(descriptor),
			vec![descriptor.name.clone()],
			candidate_plugins_for_tool(context.plugin_snapshot, &descriptor.name),
			Vec::new(),
			format!(
				"single discoverable tool `{}` matched deterministically",
				selector.name()
			),
		);
		return Some(RouteDecisionResult::Direct(DirectRoutePlan {
			decision,
			kind: DirectRouteKind::Tool { selector },
		}));
	}

	if selected_tools.len() > 1 || looks_multi_step(&request.goal) {
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

	None
}

fn classify_with_llm(
	context: &RouteClassifierContext<'_>,
	request: &RequestEnvelope,
	router: &LlmRouter,
) -> RouteDecisionResult {
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
	if decision.intent_family == IntentFamily::Chat {
		return RouteDecisionResult::Direct(DirectRoutePlan {
			decision,
			kind: DirectRouteKind::Conversation,
		});
	}
	if let Some(selector) = select_tool_from_candidates(context.catalog, &decision.candidate_tools)
	{
		return RouteDecisionResult::Direct(DirectRoutePlan {
			decision,
			kind: DirectRouteKind::Tool { selector },
		});
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
	let normalized_goal = normalize(goal);
	let mut entries = catalog
		.entries()
		.iter()
		.filter(|entry| entry.kind == ResourceKind::Skill)
		.collect::<Vec<_>>();
	entries.sort_by(|left, right| right.name.len().cmp(&left.name.len()));
	entries.into_iter().find_map(|entry| {
		let normalized_name = normalize(&entry.name);
		(normalized_name.len() > 2 && normalized_goal.contains(&normalized_name))
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

fn best_skill_authoring_selector(
	catalog: &ResourceCatalog,
	goal: &str,
) -> Option<ResourceSelector> {
	if !request_targets_skill_authoring(goal) {
		return None;
	}

	catalog
		.descriptors_for_kind(ResourceKind::Skill)
		.into_iter()
		.filter_map(|descriptor| {
			let score = skill_authoring_match_score(&descriptor);
			(score > 0).then_some((score, descriptor.name.clone(), descriptor.selector))
		})
		.max_by(|left, right| left.0.cmp(&right.0).then_with(|| right.1.cmp(&left.1)))
		.map(|(_, _, selector)| selector)
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

fn is_conversation(
	request: &RequestEnvelope,
	tool_matches: &[CatalogMatch],
	skill_matches: &[CatalogMatch],
) -> bool {
	if is_direct_assistant_ping(&request.goal) {
		return true;
	}

	let top_score = tool_matches
		.first()
		.map(|entry| entry.score)
		.into_iter()
		.chain(skill_matches.first().map(|entry| entry.score))
		.fold(0.0_f32, f32::max);
	let goal = request.goal.trim();
	let normalized = goal.to_ascii_lowercase();
	let chatty = [
		"你好",
		"嗨",
		"hey",
		"hi",
		"hello",
		"thanks",
		"谢谢",
		"在吗",
		"你是谁",
		"介绍一下你自己",
	]
	.iter()
	.any(|pattern| normalized.contains(pattern) || goal.contains(pattern));

	chatty && (!has_task_intent(goal) || top_score < MIN_TOOL_SCORE)
}

fn normalize(value: &str) -> String {
	value
		.chars()
		.filter(|character| character.is_ascii_alphanumeric())
		.collect::<String>()
		.to_ascii_lowercase()
}

fn is_direct_assistant_ping(goal: &str) -> bool {
	let trimmed = goal.trim();
	if trimmed.is_empty() {
		return true;
	}
	let normalized = normalize(trimmed);
	if normalized.is_empty() {
		return false;
	}
	if normalized == "roku" {
		return true;
	}
	let mentions_roku = normalized.contains("roku");
	if !mentions_roku {
		return false;
	}
	let compact = trimmed
		.chars()
		.filter(|character| !character.is_whitespace() && !character.is_ascii_punctuation())
		.collect::<String>()
		.to_ascii_lowercase();
	if compact == "roku" {
		return true;
	}
	let token_count = trimmed.split_whitespace().count();
	token_count <= 4 && !has_task_intent(trimmed)
}

fn has_task_intent(goal: &str) -> bool {
	let normalized = goal.to_ascii_lowercase();
	[
		"install",
		"setup",
		"use ",
		"create",
		"build",
		"generate",
		"analyze",
		"analyse",
		"review",
		"search",
		"find",
		"summarize",
		"debug",
		"fix",
		"帮我",
		"请帮",
		"安装",
		"装一个",
		"使用",
		"创建",
		"新建",
		"生成",
		"分析",
		"总结",
		"检索",
		"搜索",
		"修复",
		"排查",
	]
	.iter()
	.any(|pattern| normalized.contains(pattern) || goal.contains(pattern))
}

fn wants_inventory_overview(goal: &str) -> bool {
	let normalized = goal.to_ascii_lowercase();
	let compact = goal
		.chars()
		.filter(|character| !character.is_whitespace())
		.collect::<String>();
	[
		"what skills",
		"which skills",
		"list skills",
		"available skills",
		"inventory",
		"你现在有啥skill",
		"你有哪些skill",
		"有什么skill",
		"列出skill",
		"有哪些技能",
		"技能列表",
		"库存",
	]
	.iter()
	.any(|pattern| {
		normalized.contains(pattern) || goal.contains(pattern) || compact.contains(pattern)
	})
}

fn request_targets_skill_authoring(goal: &str) -> bool {
	let normalized = goal.to_ascii_lowercase();
	let mentions_skill =
		normalized.contains("skill") || goal.contains("技能") || goal.contains("技能力");
	let authoring_intent = [
		"create",
		"build",
		"generate",
		"modify",
		"update",
		"improve",
		"optimize",
		"benchmark",
		"eval",
		"创建",
		"新建",
		"生成",
		"制作",
		"修改",
		"更新",
		"改进",
		"优化",
		"评估",
	]
	.iter()
	.any(|pattern| normalized.contains(pattern) || goal.contains(pattern));

	mentions_skill && authoring_intent
}

fn skill_authoring_match_score(descriptor: &CatalogDescriptor) -> usize {
	let text = descriptor.searchable_text().to_ascii_lowercase();
	let mut score = 0usize;
	if text.contains("skill") {
		score += 1;
	}
	for pattern in [
		"skill creator",
		"create new skills",
		"create a skill",
		"creating new skills",
		"new skill",
		"existing skill",
		"modify and improve existing skills",
		"edit a skill",
		"optimize a skill",
		"benchmark skill",
		"evals",
		"run evals",
	] {
		if text.contains(pattern) {
			score += 3;
		}
	}
	for pattern in [
		"create",
		"modify",
		"improve",
		"optimize",
		"benchmark",
		"eval",
	] {
		if text.contains(pattern) {
			score += 1;
		}
	}
	score
}

fn request_wants_skill_execution(goal: &str) -> bool {
	let normalized = goal.to_ascii_lowercase();
	let advisory_intent = [
		"summarize",
		"summary",
		"explain",
		"describe",
		"what is",
		"how does",
		"overview",
		"list",
		"show me",
		"总结",
		"概括",
		"解释",
		"说明",
		"介绍",
		"是什么",
		"怎么用",
		"有哪些",
		"列出",
	]
	.iter()
	.any(|pattern| normalized.contains(pattern) || goal.contains(pattern));
	let execution_intent = [
		"create",
		"build",
		"generate",
		"make",
		"write",
		"run",
		"execute",
		"install",
		"modify",
		"update",
		"fix",
		"帮我",
		"请帮",
		"创建",
		"新建",
		"生成",
		"制作",
		"写一个",
		"执行",
		"运行",
		"安装",
		"修改",
		"更新",
		"修复",
	]
	.iter()
	.any(|pattern| normalized.contains(pattern) || goal.contains(pattern));

	execution_intent || !advisory_intent
}

fn classify_unsupported_family(goal: &str) -> Option<(IntentFamily, Vec<String>, String)> {
	let normalized = goal.to_ascii_lowercase();
	if normalized.contains("list the files")
		|| normalized.contains("read file")
		|| normalized.contains("open file")
		|| goal.contains("列出文件")
		|| goal.contains("读取文件")
	{
		let missing = if normalized.contains("current directory")
			|| goal.contains("当前目录")
			|| goal.contains("这个目录")
			|| goal.contains("本目录")
		{
			Vec::new()
		} else {
			vec!["path".to_string()]
		};
		return Some((
			IntentFamily::FilesystemRead,
			missing,
			"request targets filesystem reading, but Phase 3 does not expose fs.* tools yet"
				.to_string(),
		));
	}
	if normalized.contains("csv")
		|| normalized.contains("table")
		|| goal.contains("表格")
		|| goal.contains("csv")
	{
		return Some((
			IntentFamily::TableRead,
			Vec::new(),
			"request targets table reading, but Phase 3 does not expose table.* tools yet"
				.to_string(),
		));
	}
	if normalized.contains("search the web")
		|| normalized.contains("browse")
		|| goal.contains("上网")
		|| goal.contains("网页搜索")
	{
		return Some((
			IntentFamily::WebLookup,
			Vec::new(),
			"request targets web lookup, but Phase 3 does not expose web.search yet".to_string(),
		));
	}
	if normalized.contains("run python")
		|| normalized.contains("execute python")
		|| goal.contains("运行 python")
		|| goal.contains("执行 python")
	{
		return Some((
			IntentFamily::CodeExec,
			Vec::new(),
			"request targets code execution, but Phase 3 does not expose python.run yet"
				.to_string(),
		));
	}
	None
}

fn looks_multi_step(goal: &str) -> bool {
	let normalized = goal.to_ascii_lowercase();
	[
		"and then",
		"step by step",
		"multi-step",
		"workflow",
		"compare",
		"analyze",
		"analyse",
		"research",
		"部署",
		"分析",
		"比较",
		"多步骤",
		"工作流",
	]
	.iter()
	.any(|pattern| normalized.contains(pattern) || goal.contains(pattern))
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
	if plugin_snapshot.is_plugin_enabled("builtin-tools") {
		plugins.push("builtin-tools".to_string());
	}
	if tool_name.starts_with("skill.") && plugin_snapshot.is_plugin_enabled("skill-source-local") {
		plugins.push("skill-source-local".to_string());
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
