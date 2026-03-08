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

//! Build capability-based dynamic agent instances from task node requirements.

use std::collections::{HashMap, HashSet};

use roku_common_types::{
	AgentContext, AgentInstanceSpec, AggregationMode, ConversationTurn, JoinPolicy, NodeId,
	PolicyBindings, TaskId, TaskNode, TaskNodeKind,
};

const PROFILE_RESEARCH: &str = "research";
const PROFILE_DATA: &str = "data";
const PROFILE_REVIEW: &str = "review";
const PROFILE_GENERAL: &str = "general";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefaultProfile {
	Research,
	Data,
	Review,
	General,
}

impl DefaultProfile {
	fn profile_id(self) -> &'static str {
		match self {
			Self::Research => PROFILE_RESEARCH,
			Self::Data => PROFILE_DATA,
			Self::Review => PROFILE_REVIEW,
			Self::General => PROFILE_GENERAL,
		}
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityProfile {
	pub profile_id: String,
	pub capability_prefixes: Vec<String>,
	pub default_capabilities: Vec<String>,
	pub default_budget_tokens: u64,
	pub default_time_budget_ms: u64,
}

impl CapabilityProfile {
	pub fn new(
		profile_id: impl Into<String>,
		capability_prefixes: Vec<String>,
		default_capabilities: Vec<String>,
		default_budget_tokens: u64,
		default_time_budget_ms: u64,
	) -> Self {
		Self {
			profile_id: profile_id.into(),
			capability_prefixes,
			default_capabilities,
			default_budget_tokens,
			default_time_budget_ms,
		}
	}

	fn match_score(&self, capabilities: &[String]) -> usize {
		capabilities
			.iter()
			.filter(|capability| {
				self.capability_prefixes
					.iter()
					.any(|prefix| capability.starts_with(prefix))
			})
			.count()
	}
}

#[derive(Debug)]
pub struct AgentInstanceFactory {
	profiles: HashMap<String, CapabilityProfile>,
	fallback_profile_id: String,
}

impl AgentInstanceFactory {
	pub fn register_profile(&mut self, profile: CapabilityProfile) -> Option<CapabilityProfile> {
		self.profiles.insert(profile.profile_id.clone(), profile)
	}

	pub fn build_for_node(&self, task_id: &TaskId, node: &TaskNode) -> AgentInstanceSpec {
		self.build_for_node_with_history(task_id, node, &[])
	}

	pub fn build_for_node_with_history(
		&self,
		task_id: &TaskId,
		node: &TaskNode,
		history: &[ConversationTurn],
	) -> AgentInstanceSpec {
		let profile = self.select_profile_for_node(node);
		self.build_instance(task_id, node, profile, history)
	}

	pub fn build_from_profile(
		&self,
		task_id: &TaskId,
		profile: DefaultProfile,
	) -> AgentInstanceSpec {
		let profile_id = profile.profile_id();
		let profile = self
			.profiles
			.get(profile_id)
			.or_else(|| self.profiles.get(&self.fallback_profile_id))
			.expect("factory fallback profile must exist");

		let node = TaskNode {
			node_id: NodeId("profile-node".to_string()),
			kind: TaskNodeKind::Execution,
			description: format!("profile-based instance: {profile_id}"),
			capabilities: profile.default_capabilities.clone(),
			join_policy: JoinPolicy::default(),
			aggregation_mode: AggregationMode::default(),
			..TaskNode::default()
		};
		self.build_instance(task_id, &node, profile, &[])
	}

	pub fn inferred_profile_id(&self, node: &TaskNode) -> String {
		self.select_profile_for_node(node).profile_id.clone()
	}

	fn select_profile_for_node(&self, node: &TaskNode) -> &CapabilityProfile {
		let mut selected = self.profiles.get(&self.fallback_profile_id);
		let mut selected_score = 0usize;

		for profile in self.profiles.values() {
			let score = profile.match_score(&node.capabilities);
			if score > selected_score
				|| (score == selected_score
					&& score > 0 && selected
					.map(|current| profile.profile_id < current.profile_id)
					.unwrap_or(true))
			{
				selected = Some(profile);
				selected_score = score;
			}
		}

		selected.expect("factory fallback profile must exist")
	}

	fn build_instance(
		&self,
		task_id: &TaskId,
		node: &TaskNode,
		profile: &CapabilityProfile,
		history: &[ConversationTurn],
	) -> AgentInstanceSpec {
		let merged_capabilities =
			merge_capabilities(&profile.default_capabilities, &node.capabilities);
		let policy_bindings = derive_policy_bindings(node, profile);

		AgentInstanceSpec {
			instance_id: format!("agent-{}-{}", profile.profile_id, node.node_id.0),
			context: AgentContext {
				task_id: task_id.clone(),
				node_id: NodeId(node.node_id.0.clone()),
				summary: node.description.clone(),
				conversation_history: history.to_vec(),
			},
			capabilities: merged_capabilities,
			policy_bindings,
		}
	}
}

impl Default for AgentInstanceFactory {
	fn default() -> Self {
		let mut profiles = HashMap::new();
		for profile in default_profiles() {
			profiles.insert(profile.profile_id.clone(), profile);
		}

		Self {
			profiles,
			fallback_profile_id: PROFILE_GENERAL.to_string(),
		}
	}
}

fn default_profiles() -> Vec<CapabilityProfile> {
	vec![
		CapabilityProfile::new(
			PROFILE_RESEARCH,
			vec!["information.".to_string(), "research.".to_string()],
			vec!["information.read".to_string()],
			10_000,
			30_000,
		),
		CapabilityProfile::new(
			PROFILE_DATA,
			vec!["data.".to_string()],
			vec!["data.read".to_string(), "data.write".to_string()],
			12_000,
			35_000,
		),
		CapabilityProfile::new(
			PROFILE_REVIEW,
			vec!["review.".to_string(), "validation.".to_string()],
			vec!["review.check".to_string()],
			8_000,
			20_000,
		),
		CapabilityProfile::new(PROFILE_GENERAL, Vec::new(), Vec::new(), 8_000, 20_000),
	]
}

fn merge_capabilities(profile_caps: &[String], node_caps: &[String]) -> Vec<String> {
	let mut seen = HashSet::new();
	let mut merged = Vec::new();

	for capability in profile_caps.iter().chain(node_caps.iter()) {
		if seen.insert(capability.clone()) {
			merged.push(capability.clone());
		}
	}

	merged
}

fn derive_policy_bindings(node: &TaskNode, profile: &CapabilityProfile) -> PolicyBindings {
	let capability_count = u64::try_from(node.capabilities.len()).unwrap_or(0);
	let mut budget_tokens = profile
		.default_budget_tokens
		.saturating_add(capability_count.saturating_mul(500));
	let mut time_budget_ms = profile
		.default_time_budget_ms
		.saturating_add(capability_count.saturating_mul(1_000));

	if matches!(
		node.kind,
		TaskNodeKind::Validation | TaskNodeKind::Aggregation
	) {
		budget_tokens = budget_tokens.min(6_000);
		time_budget_ms = time_budget_ms.min(15_000);
	}

	PolicyBindings {
		budget_tokens,
		time_budget_ms,
	}
}

#[cfg(test)]
mod tests {
	use roku_common_types::{AggregationMode, JoinPolicy};

	use super::*;

	fn execution_node(node_id: &str, capabilities: Vec<&str>) -> TaskNode {
		TaskNode {
			node_id: NodeId(node_id.to_string()),
			kind: TaskNodeKind::Execution,
			description: "node description".to_string(),
			capabilities: capabilities
				.into_iter()
				.map(std::string::ToString::to_string)
				.collect(),
			join_policy: JoinPolicy::default(),
			aggregation_mode: AggregationMode::default(),
			..TaskNode::default()
		}
	}

	#[test]
	fn select_data_profile_for_data_capabilities() {
		let factory = AgentInstanceFactory::default();
		let task_id = TaskId("task-data".to_string());
		let node = execution_node("node-1", vec!["data.read", "data.transform"]);

		let spec = factory.build_for_node(&task_id, &node);

		assert!(spec.instance_id.starts_with("agent-data-"));
		assert!(spec.capabilities.contains(&"data.read".to_string()));
		assert!(spec.capabilities.contains(&"data.write".to_string()));
		assert!(spec.capabilities.contains(&"data.transform".to_string()));
		assert!(spec.policy_bindings.budget_tokens >= 12_000);
		assert!(spec.policy_bindings.time_budget_ms >= 35_000);
	}

	#[test]
	fn register_and_select_custom_profile() {
		let mut factory = AgentInstanceFactory::default();
		factory.register_profile(CapabilityProfile::new(
			"quant",
			vec!["quant.".to_string()],
			vec!["quant.read".to_string()],
			20_000,
			60_000,
		));

		let task_id = TaskId("task-quant".to_string());
		let node = execution_node("node-q1", vec!["quant.backtest"]);
		let spec = factory.build_for_node(&task_id, &node);

		assert!(spec.instance_id.starts_with("agent-quant-"));
		assert!(spec.capabilities.contains(&"quant.read".to_string()));
		assert!(spec.capabilities.contains(&"quant.backtest".to_string()));
	}

	#[test]
	fn fallback_to_general_profile_when_no_capability_matches() {
		let factory = AgentInstanceFactory::default();
		let task_id = TaskId("task-general".to_string());
		let node = execution_node("node-g1", vec!["unknown.capability"]);

		let spec = factory.build_for_node(&task_id, &node);
		assert!(spec.instance_id.starts_with("agent-general-"));
		assert!(
			spec.capabilities
				.contains(&"unknown.capability".to_string())
		);
	}

	#[test]
	fn build_profile_instance_uses_expected_profile_defaults() {
		let factory = AgentInstanceFactory::default();
		let task_id = TaskId("task-profile".to_string());

		let spec = factory.build_from_profile(&task_id, DefaultProfile::Review);
		assert!(spec.instance_id.starts_with("agent-review-"));
		assert!(spec.capabilities.contains(&"review.check".to_string()));
	}
}
