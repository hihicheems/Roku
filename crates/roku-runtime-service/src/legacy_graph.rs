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

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;

use roku_common_types::{
	AgentContext, AgentInstanceSpec, NodeId, PolicyBindings, RecoveryEligibility, RerunPolicy,
	ResultEnvelope, ResultStatus, ResumeCandidate, RuntimeError, Task, TaskEdgeCondition,
	TaskGraph, TaskNode, TaskNodeDispatchPolicy, TaskNodeKind,
};

const PROFILE_RESEARCH: &str = "research";
const PROFILE_DATA: &str = "data";
const PROFILE_REVIEW: &str = "review";
const PROFILE_SKILL: &str = "skill";
const PROFILE_GENERAL: &str = "general";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphCompletionAssessment {
	pub completed: bool,
	pub reason: String,
	pub final_node_id: Option<NodeId>,
	pub final_message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphScheduleError {
	UnknownNode(String),
	CycleDetected,
}

impl fmt::Display for GraphScheduleError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::UnknownNode(node_id) => write!(f, "graph references unknown node: {node_id}"),
			Self::CycleDetected => write!(f, "graph contains a cycle"),
		}
	}
}

impl std::error::Error for GraphScheduleError {}

#[derive(Debug, Clone, Copy)]
struct CapabilityProfile {
	profile_id: &'static str,
	capability_prefixes: &'static [&'static str],
	default_budget_tokens: u64,
	default_time_budget_ms: u64,
}

impl CapabilityProfile {
	fn match_score(self, capabilities: &[String]) -> usize {
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

const CAPABILITY_PROFILES: [CapabilityProfile; 5] = [
	CapabilityProfile {
		profile_id: PROFILE_RESEARCH,
		capability_prefixes: &["information.", "research."],
		default_budget_tokens: 10_000,
		default_time_budget_ms: 30_000,
	},
	CapabilityProfile {
		profile_id: PROFILE_DATA,
		capability_prefixes: &["data."],
		default_budget_tokens: 12_000,
		default_time_budget_ms: 35_000,
	},
	CapabilityProfile {
		profile_id: PROFILE_REVIEW,
		capability_prefixes: &["review.", "validation."],
		default_budget_tokens: 8_000,
		default_time_budget_ms: 20_000,
	},
	CapabilityProfile {
		profile_id: PROFILE_SKILL,
		capability_prefixes: &["skill."],
		default_budget_tokens: 10_000,
		default_time_budget_ms: 120_000,
	},
	CapabilityProfile {
		profile_id: PROFILE_GENERAL,
		capability_prefixes: &[],
		default_budget_tokens: 8_000,
		default_time_budget_ms: 20_000,
	},
];

#[derive(Debug, Default)]
pub struct LegacyTaskGraphScheduler;

impl LegacyTaskGraphScheduler {
	pub fn ready_nodes(
		&self,
		graph: &TaskGraph,
		completed: &[NodeId],
	) -> Result<Vec<TaskNode>, GraphScheduleError> {
		self.ensure_valid(graph)?;
		let automatic_node_ids = automatic_node_ids(graph);

		let completed_ids = completed
			.iter()
			.filter(|node_id| automatic_node_ids.contains(&node_id.0))
			.map(|node_id| node_id.0.clone())
			.collect::<HashSet<_>>();
		let dependencies = dependency_map(graph, &automatic_node_ids)?;

		Ok(graph
			.nodes
			.iter()
			.filter(|node| node.dispatch_policy == TaskNodeDispatchPolicy::Automatic)
			.filter(|node| !completed_ids.contains(&node.node_id.0))
			.filter(|node| {
				dependencies.get(&node.node_id.0).is_none_or(|parents| {
					parents.iter().all(|parent| completed_ids.contains(parent))
				})
			})
			.cloned()
			.collect())
	}

	pub fn execution_layers(
		&self,
		graph: &TaskGraph,
	) -> Result<Vec<Vec<NodeId>>, GraphScheduleError> {
		self.validate_edges(graph)?;
		let automatic_node_ids = automatic_node_ids(graph);
		let outgoing = outgoing_map(graph, &automatic_node_ids)?;
		let mut indegree = graph
			.nodes
			.iter()
			.filter(|node| automatic_node_ids.contains(&node.node_id.0))
			.map(|node| (node.node_id.0.clone(), 0usize))
			.collect::<HashMap<_, _>>();

		for edge in &graph.edges {
			if !edge_is_active_for_automatic_schedule(edge.condition) {
				continue;
			}
			if !automatic_node_ids.contains(&edge.from.0)
				|| !automatic_node_ids.contains(&edge.to.0)
			{
				continue;
			}
			let target = indegree
				.get_mut(&edge.to.0)
				.ok_or_else(|| GraphScheduleError::UnknownNode(edge.to.0.clone()))?;
			*target += 1;
		}

		let mut queue = graph
			.nodes
			.iter()
			.filter(|node| automatic_node_ids.contains(&node.node_id.0))
			.filter(|node| indegree.get(&node.node_id.0) == Some(&0))
			.map(|node| node.node_id.0.clone())
			.collect::<VecDeque<_>>();
		let mut processed = 0usize;
		let mut layers = Vec::new();

		while !queue.is_empty() {
			let layer_size = queue.len();
			let mut layer = Vec::with_capacity(layer_size);

			for _ in 0..layer_size {
				let node_id = queue
					.pop_front()
					.expect("queue length is checked before each layer iteration");
				processed += 1;
				layer.push(NodeId(node_id.clone()));

				for next in outgoing.get(&node_id).into_iter().flatten() {
					if let Some(indegree) = indegree.get_mut(next) {
						*indegree = indegree.saturating_sub(1);
						if *indegree == 0 {
							queue.push_back(next.clone());
						}
					}
				}
			}

			layers.push(layer);
		}

		if processed != automatic_node_ids.len() {
			return Err(GraphScheduleError::CycleDetected);
		}

		Ok(layers)
	}

	pub fn is_complete(
		&self,
		graph: &TaskGraph,
		completed: &[NodeId],
	) -> Result<bool, GraphScheduleError> {
		self.ensure_valid(graph)?;
		let automatic_node_ids = automatic_node_ids(graph);
		let completed_ids = completed
			.iter()
			.filter(|node_id| automatic_node_ids.contains(&node_id.0))
			.map(|node_id| node_id.0.clone())
			.collect::<HashSet<_>>();

		Ok(graph
			.nodes
			.iter()
			.filter(|node| node.dispatch_policy == TaskNodeDispatchPolicy::Automatic)
			.all(|node| completed_ids.contains(&node.node_id.0)))
	}

	pub fn replay_ready_nodes(
		&self,
		graph: &TaskGraph,
		completed: &[NodeId],
	) -> Result<Vec<TaskNode>, GraphScheduleError> {
		Ok(self
			.ready_nodes(graph, completed)?
			.into_iter()
			.filter(|node| !matches!(node.rerun_policy, RerunPolicy::Never))
			.collect())
	}

	pub fn resume_candidates(
		&self,
		graph: &TaskGraph,
		completed: &[NodeId],
	) -> Result<Vec<ResumeCandidate>, GraphScheduleError> {
		Ok(self
			.replay_ready_nodes(graph, completed)?
			.into_iter()
			.map(|node| {
				let eligibility = recovery_eligibility_for_node(&node);
				ResumeCandidate {
					node_id: node.node_id,
					kind: node.kind,
					resume_point_id: node.recovery_anchor.resume_point_id,
					eligibility,
					deadline_ms: node.deadline_ms,
					capability_requirements: node.capability_requirements_snapshot,
					rerun_policy: node.rerun_policy,
				}
			})
			.collect())
	}

	fn ensure_valid(&self, graph: &TaskGraph) -> Result<(), GraphScheduleError> {
		self.validate_edges(graph)?;
		let _ = self.execution_layers(graph)?;
		Ok(())
	}

	fn validate_edges(&self, graph: &TaskGraph) -> Result<(), GraphScheduleError> {
		for edge in &graph.edges {
			let from_exists = graph.nodes.iter().any(|node| node.node_id == edge.from);
			let to_exists = graph.nodes.iter().any(|node| node.node_id == edge.to);
			if !from_exists {
				return Err(GraphScheduleError::UnknownNode(edge.from.0.clone()));
			}
			if !to_exists {
				return Err(GraphScheduleError::UnknownNode(edge.to.0.clone()));
			}
		}
		Ok(())
	}
}

pub fn build_agent_instance_for_node_with_history(
	task: &Task,
	node: &TaskNode,
	memory_context: &str,
) -> AgentInstanceSpec {
	let profile = select_profile_for_node(node);
	let policy_bindings = derive_policy_bindings(node, profile);

	AgentInstanceSpec {
		instance_id: format!("agent-{}-{}", profile.profile_id, node.node_id.0),
		context: AgentContext {
			task_id: task.task_id.clone(),
			node_id: NodeId(node.node_id.0.clone()),
			summary: node.description.clone(),
			resources: node.resources.clone(),
			conversation_history: task.conversation_history.clone(),
			memory_context: memory_context.to_string(),
		},
		capabilities: node.capabilities.clone(),
		capability_tokens: Vec::new(),
		policy_bindings,
	}
}

pub fn assess_graph_completion(
	task: &Task,
	results: &[ResultEnvelope],
) -> Result<GraphCompletionAssessment, RuntimeError> {
	let Some(graph) = &task.graph else {
		return Ok(GraphCompletionAssessment {
			completed: false,
			reason: "task graph is missing".to_string(),
			final_node_id: None,
			final_message: None,
		});
	};

	let scheduler = LegacyTaskGraphScheduler;
	let is_complete = scheduler
		.is_complete(graph, &task.completed_nodes)
		.map_err(|error| RuntimeError::new(error.to_string()))?;

	let selected_result = is_complete
		.then(|| select_final_result(graph, results))
		.flatten();
	let selected_message = selected_result.map(result_message);
	let selected_node_id = selected_result.map(|result| result.node_id.clone());

	Ok(GraphCompletionAssessment {
		completed: is_complete,
		reason: if is_complete {
			"all task graph nodes completed".to_string()
		} else {
			"task graph still has incomplete nodes".to_string()
		},
		final_node_id: selected_node_id,
		final_message: selected_message,
	})
}

fn select_profile_for_node(node: &TaskNode) -> CapabilityProfile {
	let mut selected = CAPABILITY_PROFILES
		.iter()
		.find(|profile| profile.profile_id == PROFILE_GENERAL)
		.copied()
		.expect("general capability profile must exist");
	let mut selected_score = 0usize;

	for profile in CAPABILITY_PROFILES {
		let score = profile.match_score(&node.capabilities);
		if score > selected_score
			|| (score == selected_score && score > 0 && profile.profile_id < selected.profile_id)
		{
			selected = profile;
			selected_score = score;
		}
	}

	selected
}

fn derive_policy_bindings(node: &TaskNode, profile: CapabilityProfile) -> PolicyBindings {
	let capability_count = u64::try_from(node.capabilities.len()).unwrap_or(0);
	let mut budget_tokens = profile
		.default_budget_tokens
		.saturating_add(capability_count.saturating_mul(500));
	let mut time_budget_ms = profile
		.default_time_budget_ms
		.saturating_add(capability_count.saturating_mul(1_000));

	if matches!(
		node.kind,
		TaskNodeKind::Validation
			| TaskNodeKind::Aggregation
			| TaskNodeKind::Retry
			| TaskNodeKind::DeadLetter
	) {
		budget_tokens = budget_tokens.min(6_000);
		time_budget_ms = time_budget_ms.min(15_000);
	}

	if node.budget_snapshot.token_budget > 0 {
		budget_tokens = budget_tokens.min(node.budget_snapshot.token_budget);
	}
	if node.budget_snapshot.time_budget_ms > 0 {
		time_budget_ms = time_budget_ms.min(node.budget_snapshot.time_budget_ms);
	}
	if node.deadline_ms > 0 {
		time_budget_ms = time_budget_ms.min(node.deadline_ms);
	}

	PolicyBindings {
		budget_tokens,
		time_budget_ms,
	}
}

fn automatic_node_ids(graph: &TaskGraph) -> HashSet<String> {
	graph
		.nodes
		.iter()
		.filter(|node| node.dispatch_policy == TaskNodeDispatchPolicy::Automatic)
		.map(|node| node.node_id.0.clone())
		.collect()
}

fn dependency_map(
	graph: &TaskGraph,
	eligible_node_ids: &HashSet<String>,
) -> Result<HashMap<String, Vec<String>>, GraphScheduleError> {
	let known_nodes = graph
		.nodes
		.iter()
		.map(|node| node.node_id.0.clone())
		.collect::<HashSet<_>>();
	let mut dependencies = HashMap::<String, Vec<String>>::new();

	for edge in &graph.edges {
		if !known_nodes.contains(&edge.from.0) {
			return Err(GraphScheduleError::UnknownNode(edge.from.0.clone()));
		}
		if !known_nodes.contains(&edge.to.0) {
			return Err(GraphScheduleError::UnknownNode(edge.to.0.clone()));
		}
		if !edge_is_active_for_automatic_schedule(edge.condition) {
			continue;
		}
		if !eligible_node_ids.contains(&edge.from.0) || !eligible_node_ids.contains(&edge.to.0) {
			continue;
		}
		dependencies
			.entry(edge.to.0.clone())
			.or_default()
			.push(edge.from.0.clone());
	}

	Ok(dependencies)
}

fn outgoing_map(
	graph: &TaskGraph,
	eligible_node_ids: &HashSet<String>,
) -> Result<HashMap<String, Vec<String>>, GraphScheduleError> {
	let known_nodes = graph
		.nodes
		.iter()
		.map(|node| node.node_id.0.clone())
		.collect::<HashSet<_>>();
	let mut outgoing = HashMap::<String, Vec<String>>::new();

	for edge in &graph.edges {
		if !known_nodes.contains(&edge.from.0) {
			return Err(GraphScheduleError::UnknownNode(edge.from.0.clone()));
		}
		if !known_nodes.contains(&edge.to.0) {
			return Err(GraphScheduleError::UnknownNode(edge.to.0.clone()));
		}
		if !edge_is_active_for_automatic_schedule(edge.condition) {
			continue;
		}
		if !eligible_node_ids.contains(&edge.from.0) || !eligible_node_ids.contains(&edge.to.0) {
			continue;
		}
		outgoing
			.entry(edge.from.0.clone())
			.or_default()
			.push(edge.to.0.clone());
	}

	Ok(outgoing)
}

fn recovery_eligibility_for_node(node: &TaskNode) -> RecoveryEligibility {
	if node.recovery_anchor.requires_manual_resume
		|| matches!(node.rerun_policy, RerunPolicy::RequiresManualResume)
	{
		RecoveryEligibility::RequiresManualResume
	} else {
		RecoveryEligibility::ResumeReady
	}
}

fn edge_is_active_for_automatic_schedule(condition: TaskEdgeCondition) -> bool {
	matches!(
		condition,
		TaskEdgeCondition::Always | TaskEdgeCondition::OnSuccess | TaskEdgeCondition::OnApproved
	)
}

fn select_final_result<'a>(
	graph: &TaskGraph,
	results: &'a [ResultEnvelope],
) -> Option<&'a ResultEnvelope> {
	let successful_results = results
		.iter()
		.filter(|result| matches!(result.status, ResultStatus::Ok))
		.collect::<Vec<_>>();
	if successful_results.is_empty() {
		return None;
	}

	let node_by_id = graph
		.nodes
		.iter()
		.map(|node| (node.node_id.0.as_str(), node))
		.collect::<HashMap<_, _>>();
	let node_position = graph
		.nodes
		.iter()
		.enumerate()
		.map(|(index, node)| (node.node_id.0.as_str(), index))
		.collect::<HashMap<_, _>>();
	let completion_path_sources = graph
		.edges
		.iter()
		.filter(|edge| edge_is_completion_path(edge.condition))
		.map(|edge| edge.from.0.as_str())
		.collect::<HashSet<_>>();

	for kind in [
		TaskNodeKind::Aggregation,
		TaskNodeKind::Validation,
		TaskNodeKind::Execution,
	] {
		if let Some(result) = pick_best_result(
			successful_results.iter().copied().filter(|result| {
				node_by_id
					.get(result.node_id.0.as_str())
					.is_some_and(|node| node.kind == kind)
					&& !completion_path_sources.contains(result.node_id.0.as_str())
			}),
			&node_position,
		) {
			return Some(result);
		}
	}

	for kind in [
		TaskNodeKind::Aggregation,
		TaskNodeKind::Validation,
		TaskNodeKind::Execution,
	] {
		if let Some(result) = pick_best_result(
			successful_results.iter().copied().filter(|result| {
				node_by_id
					.get(result.node_id.0.as_str())
					.is_some_and(|node| node.kind == kind)
			}),
			&node_position,
		) {
			return Some(result);
		}
	}

	pick_best_result(successful_results.into_iter(), &node_position)
}

fn pick_best_result<'a>(
	candidates: impl Iterator<Item = &'a ResultEnvelope>,
	node_position: &HashMap<&str, usize>,
) -> Option<&'a ResultEnvelope> {
	candidates.max_by(|left, right| {
		left.confidence
			.total_cmp(&right.confidence)
			.then_with(|| {
				let left_position = node_position
					.get(left.node_id.0.as_str())
					.copied()
					.unwrap_or(0);
				let right_position = node_position
					.get(right.node_id.0.as_str())
					.copied()
					.unwrap_or(0);
				left_position.cmp(&right_position)
			})
			.then_with(|| left.node_id.0.cmp(&right.node_id.0))
	})
}

fn edge_is_completion_path(condition: TaskEdgeCondition) -> bool {
	matches!(
		condition,
		TaskEdgeCondition::Always | TaskEdgeCondition::OnSuccess | TaskEdgeCondition::OnApproved
	)
}

fn result_message(result: &ResultEnvelope) -> String {
	extract_message(&result.payload).unwrap_or_else(|| result.payload.clone())
}

fn extract_message(payload: &str) -> Option<String> {
	let parsed = serde_json::from_str::<serde_json::Value>(payload).ok()?;
	parsed
		.get("message")
		.and_then(serde_json::Value::as_str)
		.map(str::to_string)
}
