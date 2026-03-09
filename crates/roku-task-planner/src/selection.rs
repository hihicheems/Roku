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

const MIN_TOOL_SCORE: f32 = 0.60;
const MIN_SKILL_SCORE: f32 = 0.72;
const HIGH_CONFIDENCE: f32 = 0.78;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SelectionRoute {
	Conversation,
	InstallSkill {
		source_url: String,
		if_missing: bool,
	},
	UseSkill {
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
		let query = selection_query(request);
		if let Some(source_url) = extract_skill_source_url(&request.goal) {
			return SelectionRoute::InstallSkill {
				source_url,
				if_missing: true,
			};
		}

		let tool_matches = self.catalog.retrieve(&query, Some(ResourceKind::Tool), 4);
		let skill_matches = self.catalog.retrieve(&query, Some(ResourceKind::Skill), 4);
		if is_conversation(request, &tool_matches, &skill_matches) {
			return SelectionRoute::Conversation;
		}
		if let Some(selector) = explicit_skill_selector(&request.goal, &skill_matches) {
			return SelectionRoute::UseSkill { selector };
		}
		if let Some(selector) = best_skill_selector(&skill_matches, &tool_matches) {
			return SelectionRoute::UseSkill { selector };
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

		let query = selection_query(request);
		let tool_matches = self.catalog.retrieve(&query, Some(ResourceKind::Tool), 4);
		let skill_matches = self.catalog.retrieve(&query, Some(ResourceKind::Skill), 4);
		let candidates = tool_matches
			.iter()
			.chain(skill_matches.iter())
			.take(6)
			.collect::<Vec<_>>();
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
				.map(|selector| SelectionRoute::UseSkill { selector })
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

fn selection_query(request: &RequestEnvelope) -> String {
	let history = request
		.conversation_history
		.iter()
		.rev()
		.take(4)
		.map(|turn| format!("{}: {}", role_label(turn.role), turn.content))
		.collect::<Vec<_>>()
		.join("\n");
	if history.is_empty() {
		request.goal.clone()
	} else {
		format!("{}\n{}", request.goal, history)
	}
}

fn explicit_skill_selector(goal: &str, matches: &[CatalogMatch]) -> Option<ResourceSelector> {
	let normalized_goal = normalize(goal);
	matches.iter().find_map(|entry| {
		let normalized_name = normalize(&entry.descriptor.name);
		(normalized_name.len() > 2 && normalized_goal.contains(&normalized_name))
			.then(|| entry.descriptor.selector.clone())
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

fn is_conversation(
	request: &RequestEnvelope,
	tool_matches: &[CatalogMatch],
	skill_matches: &[CatalogMatch],
) -> bool {
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

	chatty && top_score < MIN_TOOL_SCORE
}

fn selection_prompt(request: &RequestEnvelope, candidates: &[&CatalogMatch]) -> String {
	let candidates_json = candidates
		.iter()
		.map(|entry| {
			json!({
				"selector": entry.descriptor.selector.display_key(),
				"kind": format!("{:?}", entry.descriptor.kind),
				"name": entry.descriptor.name,
				"description": entry.descriptor.description,
				"summary": entry.descriptor.summary,
				"examples": entry.descriptor.examples,
				"score": entry.score,
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

fn role_label(role: ConversationRole) -> &'static str {
	match role {
		ConversationRole::User => "user",
		ConversationRole::Assistant => "assistant",
		ConversationRole::System => "system",
	}
}
