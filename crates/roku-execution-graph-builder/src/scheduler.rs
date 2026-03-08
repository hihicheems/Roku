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
	NodeId, RecoveryEligibility, RerunPolicy, ResumeCandidate, TaskGraph, TaskNode,
	TaskNodeDispatchPolicy,
};

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

#[derive(Debug, Default)]
pub struct TaskGraphScheduler;

impl TaskGraphScheduler {
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

#[cfg(test)]
mod tests {
	use super::*;
	use roku_common_types::{
		TaskEdge, TaskGraph, TaskId, TaskNode, TaskNodeDispatchPolicy, TaskNodeKind,
	};

	fn node(id: &str, kind: TaskNodeKind) -> TaskNode {
		TaskNode {
			node_id: NodeId(id.to_string()),
			kind,
			description: id.to_string(),
			capabilities: Vec::new(),
			dispatch_policy: TaskNodeDispatchPolicy::Automatic,
			join_policy: roku_common_types::JoinPolicy::AllParents,
			aggregation_mode: roku_common_types::AggregationMode::CollectAll,
			recovery_anchor: roku_common_types::NodeRecoveryAnchor {
				resume_point_id: format!("resume:{id}"),
				requires_manual_resume: matches!(kind, TaskNodeKind::Approval),
				allows_partial_rerun: matches!(kind, TaskNodeKind::Execution),
			},
			budget_snapshot: roku_common_types::NodeBudgetSnapshot {
				token_budget: 1_000,
				time_budget_ms: 1_000,
			},
			deadline_ms: 1_000,
			capability_requirements_snapshot: Vec::new(),
			retry_policy: roku_common_types::RetryPolicy::default(),
			rerun_policy: if matches!(kind, TaskNodeKind::Approval) {
				roku_common_types::RerunPolicy::RequiresManualResume
			} else {
				roku_common_types::RerunPolicy::SafeToRerun
			},
		}
	}

	fn manual_node(id: &str, kind: TaskNodeKind) -> TaskNode {
		TaskNode {
			dispatch_policy: TaskNodeDispatchPolicy::ManualRecovery,
			..node(id, kind)
		}
	}

	#[test]
	fn ready_nodes_respects_completed_dependencies() {
		let scheduler = TaskGraphScheduler;
		let graph = TaskGraph {
			task_id: TaskId("task-1".to_string()),
			nodes: vec![
				node("extract", TaskNodeKind::Execution),
				node("analyze", TaskNodeKind::Execution),
				node("validate", TaskNodeKind::Validation),
			],
			edges: vec![
				TaskEdge {
					from: NodeId("extract".to_string()),
					to: NodeId("analyze".to_string()),
				},
				TaskEdge {
					from: NodeId("analyze".to_string()),
					to: NodeId("validate".to_string()),
				},
			],
		};

		let first = scheduler
			.ready_nodes(&graph, &[])
			.expect("graph should be schedulable");
		assert_eq!(first.len(), 1);
		assert_eq!(first[0].node_id.0, "extract");

		let second = scheduler
			.ready_nodes(&graph, &[NodeId("extract".to_string())])
			.expect("graph should be schedulable");
		assert_eq!(second.len(), 1);
		assert_eq!(second[0].node_id.0, "analyze");
	}

	#[test]
	fn execution_layers_support_parallel_branches() {
		let scheduler = TaskGraphScheduler;
		let graph = TaskGraph {
			task_id: TaskId("task-1".to_string()),
			nodes: vec![
				node("extract-a", TaskNodeKind::Execution),
				node("extract-b", TaskNodeKind::Execution),
				node("join", TaskNodeKind::Validation),
			],
			edges: vec![
				TaskEdge {
					from: NodeId("extract-a".to_string()),
					to: NodeId("join".to_string()),
				},
				TaskEdge {
					from: NodeId("extract-b".to_string()),
					to: NodeId("join".to_string()),
				},
			],
		};

		let layers = scheduler
			.execution_layers(&graph)
			.expect("graph should be schedulable");
		assert_eq!(layers.len(), 2);
		assert_eq!(layers[0].len(), 2);
		assert_eq!(layers[1], vec![NodeId("join".to_string())]);
	}

	#[test]
	fn scheduler_rejects_cycles() {
		let scheduler = TaskGraphScheduler;
		let graph = TaskGraph {
			task_id: TaskId("task-1".to_string()),
			nodes: vec![
				node("a", TaskNodeKind::Execution),
				node("b", TaskNodeKind::Execution),
			],
			edges: vec![
				TaskEdge {
					from: NodeId("a".to_string()),
					to: NodeId("b".to_string()),
				},
				TaskEdge {
					from: NodeId("b".to_string()),
					to: NodeId("a".to_string()),
				},
			],
		};

		let error = scheduler
			.execution_layers(&graph)
			.expect_err("cyclic graph should be rejected");
		assert_eq!(error, GraphScheduleError::CycleDetected);
	}

	#[test]
	fn resume_candidates_reflect_manual_resume_policy() {
		let scheduler = TaskGraphScheduler;
		let graph = TaskGraph {
			task_id: TaskId("task-replay".to_string()),
			nodes: vec![node("approval", TaskNodeKind::Approval)],
			edges: Vec::new(),
		};

		let candidates = scheduler
			.resume_candidates(&graph, &[])
			.expect("resume candidates should resolve");
		assert_eq!(candidates.len(), 1);
		assert_eq!(
			candidates[0].eligibility,
			RecoveryEligibility::RequiresManualResume
		);
	}

	#[test]
	fn ready_nodes_ignore_manual_recovery_helpers() {
		let scheduler = TaskGraphScheduler;
		let graph = TaskGraph {
			task_id: TaskId("task-helpers".to_string()),
			nodes: vec![
				node("step", TaskNodeKind::Execution),
				manual_node("step-retry", TaskNodeKind::Retry),
				node("validation", TaskNodeKind::Validation),
			],
			edges: vec![
				TaskEdge {
					from: NodeId("step".to_string()),
					to: NodeId("step-retry".to_string()),
				},
				TaskEdge {
					from: NodeId("step".to_string()),
					to: NodeId("validation".to_string()),
				},
			],
		};

		let ready = scheduler
			.ready_nodes(&graph, &[NodeId("step".to_string())])
			.expect("graph should be schedulable");

		assert_eq!(ready.len(), 1);
		assert_eq!(ready[0].node_id, NodeId("validation".to_string()));
		assert!(
			scheduler
				.is_complete(
					&graph,
					&[NodeId("step".to_string()), NodeId("validation".to_string()),],
				)
				.expect("graph should be schedulable")
		);
	}
}
