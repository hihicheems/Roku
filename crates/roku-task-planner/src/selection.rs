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
use roku_llm_adapter::{GenerationRequest, LlmRouter, RiskTier};
use roku_resource_catalog::{CatalogMatch, ResourceCatalog, ResourceKind};
use roku_skill_registry::SkillSource;
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;

const MIN_TOOL_SCORE: f32 = 0.60;
const MIN_SKILL_SCORE: f32 = 0.72;
const HIGH_CONFIDENCE: f32 = 0.78;
const FALLBACK_SKILL_CANDIDATE_SCORE: f32 = 0.25;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SelectionRoute {
	Conversation,
	InstallSkill {
		source_url: String,
		if_missing: bool,
	},
	UseSkillAdvisory {
		selector: ResourceSelector,
	},
	UseSkillExecutable {
		selector: ResourceSelector,
	},
	UseTools {
		selectors: Vec<ResourceSelector>,
	},
	PlannerDefault,
}

#[derive(Clone)]
pub(crate) struct ResourceSelectionEngine {
	catalog: ResourceCatalog,
}

impl ResourceSelectionEngine {
	pub(crate) fn new(catalog: ResourceCatalog) -> Self {
		Self { catalog }
	}

	pub(crate) fn select_without_llm(&self, request: &RequestEnvelope) -> SelectionRoute {
		if let Some(source_url) = extract_skill_source_url(&request.goal) {
			return SelectionRoute::InstallSkill {
				source_url,
				if_missing: true,
			};
		}
		if wants_inventory_overview(&request.goal)
			&& let Some(selector) = inventory_tool_selector(&self.catalog)
		{
			return SelectionRoute::UseTools {
				selectors: vec![selector],
			};
		}

		let query = request.goal.trim();
		let tool_matches =
			discoverable_tool_matches(self.catalog.retrieve(query, Some(ResourceKind::Tool), 4));
		let skill_matches = self.catalog.retrieve(query, Some(ResourceKind::Skill), 4);
		if is_conversation(request, &tool_matches, &skill_matches) {
			return SelectionRoute::Conversation;
		}
		if let Some(selector) = explicit_skill_selector(&self.catalog, &request.goal) {
			return selection_route_for_skill_selector(&self.catalog, selector, &request.goal);
		}
		if let Some(selector) = best_skill_authoring_selector(&self.catalog, &request.goal) {
			return selection_route_for_skill_selector(&self.catalog, selector, &request.goal);
		}
		if let Some(selector) = best_skill_selector(&skill_matches, &tool_matches) {
			return selection_route_for_skill_selector(&self.catalog, selector, &request.goal);
		}
		let selected_tools = best_tool_selectors(&tool_matches);
		if !selected_tools.is_empty() {
			return SelectionRoute::UseTools {
				selectors: selected_tools,
			};
		}

		SelectionRoute::PlannerDefault
	}

	pub(crate) fn select_with_llm(
		&self,
		request: &RequestEnvelope,
		router: &LlmRouter,
	) -> SelectionRoute {
		let deterministic = self.select_without_llm(request);
		if !matches!(deterministic, SelectionRoute::PlannerDefault) {
			return deterministic;
		}

		let candidates = selection_candidates(&self.catalog, request);
		if candidates.is_empty() {
			return SelectionRoute::PlannerDefault;
		}

		let prompt = selection_prompt(request, &candidates);
		let response = match router.generate(&GenerationRequest {
			system_prompt: Some(
				"You are Roku's resource selector. Return only valid JSON and do not explain your answer."
					.to_string(),
			),
			prompt,
			expected_output_tokens: 200,
			risk_tier: RiskTier::Low,
			preferred_provider: None,
			budget_tokens_remaining: 2_000,
			budget_cost_remaining_usd: 0.1,
		}) {
			Ok(response) => response,
			Err(_) => return SelectionRoute::PlannerDefault,
		};
		let Some(choice) = parse_selection_choice(&response.output) else {
			return SelectionRoute::PlannerDefault;
		};
		if choice.confidence < HIGH_CONFIDENCE {
			return SelectionRoute::PlannerDefault;
		}

		match choice.route.as_str() {
			"conversation" => SelectionRoute::Conversation,
			"skill" => choice
				.selectors
				.into_iter()
				.find_map(|selector| parse_selector(&selector))
				.map(|selector| {
					selection_route_for_skill_selector(&self.catalog, selector, &request.goal)
				})
				.unwrap_or(SelectionRoute::PlannerDefault),
			"tools" => {
				let selectors = choice
					.selectors
					.into_iter()
					.filter_map(|selector| parse_selector(&selector))
					.filter(|selector| matches!(selector, ResourceSelector::Tool { .. }))
					.collect::<Vec<_>>();
				if selectors.is_empty() {
					SelectionRoute::PlannerDefault
				} else {
					SelectionRoute::UseTools { selectors }
				}
			}
			_ => SelectionRoute::PlannerDefault,
		}
	}
}

#[derive(Debug, Deserialize)]
struct SelectionChoice {
	route: String,
	#[serde(default)]
	selectors: Vec<String>,
	#[serde(default)]
	confidence: f32,
}

#[derive(Debug, Clone)]
struct SelectionCandidate {
	entry: CatalogMatch,
	source: CandidateSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CandidateSource {
	CurrentGoal,
	RecentUserHistory,
	InstalledSkillCatalog,
}

impl CandidateSource {
	fn as_str(self) -> &'static str {
		match self {
			Self::CurrentGoal => "current_goal",
			Self::RecentUserHistory => "recent_user_history",
			Self::InstalledSkillCatalog => "installed_skill_catalog",
		}
	}
}

fn selection_candidates(
	catalog: &ResourceCatalog,
	request: &RequestEnvelope,
) -> Vec<SelectionCandidate> {
	let current_goal_tool_matches =
		discoverable_tool_matches(catalog.retrieve(&request.goal, Some(ResourceKind::Tool), 4));
	let current_goal_skill_matches = catalog.retrieve(&request.goal, Some(ResourceKind::Skill), 4);
	let mut merged = merge_candidates(
		current_goal_tool_matches
			.into_iter()
			.chain(current_goal_skill_matches.clone())
			.collect(),
		CandidateSource::CurrentGoal,
		HashMap::new(),
	);

	if let Some(history_query) = recent_user_history_query(request) {
		merged = merge_candidates(
			discoverable_tool_matches(catalog.retrieve(
				&history_query,
				Some(ResourceKind::Tool),
				4,
			))
			.into_iter()
			.chain(catalog.retrieve(&history_query, Some(ResourceKind::Skill), 4))
			.collect(),
			CandidateSource::RecentUserHistory,
			merged,
		);
	}
	if should_include_skill_catalog_fallback(request, &current_goal_skill_matches) {
		merged = merge_candidates(
			fallback_skill_candidates(catalog),
			CandidateSource::InstalledSkillCatalog,
			merged,
		);
	}

	let mut ranked = merged.into_values().collect::<Vec<_>>();
	ranked.sort_by(|left, right| right.entry.score.total_cmp(&left.entry.score));
	ranked.truncate(6);
	ranked
}

fn merge_candidates(
	entries: Vec<CatalogMatch>,
	source: CandidateSource,
	mut merged: HashMap<String, SelectionCandidate>,
) -> HashMap<String, SelectionCandidate> {
	for entry in entries {
		let key = entry.descriptor.selector.display_key();
		match merged.get_mut(&key) {
			Some(existing) if entry.score > existing.entry.score => {
				*existing = SelectionCandidate { entry, source };
			}
			Some(_) => {}
			None => {
				merged.insert(key, SelectionCandidate { entry, source });
			}
		}
	}

	merged
}

fn recent_user_history_query(request: &RequestEnvelope) -> Option<String> {
	let history = request
		.conversation_history
		.iter()
		.rev()
		.filter(|turn| turn.role == ConversationRole::User)
		.take(2)
		.map(|turn| turn.content.trim())
		.filter(|content| !content.is_empty())
		.collect::<Vec<_>>();
	if history.is_empty() {
		return None;
	}

	let history = history.into_iter().rev().collect::<Vec<_>>().join("\n");
	Some(format!("{}\n{}", request.goal.trim(), history))
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

fn inventory_tool_selector(catalog: &ResourceCatalog) -> Option<ResourceSelector> {
	catalog
		.entries()
		.iter()
		.find(|entry| {
			entry.kind == ResourceKind::Tool
				&& entry.discoverable
				&& (entry.name.to_ascii_lowercase().contains("inventory")
					|| entry
						.description
						.to_ascii_lowercase()
						.contains("local inventory"))
		})
		.map(|entry| entry.selector.clone())
}

fn should_include_skill_catalog_fallback(
	request: &RequestEnvelope,
	skill_matches: &[CatalogMatch],
) -> bool {
	if wants_inventory_overview(&request.goal) || !has_task_intent(&request.goal) {
		return false;
	}

	skill_matches
		.first()
		.map(|entry| entry.score < HIGH_CONFIDENCE)
		.unwrap_or(true)
}

fn fallback_skill_candidates(catalog: &ResourceCatalog) -> Vec<CatalogMatch> {
	catalog
		.descriptors_for_kind(ResourceKind::Skill)
		.into_iter()
		.map(|descriptor| CatalogMatch {
			descriptor,
			bm25_score: 0.0,
			embedding_score: 0.0,
			score: FALLBACK_SKILL_CANDIDATE_SCORE,
		})
		.collect()
}

fn selection_route_for_skill_selector(
	catalog: &ResourceCatalog,
	selector: ResourceSelector,
	goal: &str,
) -> SelectionRoute {
	let executable = catalog
		.descriptor(&selector)
		.map(skill_descriptor_is_executable)
		.unwrap_or(false);
	if executable && request_wants_skill_execution(goal) {
		SelectionRoute::UseSkillExecutable { selector }
	} else {
		SelectionRoute::UseSkillAdvisory { selector }
	}
}

fn skill_descriptor_is_executable(descriptor: &roku_resource_catalog::CatalogDescriptor) -> bool {
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

fn selection_prompt(request: &RequestEnvelope, candidates: &[SelectionCandidate]) -> String {
	let candidates_json = candidates
		.iter()
		.map(|entry| {
			json!({
				"selector": entry.entry.descriptor.selector.display_key(),
				"kind": format!("{:?}", entry.entry.descriptor.kind),
				"name": entry.entry.descriptor.name,
				"description": entry.entry.descriptor.description,
				"summary": entry.entry.descriptor.summary,
				"examples": entry.entry.descriptor.examples,
				"score": entry.entry.score,
				"source": entry.source.as_str(),
			})
		})
		.collect::<Vec<_>>();
	let history = request
		.conversation_history
		.iter()
		.rev()
		.take(4)
		.map(|turn| format!("{}: {}", role_label(turn.role), turn.content))
		.collect::<Vec<_>>()
		.join("\n");

	format!(
		r#"Return only JSON using this schema:
{{
  "route": "conversation | skill | tools | planner_default",
  "selectors": ["resource selector strings"],
  "confidence": 0.0
}}

Rules:
- Choose `conversation` only for simple chat that should avoid tool/skill routing.
- Choose `skill` only when one installed skill is clearly the best fit.
- Choose `tools` when one or two tools should be used.
- Choose `planner_default` when generic planning should continue without an explicit skill/tool selection.
- Selectors must come from the candidate list exactly.
- Prefer candidates with `"source": "current_goal"`.
- Use `"source": "recent_user_history"` only when the current user turn is clearly a follow-up, rewrite, clarification, or continuation of that earlier user request.
- If the current user turn starts a new topic, ignore stale history candidates and choose `conversation` or `planner_default`.

User goal:
{goal}

Recent history:
{history}

Candidates:
{candidates}"#,
		goal = request.goal,
		history = history,
		candidates = serde_json::to_string_pretty(&candidates_json).unwrap_or_default(),
	)
}

fn parse_selection_choice(output: &str) -> Option<SelectionChoice> {
	let trimmed = output.trim();
	let json_payload = if let Some(stripped) = trimmed.strip_prefix("```") {
		stripped
			.strip_prefix("json")
			.map(str::trim_start)
			.unwrap_or(stripped)
			.strip_suffix("```")
			.map(str::trim)
			.unwrap_or(stripped)
	} else {
		trimmed
	};

	serde_json::from_str(json_payload).ok()
}

fn parse_selector(value: &str) -> Option<ResourceSelector> {
	if let Some(name) = value.strip_prefix("tool:") {
		return Some(ResourceSelector::tool(name.trim()));
	}
	if let Some(name) = value.strip_prefix("skill:") {
		return Some(ResourceSelector::skill(name.trim()));
	}
	None
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
	.any(|pattern| normalized.contains(pattern) || goal.contains(pattern))
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

fn skill_authoring_match_score(descriptor: &roku_resource_catalog::CatalogDescriptor) -> usize {
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
		"运行",
		"执行",
		"安装",
		"修改",
		"更新",
		"修复",
	]
	.iter()
	.any(|pattern| normalized.contains(pattern) || goal.contains(pattern));

	execution_intent || (!advisory_intent && has_task_intent(goal))
}

fn role_label(role: ConversationRole) -> &'static str {
	match role {
		ConversationRole::User => "user",
		ConversationRole::Assistant => "assistant",
		ConversationRole::System => "system",
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use roku_common_types::RequestId;
	use roku_llm_adapter::{
		GenerationRequest, LlmProvider, ModelProfile, ProviderCallError, ProviderResponse,
		RiskTier, RoutingPolicy,
	};
	use roku_resource_catalog::{CatalogDescriptor, ResourceCatalog, ResourceCost, ResourceRisk};
	use std::sync::{Arc, Mutex};

	fn request(goal: &str) -> RequestEnvelope {
		RequestEnvelope {
			request_id: RequestId("req-1".to_string()),
			session_id: "session-1".to_string(),
			goal: goal.to_string(),
			planning_mode_hint: None,
			conversation_history: Vec::new(),
		}
	}

	struct CapturingProvider {
		prompt: Arc<Mutex<Option<String>>>,
		output: &'static str,
	}

	impl LlmProvider for CapturingProvider {
		fn provider_name(&self) -> &'static str {
			"selection-test-provider"
		}

		fn complete(
			&self,
			_model: &ModelProfile,
			request: &GenerationRequest,
		) -> Result<ProviderResponse, ProviderCallError> {
			*self
				.prompt
				.lock()
				.expect("capturing provider prompt lock must not be poisoned") = Some(request.prompt.clone());
			Ok(ProviderResponse {
				output: self.output.to_string(),
				prompt_tokens: 64,
				output_tokens: 32,
				latency_ms: 25,
			})
		}
	}

	fn selection_router(prompt: Arc<Mutex<Option<String>>>, output: &'static str) -> LlmRouter {
		let mut router = LlmRouter::new(RoutingPolicy::default());
		router.register_provider(CapturingProvider { prompt, output });
		router.register_model(ModelProfile {
			model_id: "selection-test-model".to_string(),
			provider: "selection-test-provider".to_string(),
			max_context_tokens: 8_000,
			cost_per_1k_tokens_usd: 0.01,
			max_risk_tier: RiskTier::Critical,
			route_priority: 100,
		});
		router
	}

	fn tool_descriptor(name: &str, description: &str) -> CatalogDescriptor {
		tool_descriptor_with_discoverable(name, description, true)
	}

	fn tool_descriptor_with_discoverable(
		name: &str,
		description: &str,
		discoverable: bool,
	) -> CatalogDescriptor {
		CatalogDescriptor {
			selector: ResourceSelector::tool(name),
			kind: ResourceKind::Tool,
			name: name.to_string(),
			role: None,
			discoverable,
			description: description.to_string(),
			tags: Vec::new(),
			examples: Vec::new(),
			input_schema: Vec::new(),
			risk: ResourceRisk::Low,
			cost: ResourceCost::default(),
			required_capabilities: Vec::new(),
			summary: description.to_string(),
			key_commands: Vec::new(),
			use_cases: Vec::new(),
		}
	}

	fn skill_descriptor(name: &str, description: &str, executable: bool) -> CatalogDescriptor {
		CatalogDescriptor {
			selector: ResourceSelector::skill(name),
			kind: ResourceKind::Skill,
			name: name.to_string(),
			role: None,
			discoverable: true,
			description: description.to_string(),
			tags: if executable {
				vec!["has-scripts".to_string()]
			} else {
				Vec::new()
			},
			examples: Vec::new(),
			input_schema: Vec::new(),
			risk: ResourceRisk::Low,
			cost: ResourceCost::default(),
			required_capabilities: Vec::new(),
			summary: description.to_string(),
			key_commands: Vec::new(),
			use_cases: Vec::new(),
		}
	}

	fn selection_engine() -> ResourceSelectionEngine {
		ResourceSelectionEngine::new(ResourceCatalog::new(vec![
			tool_descriptor(
				"inventory.describe",
				"Describe Roku's local inventory including installed skills, discoverable tools, and capability families, then reformat it for the user.",
			),
			tool_descriptor(
				"research.synthesize",
				"Research a topic and synthesize findings",
			),
			tool_descriptor_with_discoverable(
				"skill.ensure_installed",
				"Install a skill package from a supported source URL",
				false,
			),
		]))
	}

	#[test]
	fn routes_bare_roku_ping_to_conversation() {
		let selection = selection_engine().select_without_llm(&request("roku"));
		assert_eq!(selection, SelectionRoute::Conversation);
	}

	#[test]
	fn routes_greeting_with_roku_to_conversation() {
		let selection = selection_engine().select_without_llm(&request("你好 roku"));
		assert_eq!(selection, SelectionRoute::Conversation);
	}

	#[test]
	fn selects_inventory_tool_for_inventory_queries() {
		let selection =
			selection_engine().select_without_llm(&request("现在有哪些 tool 和 capability？"));
		assert_eq!(
			selection,
			SelectionRoute::UseTools {
				selectors: vec![ResourceSelector::tool("inventory.describe")],
			}
		);
	}

	#[test]
	fn explicit_script_backed_skill_routes_to_executable_skill() {
		let engine = ResourceSelectionEngine::new(ResourceCatalog::new(vec![skill_descriptor(
			"skill-creator",
			"Create and iterate on local skills",
			true,
		)]));

		let selection =
			engine.select_without_llm(&request("Use skill-creator to create a Python skill"));

		assert_eq!(
			selection,
			SelectionRoute::UseSkillExecutable {
				selector: ResourceSelector::skill("skill-creator"),
			}
		);
	}

	#[test]
	fn selects_inventory_tool_for_format_followup() {
		let mut request = request("用无序列表列一下");
		request.conversation_history = vec![
			roku_common_types::ConversationTurn {
				role: ConversationRole::User,
				content: "列出 skill、tool".to_string(),
				created_at_unix_ms: 0,
			},
			roku_common_types::ConversationTurn {
				role: ConversationRole::Assistant,
				content: "已安装的 skills: xlsx。可用工具: data.execute。能力类别: data.read."
					.to_string(),
				created_at_unix_ms: 0,
			},
		];
		let selection = selection_engine().select_without_llm(&request);
		assert_eq!(selection, SelectionRoute::PlannerDefault);
	}

	#[test]
	fn llm_selector_receives_recent_user_history_as_secondary_candidates() {
		let mut request = request("用无序列表列一下");
		request.conversation_history = vec![
			roku_common_types::ConversationTurn {
				role: ConversationRole::User,
				content: "列出 skill、tool".to_string(),
				created_at_unix_ms: 0,
			},
			roku_common_types::ConversationTurn {
				role: ConversationRole::Assistant,
				content: "已安装的 skills: xlsx。可用工具: inventory.describe.".to_string(),
				created_at_unix_ms: 0,
			},
		];
		let prompt = Arc::new(Mutex::new(None));
		let router = selection_router(
			prompt.clone(),
			r#"{"route":"tools","selectors":["tool:inventory.describe"],"confidence":0.95}"#,
		);

		let selection = selection_engine().select_with_llm(&request, &router);

		assert_eq!(
			selection,
			SelectionRoute::UseTools {
				selectors: vec![ResourceSelector::tool("inventory.describe")],
			}
		);
		let prompt = prompt
			.lock()
			.expect("captured prompt lock must not be poisoned")
			.clone()
			.expect("selection prompt should be captured");
		assert!(prompt.contains(r#""source": "recent_user_history""#));
		assert!(prompt.contains("列出 skill、tool"));
	}

	#[test]
	fn keeps_explicit_skill_url_install_routing() {
		let selection = selection_engine().select_without_llm(&request(
			"install https://github.com/anthropics/skills/tree/main/skills/claude-api",
		));
		assert_eq!(
			selection,
			SelectionRoute::InstallSkill {
				source_url: "https://github.com/anthropics/skills/tree/main/skills/claude-api"
					.to_string(),
				if_missing: true,
			}
		);
	}
}
