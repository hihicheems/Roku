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

use roku_common_types::{ConversationRole, ConversationTurn, RequestEnvelope};
use roku_llm_adapter::{GenerationRequest, LlmRouter, RiskTier};
use roku_skill_registry::{SkillRegistry, SkillSource};
use serde::Deserialize;
use serde_json::json;

const CLASSIFIER_MAX_HISTORY_TURNS: usize = 4;
const CLASSIFIER_MAX_TURN_CHARS: usize = 240;
const CLASSIFIER_EXPECTED_OUTPUT_TOKENS: u64 = 96;
const CLASSIFIER_BUDGET_TOKENS: u64 = 2_000;
const CLASSIFIER_BUDGET_COST_USD: f64 = 0.10;
const HIGH_CONFIDENCE_THRESHOLD: f32 = 0.78;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ShortcutIntent {
	EnsureSkillInstalled {
		source_url: String,
		if_missing: bool,
	},
	UseInstalledSkill {
		skill_name: String,
	},
}

#[derive(Clone)]
pub(crate) struct SkillShortcutResolver {
	skill_registry: SkillRegistry,
}

impl SkillShortcutResolver {
	pub(crate) fn new(skill_registry: SkillRegistry) -> Self {
		Self { skill_registry }
	}

	pub(crate) fn resolve_without_classifier(
		&self,
		request: &RequestEnvelope,
	) -> Option<ShortcutIntent> {
		let anchors = self.extract_anchors(request);
		deterministic_shortcut(&anchors)
	}

	pub(crate) fn resolve_with_classifier(
		&self,
		request: &RequestEnvelope,
		router: &LlmRouter,
	) -> Option<ShortcutIntent> {
		let anchors = self.extract_anchors(request);
		if let Some(intent) = deterministic_shortcut(&anchors) {
			return Some(intent);
		}
		if !should_attempt_classification(request, &anchors) {
			return None;
		}

		let classification = self.classify_intent(request, &anchors, router)?;
		merge_shortcut_policy(&self.skill_registry, &anchors, &classification)
	}

	fn extract_anchors(&self, request: &RequestEnvelope) -> SkillRequestAnchors {
		let mut available_installed_skill_names = self
			.skill_registry
			.list_skills()
			.map(|records| {
				records
					.into_iter()
					.map(|record| record.descriptor.name)
					.collect::<Vec<_>>()
			})
			.unwrap_or_default();
		available_installed_skill_names.sort();
		available_installed_skill_names.dedup();

		let matched_installed_skill_names = self
			.skill_registry
			.referenced_skill_names(&request.goal)
			.unwrap_or_default();
		let valid_skill_source_urls = extract_valid_skill_source_urls(&request.goal);
		let explicit_command = detect_explicit_command(
			&request.goal,
			&valid_skill_source_urls,
			&matched_installed_skill_names,
			&available_installed_skill_names,
		);

		SkillRequestAnchors {
			valid_skill_source_urls,
			matched_installed_skill_names,
			available_installed_skill_names,
			explicit_command,
		}
	}

	fn classify_intent(
		&self,
		request: &RequestEnvelope,
		anchors: &SkillRequestAnchors,
		router: &LlmRouter,
	) -> Option<StructuredSkillIntent> {
		let prompt = classification_prompt(request, anchors);
		let response = router
			.generate(&GenerationRequest {
				system_prompt: Some(
					"You are Roku's shortcut intent classifier. Return only one line of valid JSON. Do not explain your answer. Do not emit reasoning."
						.to_string(),
				),
				prompt,
				expected_output_tokens: CLASSIFIER_EXPECTED_OUTPUT_TOKENS,
				risk_tier: RiskTier::Low,
				preferred_provider: None,
				budget_tokens_remaining: CLASSIFIER_BUDGET_TOKENS,
				budget_cost_remaining_usd: CLASSIFIER_BUDGET_COST_USD,
			})
			.ok()?;

		parse_structured_intent(&response.output)
	}

	#[cfg(test)]
	pub(crate) fn resolve_installed_skill_name(&self, candidate: &str) -> Option<String> {
		self.skill_registry
			.resolve_installed_skill_name(candidate)
			.ok()
			.flatten()
	}

	#[cfg(test)]
	pub(crate) fn referenced_skill_names(&self, query: &str) -> Vec<String> {
		self.skill_registry
			.referenced_skill_names(query)
			.unwrap_or_default()
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SkillRequestAnchors {
	valid_skill_source_urls: Vec<String>,
	matched_installed_skill_names: Vec<String>,
	available_installed_skill_names: Vec<String>,
	explicit_command: Option<SkillCommandAnchor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SkillCommandAnchor {
	EnsureInstalled,
	UseInstalledSkill,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum StructuredIntentLabel {
	EnsureSkillInstalled,
	UseInstalledSkill,
	None,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
struct StructuredSkillIntent {
	intent: StructuredIntentLabel,
	#[serde(default)]
	source_url: Option<String>,
	#[serde(default)]
	skill_name: Option<String>,
	#[serde(default)]
	if_missing: bool,
	#[serde(default)]
	confidence: f32,
}

fn deterministic_shortcut(anchors: &SkillRequestAnchors) -> Option<ShortcutIntent> {
	if let Some(source_url) = single_skill_source_url(anchors) {
		return Some(ShortcutIntent::EnsureSkillInstalled {
			source_url,
			if_missing: true,
		});
	}

	if anchors.explicit_command == Some(SkillCommandAnchor::UseInstalledSkill)
		&& anchors.matched_installed_skill_names.len() == 1
	{
		return Some(ShortcutIntent::UseInstalledSkill {
			skill_name: anchors.matched_installed_skill_names[0].clone(),
		});
	}

	None
}

fn merge_shortcut_policy(
	skill_registry: &SkillRegistry,
	anchors: &SkillRequestAnchors,
	classification: &StructuredSkillIntent,
) -> Option<ShortcutIntent> {
	if let Some(source_url) = single_skill_source_url(anchors) {
		let if_missing = if classification.intent == StructuredIntentLabel::EnsureSkillInstalled
			&& classification.confidence >= HIGH_CONFIDENCE_THRESHOLD
		{
			classification.if_missing
		} else {
			true
		};
		return Some(ShortcutIntent::EnsureSkillInstalled {
			source_url,
			if_missing,
		});
	}

	let matched_skill_name = if anchors.matched_installed_skill_names.len() == 1 {
		Some(anchors.matched_installed_skill_names[0].clone())
	} else {
		None
	};
	let classified_skill_name = classification.skill_name.as_deref().and_then(|candidate| {
		skill_registry
			.resolve_installed_skill_name(candidate)
			.ok()
			.flatten()
	});

	if classification.intent == StructuredIntentLabel::UseInstalledSkill
		&& classification.confidence >= HIGH_CONFIDENCE_THRESHOLD
	{
		return classified_skill_name
			.or(matched_skill_name)
			.map(|skill_name| ShortcutIntent::UseInstalledSkill { skill_name });
	}

	None
}

fn should_attempt_classification(request: &RequestEnvelope, anchors: &SkillRequestAnchors) -> bool {
	!anchors.valid_skill_source_urls.is_empty()
		|| !anchors.matched_installed_skill_names.is_empty()
		|| anchors.explicit_command.is_some()
		|| request.goal.to_ascii_lowercase().contains("skill")
		|| request.goal.contains("技能")
}

fn classification_prompt(request: &RequestEnvelope, anchors: &SkillRequestAnchors) -> String {
	let available_installed_skill_names = anchors
		.available_installed_skill_names
		.iter()
		.take(32)
		.cloned()
		.collect::<Vec<_>>();
	let anchors_json = json!({
		"valid_skill_source_urls": anchors.valid_skill_source_urls,
		"matched_installed_skill_names": anchors.matched_installed_skill_names,
		"available_installed_skill_names": available_installed_skill_names,
		"explicit_command": anchors.explicit_command.map(command_anchor_label),
	});

	format!(
		r#"Return one-line JSON only:
{{
  "intent": "ensure_skill_installed | use_installed_skill | none",
  "source_url": "string or null",
  "skill_name": "string or null",
  "if_missing": true,
  "confidence": 0.0
}}

Rules:
- Only classify the shortcut intent. Do not produce a plan.
- `source_url` must be one of `valid_skill_source_urls` or null.
- `skill_name` must be one of `available_installed_skill_names` or null.
- If unsure, return `none` with low confidence.

User request:
{goal}

Conversation history:
{history}

Anchors:
{anchors_json}"#,
		goal = request.goal,
		history = render_recent_history(&request.conversation_history),
		anchors_json = anchors_json,
	)
}

fn command_anchor_label(anchor: SkillCommandAnchor) -> &'static str {
	match anchor {
		SkillCommandAnchor::EnsureInstalled => "ensure_installed",
		SkillCommandAnchor::UseInstalledSkill => "use_installed_skill",
	}
}

fn render_recent_history(history: &[ConversationTurn]) -> String {
	if history.is_empty() {
		return "none".to_string();
	}

	history
		.iter()
		.rev()
		.take(CLASSIFIER_MAX_HISTORY_TURNS)
		.collect::<Vec<_>>()
		.into_iter()
		.rev()
		.map(|turn| {
			format!(
				"{}: {}",
				conversation_role_label(turn.role),
				truncate_text(&turn.content, CLASSIFIER_MAX_TURN_CHARS)
			)
		})
		.collect::<Vec<_>>()
		.join("\n")
}

fn conversation_role_label(role: ConversationRole) -> &'static str {
	match role {
		ConversationRole::User => "user",
		ConversationRole::Assistant => "assistant",
		ConversationRole::System => "system",
	}
}

fn truncate_text(value: &str, max_chars: usize) -> String {
	let mut chars = value.chars();
	let truncated = chars.by_ref().take(max_chars).collect::<String>();
	if chars.next().is_some() {
		format!("{truncated}...")
	} else {
		truncated
	}
}

fn parse_structured_intent(payload: &str) -> Option<StructuredSkillIntent> {
	let mut parsed =
		serde_json::from_str::<StructuredSkillIntent>(&extract_json_payload(payload)).ok()?;
	parsed.confidence = parsed.confidence.clamp(0.0, 1.0);
	parsed.source_url = parsed
		.source_url
		.map(|value| value.trim().to_string())
		.filter(|value| !value.is_empty());
	parsed.skill_name = parsed
		.skill_name
		.map(|value| value.trim().to_string())
		.filter(|value| !value.is_empty());
	Some(parsed)
}

fn extract_json_payload(payload: &str) -> String {
	let trimmed = payload.trim();
	if let Some(stripped) = trimmed.strip_prefix("```") {
		let without_language = stripped
			.strip_prefix("json")
			.map(str::trim_start)
			.unwrap_or(stripped);
		return without_language
			.strip_suffix("```")
			.map(str::trim)
			.unwrap_or(without_language)
			.to_string();
	}

	trimmed.to_string()
}

fn detect_explicit_command(
	goal: &str,
	valid_skill_source_urls: &[String],
	matched_installed_skill_names: &[String],
	available_installed_skill_names: &[String],
) -> Option<SkillCommandAnchor> {
	let trimmed = goal.trim();
	let normalized = trimmed.to_ascii_lowercase();
	let mut tokens = normalized.split_whitespace();
	let first = tokens.next()?;
	let second = tokens.next();

	let is_skill_command = matches!(first, "/skill" | "skill" | "/skills" | "skills");
	if is_skill_command {
		if !valid_skill_source_urls.is_empty() {
			return Some(SkillCommandAnchor::EnsureInstalled);
		}
		if matches!(second, Some("install" | "ensure" | "add"))
			&& !valid_skill_source_urls.is_empty()
		{
			return Some(SkillCommandAnchor::EnsureInstalled);
		}
		if matches!(second, Some("use" | "run" | "apply"))
			&& command_skill_target(trimmed, available_installed_skill_names)
				.or_else(|| matched_installed_skill_names.first().cloned())
				.is_some()
		{
			return Some(SkillCommandAnchor::UseInstalledSkill);
		}
	}

	if matches!(first, "/use-skill" | "use-skill")
		&& command_skill_target(trimmed, available_installed_skill_names)
			.or_else(|| matched_installed_skill_names.first().cloned())
			.is_some()
	{
		return Some(SkillCommandAnchor::UseInstalledSkill);
	}

	None
}

fn command_skill_target(goal: &str, available_installed_skill_names: &[String]) -> Option<String> {
	let candidate = goal
		.split_whitespace()
		.last()
		.map(trim_command_token)
		.filter(|value| !value.is_empty())?;

	available_installed_skill_names
		.iter()
		.find_map(|name| (normalize_text(name) == normalize_text(candidate)).then(|| name.clone()))
}

fn trim_command_token(token: &str) -> &str {
	token.trim_matches(|character: char| {
		matches!(
			character,
			'(' | ')'
				| '[' | ']' | '{'
				| '}' | '<' | '>'
				| '"' | '\'' | ','
				| ';' | '.' | '!'
				| '?'
		)
	})
}

fn extract_valid_skill_source_urls(goal: &str) -> Vec<String> {
	let mut urls = goal
		.split_whitespace()
		.map(trim_command_token)
		.filter(|token| !token.is_empty())
		.filter_map(|token| {
			SkillSource::parse(token)
				.ok()
				.map(|source| source.original_url().to_string())
		})
		.collect::<Vec<_>>();
	urls.sort();
	urls.dedup();
	urls
}

fn single_skill_source_url(anchors: &SkillRequestAnchors) -> Option<String> {
	match anchors.valid_skill_source_urls.as_slice() {
		[source_url] => Some(source_url.clone()),
		_ => None,
	}
}

fn normalize_text(value: &str) -> String {
	value
		.chars()
		.map(|character| {
			if character.is_ascii_alphanumeric() {
				character.to_ascii_lowercase()
			} else {
				' '
			}
		})
		.collect::<String>()
		.split_whitespace()
		.collect::<Vec<_>>()
		.join(" ")
}

#[cfg(test)]
mod tests {
	use std::io::{Cursor, Write};
	use std::sync::Arc;

	use roku_skill_registry::{DownloadedArchive, SkillArchiveFetcher, SkillRegistryError};

	use super::*;

	#[derive(Clone)]
	struct StaticArchiveFetcher {
		archive: DownloadedArchive,
	}

	impl SkillArchiveFetcher for StaticArchiveFetcher {
		fn fetch(&self, _source: &SkillSource) -> Result<DownloadedArchive, SkillRegistryError> {
			Ok(self.archive.clone())
		}
	}

	#[test]
	fn deterministic_shortcut_prefers_single_skill_url() {
		let resolver = SkillShortcutResolver::new(SkillRegistry::disabled());
		let request = RequestEnvelope {
			request_id: roku_common_types::RequestId("req-1".to_string()),
			session_id: "s1".to_string(),
			goal: "帮我装一下 https://github.com/anthropics/skills/tree/main/skills/skill-creator"
				.to_string(),
			planning_mode_hint: None,
			conversation_history: Vec::new(),
		};

		assert_eq!(
			resolver.resolve_without_classifier(&request),
			Some(ShortcutIntent::EnsureSkillInstalled {
				source_url: "https://github.com/anthropics/skills/tree/main/skills/skill-creator"
					.to_string(),
				if_missing: true,
			})
		);
	}

	#[test]
	fn extracts_installed_skill_name_matches_from_registry() {
		let root = tempfile::tempdir().expect("temp root should exist");
		let registry = installed_skill_registry(root.path());
		let resolver = SkillShortcutResolver::new(registry);

		assert_eq!(
			resolver.referenced_skill_names("Use the skill-creator skill to explain eval workflow"),
			vec!["skill-creator".to_string()]
		);
		assert_eq!(
			resolver.resolve_installed_skill_name("Skill Creator"),
			Some("skill-creator".to_string())
		);
	}

	#[test]
	fn parse_structured_intent_accepts_fenced_json() {
		let parsed = parse_structured_intent(
			"```json\n{\"intent\":\"use_installed_skill\",\"source_url\":null,\"skill_name\":\"skill-creator\",\"if_missing\":false,\"confidence\":0.91}\n```",
		)
		.expect("classification should parse");

		assert_eq!(parsed.intent, StructuredIntentLabel::UseInstalledSkill);
		assert_eq!(parsed.skill_name.as_deref(), Some("skill-creator"));
		assert_eq!(parsed.confidence, 0.91);
	}

	fn installed_skill_registry(root: &std::path::Path) -> SkillRegistry {
		let registry = SkillRegistry::file_backed(root.join("skills")).with_fetcher(Arc::new(
			StaticArchiveFetcher {
				archive: DownloadedArchive {
					archive_url: "https://example.com/archive.zip".to_string(),
					bytes: skill_archive_bytes(),
					resolved_reference: Some("main".to_string()),
				},
			},
		));
		registry
			.install_from_url(
				"https://github.com/anthropics/skills/tree/main/skills/skill-creator",
				"test-suite",
			)
			.expect("install should succeed");
		registry
	}

	fn skill_archive_bytes() -> Vec<u8> {
		let mut cursor = Cursor::new(Vec::new());
		{
			let mut writer = zip::ZipWriter::new(&mut cursor);
			let options = zip::write::SimpleFileOptions::default();
			writer
				.add_directory("skills-main/", options)
				.expect("root dir should be added");
			writer
				.add_directory("skills-main/skills/", options)
				.expect("skills dir should be added");
			writer
				.add_directory("skills-main/skills/skill-creator/", options)
				.expect("skill dir should be added");
			writer
				.start_file("skills-main/skills/skill-creator/SKILL.md", options)
				.expect("skill file should start");
			writer
				.write_all(
					br#"---
name: skill-creator
description: Build and evaluate new skills.
---

# Skill Creator
"#,
				)
				.expect("skill file should write");
			writer.finish().expect("zip should finish");
		}
		cursor.into_inner()
	}
}
