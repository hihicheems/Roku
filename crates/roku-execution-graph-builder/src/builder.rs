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

use std::collections::{HashMap, HashSet};

use roku_common_types::{
	AggregationMode, JoinPolicy, NodeBudgetSnapshot, NodeId, NodeRecoveryAnchor, PlanOutline,
	RerunPolicy, RetryPolicy, TaskEdge, TaskEdgeCondition, TaskGraph, TaskId, TaskNode,
	TaskNodeDispatchPolicy, TaskNodeKind,
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum GraphBuildError {
	#[error("duplicate step id: {0}")]
	DuplicateStepId(String),
	#[error("step {step_id} depends on unknown step {dependency}")]
	MissingDependency { step_id: String, dependency: String },
}

#[derive(Debug, Clone)]
pub struct GraphBuildConfig {
	pub include_validation_gate: bool,
	pub include_approval_gate: bool,
	pub include_aggregation_gate: bool,
}

impl Default for GraphBuildConfig {
	fn default() -> Self {
		Self {
			include_validation_gate: true,
			include_approval_gate: true,
			include_aggregation_gate: true,
		}
	}
}

#[derive(Debug, Default)]
pub struct ExecutionGraphBuilder;

impl ExecutionGraphBuilder {
	pub fn compile(
		&self,
		task_id: TaskId,
		outline: &PlanOutline,
		cfg: &GraphBuildConfig,
	) -> Result<TaskGraph, GraphBuildError> {
		let mut nodes = Vec::new();
		let mut edges = Vec::new();
		let mut seen_steps = HashSet::new();
		let mut execution_nodes = HashMap::new();
		let mut terminal_nodes = HashMap::new();
		let mut terminal_node_kinds = HashMap::new();

		for step in &outline.steps {
			if !seen_steps.insert(step.step_id.clone()) {
				return Err(GraphBuildError::DuplicateStepId(step.step_id.clone()));
			}

			let execution_node_id = NodeId(step.step_id.clone());
			let execution_metadata = default_node_metadata(
				&execution_node_id,
				TaskNodeKind::Execution,
				&step.required_capabilities,
			);
			nodes.push(build_task_node(
				execution_node_id.clone(),
				TaskNodeKind::Execution,
				step.summary.clone(),
				step.required_capabilities.clone(),
				TaskNodeDispatchPolicy::Automatic,
				execution_metadata,
			));
			execution_nodes.insert(step.step_id.clone(), execution_node_id.clone());

			let retry_node_id = NodeId(format!("{}-retry", step.step_id));
			let retry_metadata = default_node_metadata(
				&retry_node_id,
				TaskNodeKind::Retry,
				&[String::from("control.retry")],
			);
			nodes.push(build_task_node(
				retry_node_id.clone(),
				TaskNodeKind::Retry,
				format!("Retry helper for {}", step.step_id),
				vec!["control.retry".to_string()],
				TaskNodeDispatchPolicy::ManualRecovery,
				retry_metadata,
			));
			edges.push(TaskEdge {
				from: execution_node_id.clone(),
				to: retry_node_id.clone(),
				condition: TaskEdgeCondition::OnFailureRetryable,
			});

			let dead_letter_node_id = NodeId(format!("{}-dead-letter", step.step_id));
			let dead_letter_metadata = default_node_metadata(
				&dead_letter_node_id,
				TaskNodeKind::DeadLetter,
				&[String::from("control.dead_letter")],
			);
			nodes.push(build_task_node(
				dead_letter_node_id.clone(),
				TaskNodeKind::DeadLetter,
				format!("Dead-letter helper for {}", step.step_id),
				vec!["control.dead_letter".to_string()],
				TaskNodeDispatchPolicy::ManualRecovery,
				dead_letter_metadata,
			));
			edges.push(TaskEdge {
				from: retry_node_id,
				to: dead_letter_node_id,
				condition: TaskEdgeCondition::OnFailureExhausted,
			});

			let terminal_node_id = if cfg.include_approval_gate && step.requires_approval {
				let approval_node_id = NodeId(format!("{}-approval", step.step_id));
				let approval_metadata = default_node_metadata(
					&approval_node_id,
					TaskNodeKind::Approval,
					&[String::from("approve.action")],
				);
				nodes.push(build_task_node(
					approval_node_id.clone(),
					TaskNodeKind::Approval,
					"Approval gate".to_string(),
					vec!["approve.action".to_string()],
					TaskNodeDispatchPolicy::Automatic,
					approval_metadata,
				));
				edges.push(TaskEdge {
					from: execution_node_id,
					to: approval_node_id.clone(),
					condition: TaskEdgeCondition::OnSuccess,
				});
				terminal_node_kinds.insert(step.step_id.clone(), TaskNodeKind::Approval);
				approval_node_id
			} else {
				terminal_node_kinds.insert(step.step_id.clone(), TaskNodeKind::Execution);
				execution_node_id
			};
			terminal_nodes.insert(step.step_id.clone(), terminal_node_id);
		}

		for step in &outline.steps {
			let execution_node_id = execution_nodes
				.get(&step.step_id)
				.expect("execution node should be built in first pass");
			for dependency in &step.depends_on {
				let dependency_terminal = terminal_nodes.get(dependency).ok_or_else(|| {
					GraphBuildError::MissingDependency {
						step_id: step.step_id.clone(),
						dependency: dependency.clone(),
					}
				})?;
				let dependency_kind = terminal_node_kinds
					.get(dependency)
					.copied()
					.unwrap_or(TaskNodeKind::Execution);
				edges.push(TaskEdge {
					from: dependency_terminal.clone(),
					to: execution_node_id.clone(),
					condition: completion_edge_condition(dependency_kind),
				});
			}
		}

		let mut validation_node_id = None;
		if cfg.include_validation_gate {
			let validation_id = NodeId("validation-gate".to_string());
			let validation_metadata = default_node_metadata(
				&validation_id,
				TaskNodeKind::Validation,
				&[String::from("validate.result")],
			);
			nodes.push(build_task_node(
				validation_id.clone(),
				TaskNodeKind::Validation,
				"Validation gate".to_string(),
				vec!["validate.result".to_string()],
				TaskNodeDispatchPolicy::Automatic,
				validation_metadata,
			));

			for terminal_node in terminal_step_nodes(outline, &terminal_nodes) {
				let terminal_kind = terminal_step_kind(
					outline,
					&terminal_nodes,
					&terminal_node_kinds,
					&terminal_node,
				);
				edges.push(TaskEdge {
					from: terminal_node,
					to: validation_id.clone(),
					condition: completion_edge_condition(terminal_kind),
				});
			}
			validation_node_id = Some(validation_id);
		}

		if cfg.include_aggregation_gate {
			let aggregation_id = NodeId("aggregation-gate".to_string());
			let aggregation_metadata = default_node_metadata(
				&aggregation_id,
				TaskNodeKind::Aggregation,
				&[String::from("aggregate.result")],
			);
			nodes.push(build_task_node(
				aggregation_id.clone(),
				TaskNodeKind::Aggregation,
				"Aggregation gate".to_string(),
				vec!["aggregate.result".to_string()],
				TaskNodeDispatchPolicy::Automatic,
				aggregation_metadata,
			));

			let aggregation_parents = validation_node_id.into_iter().collect::<Vec<_>>();
			if aggregation_parents.is_empty() {
				for terminal_node in terminal_step_nodes(outline, &terminal_nodes) {
					let terminal_kind = terminal_step_kind(
						outline,
						&terminal_nodes,
						&terminal_node_kinds,
						&terminal_node,
					);
					edges.push(TaskEdge {
						from: terminal_node,
						to: aggregation_id.clone(),
						condition: completion_edge_condition(terminal_kind),
					});
				}
			} else {
				for parent in aggregation_parents {
					edges.push(TaskEdge {
						from: parent,
						to: aggregation_id.clone(),
						condition: TaskEdgeCondition::OnSuccess,
					});
				}
			}
		}

		Ok(TaskGraph {
			task_id,
			nodes,
			edges,
		})
	}
}

fn completion_edge_condition(kind: TaskNodeKind) -> TaskEdgeCondition {
	match kind {
		TaskNodeKind::Approval => TaskEdgeCondition::OnApproved,
		_ => TaskEdgeCondition::OnSuccess,
	}
}

fn terminal_step_kind(
	outline: &PlanOutline,
	terminal_nodes: &HashMap<String, NodeId>,
	terminal_node_kinds: &HashMap<String, TaskNodeKind>,
	node_id: &NodeId,
) -> TaskNodeKind {
	outline
		.steps
		.iter()
		.find(|step| terminal_nodes.get(&step.step_id) == Some(node_id))
		.and_then(|step| terminal_node_kinds.get(&step.step_id).copied())
		.unwrap_or(TaskNodeKind::Execution)
}

struct NodeMetadata {
	recovery_anchor: NodeRecoveryAnchor,
	budget_snapshot: NodeBudgetSnapshot,
	deadline_ms: u64,
	capability_requirements_snapshot: Vec<String>,
	retry_policy: RetryPolicy,
	rerun_policy: RerunPolicy,
}

fn build_task_node(
	node_id: NodeId,
	kind: TaskNodeKind,
	description: String,
	capabilities: Vec<String>,
	dispatch_policy: TaskNodeDispatchPolicy,
	metadata: NodeMetadata,
) -> TaskNode {
	TaskNode {
		node_id,
		kind,
		description,
		capabilities,
		dispatch_policy,
		join_policy: JoinPolicy::AllParents,
		aggregation_mode: AggregationMode::CollectAll,
		recovery_anchor: metadata.recovery_anchor,
		budget_snapshot: metadata.budget_snapshot,
		deadline_ms: metadata.deadline_ms,
		capability_requirements_snapshot: metadata.capability_requirements_snapshot,
		retry_policy: metadata.retry_policy,
		rerun_policy: metadata.rerun_policy,
	}
}

fn default_node_metadata(
	node_id: &NodeId,
	kind: TaskNodeKind,
	capabilities: &[String],
) -> NodeMetadata {
	let (deadline_ms, retry_policy, rerun_policy, requires_manual_resume, allows_partial_rerun) =
		match kind {
			TaskNodeKind::Execution => (
				execution_deadline_ms(capabilities),
				RetryPolicy {
					max_attempts: 2,
					retry_on_timeout: true,
				},
				RerunPolicy::SafeToRerun,
				false,
				true,
			),
			TaskNodeKind::Validation => (
				15_000,
				RetryPolicy {
					max_attempts: 2,
					retry_on_timeout: false,
				},
				RerunPolicy::SafeToRerun,
				false,
				false,
			),
			TaskNodeKind::Approval => (
				300_000,
				RetryPolicy::default(),
				RerunPolicy::RequiresManualResume,
				true,
				false,
			),
			TaskNodeKind::Aggregation => (
				20_000,
				RetryPolicy {
					max_attempts: 1,
					retry_on_timeout: false,
				},
				RerunPolicy::SafeToRerun,
				false,
				false,
			),
			TaskNodeKind::Retry => (
				5_000,
				RetryPolicy::default(),
				RerunPolicy::SafeToRerun,
				false,
				true,
			),
			TaskNodeKind::DeadLetter => (
				5_000,
				RetryPolicy::default(),
				RerunPolicy::Never,
				false,
				false,
			),
		};
	let capability_count = u64::try_from(capabilities.len()).unwrap_or(u64::MAX);

	NodeMetadata {
		recovery_anchor: NodeRecoveryAnchor {
			resume_point_id: format!("resume:{}", node_id.0),
			requires_manual_resume,
			allows_partial_rerun,
		},
		budget_snapshot: NodeBudgetSnapshot {
			token_budget: execution_token_budget(node_id, capability_count),
			time_budget_ms: deadline_ms,
		},
		deadline_ms,
		capability_requirements_snapshot: capabilities.to_vec(),
		retry_policy,
		rerun_policy,
	}
}

fn execution_deadline_ms(capabilities: &[String]) -> u64 {
	if capabilities
		.iter()
		.any(|capability| capability == "skill.install")
	{
		120_000
	} else {
		45_000
	}
}

fn execution_token_budget(node_id: &NodeId, capability_count: u64) -> u64 {
	if node_id.0 == "use-installed-skill" {
		4_000
	} else {
		1_000u64.saturating_add(capability_count.saturating_mul(250))
	}
}

fn terminal_step_nodes(
	outline: &PlanOutline,
	terminal_nodes: &HashMap<String, NodeId>,
) -> Vec<NodeId> {
	let consumed_steps = outline
		.steps
		.iter()
		.flat_map(|step| step.depends_on.iter().cloned())
		.collect::<HashSet<_>>();

	outline
		.steps
		.iter()
		.filter(|step| !consumed_steps.contains(&step.step_id))
		.filter_map(|step| terminal_nodes.get(&step.step_id).cloned())
		.collect()
}

#[cfg(test)]
mod tests {
	use super::*;
	use roku_common_types::{
		PlanOutline, PlanStep, TaskEdgeCondition, TaskId, TaskNodeDispatchPolicy, TaskNodeKind,
	};

	#[test]
	fn compile_outline_to_graph() {
		let builder = ExecutionGraphBuilder;
		let graph = builder
			.compile(
				TaskId("t1".to_string()),
				&PlanOutline {
					goal: "g".to_string(),
					steps: vec![PlanStep {
						step_id: "s1".to_string(),
						summary: "do".to_string(),
						required_capabilities: vec![],
						requires_approval: true,
						depends_on: Vec::new(),
						branch: None,
						loop_control: None,
					}],
				},
				&GraphBuildConfig::default(),
			)
			.expect("graph compilation should succeed");

		assert_eq!(graph.nodes.len(), 6);
		assert_eq!(graph.nodes[0].recovery_anchor.resume_point_id, "resume:s1");
		assert!(graph.nodes[0].capability_requirements_snapshot.is_empty());
		assert!(graph.nodes[0].budget_snapshot.time_budget_ms > 0);
		assert!(
			graph
				.nodes
				.iter()
				.any(|node| node.kind == TaskNodeKind::Approval)
		);
		assert!(graph.nodes.iter().any(|node| {
			node.kind == TaskNodeKind::Retry
				&& node.dispatch_policy == TaskNodeDispatchPolicy::ManualRecovery
		}));
		assert!(
			graph
				.nodes
				.iter()
				.any(|node| node.kind == TaskNodeKind::DeadLetter)
		);
		assert!(
			graph
				.nodes
				.iter()
				.any(|node| node.kind == TaskNodeKind::Validation)
		);
		assert!(
			graph
				.nodes
				.iter()
				.any(|node| node.kind == TaskNodeKind::Aggregation)
		);
		assert!(graph.edges.iter().any(|edge| {
			edge.from == NodeId("s1".to_string())
				&& edge.to == NodeId("s1-retry".to_string())
				&& edge.condition == TaskEdgeCondition::OnFailureRetryable
		}));
		assert!(graph.edges.iter().any(|edge| {
			edge.from == NodeId("s1-retry".to_string())
				&& edge.to == NodeId("s1-dead-letter".to_string())
				&& edge.condition == TaskEdgeCondition::OnFailureExhausted
		}));
		assert!(graph.edges.iter().any(|edge| {
			edge.from == NodeId("s1".to_string())
				&& edge.to == NodeId("s1-approval".to_string())
				&& edge.condition == TaskEdgeCondition::OnSuccess
		}));
		assert!(graph.edges.iter().any(|edge| {
			edge.from == NodeId("s1-approval".to_string())
				&& edge.to == NodeId("validation-gate".to_string())
				&& edge.condition == TaskEdgeCondition::OnApproved
		}));
	}

	#[test]
	fn skill_install_steps_get_extended_execution_deadline() {
		let builder = ExecutionGraphBuilder;
		let graph = builder
			.compile(
				TaskId("t-skill".to_string()),
				&PlanOutline {
					goal: "install a skill".to_string(),
					steps: vec![PlanStep {
						step_id: "install-skill".to_string(),
						summary: "install skill".to_string(),
						required_capabilities: vec!["skill.install".to_string()],
						requires_approval: false,
						depends_on: Vec::new(),
						branch: None,
						loop_control: None,
					}],
				},
				&GraphBuildConfig::default(),
			)
			.expect("graph compilation should succeed");

		let node = graph
			.nodes
			.iter()
			.find(|node| node.node_id == NodeId("install-skill".to_string()))
			.expect("skill install node should exist");
		assert_eq!(node.deadline_ms, 120_000);
		assert_eq!(node.budget_snapshot.time_budget_ms, 120_000);
	}

	#[test]
	fn explicit_skill_usage_steps_get_expanded_token_budget() {
		let builder = ExecutionGraphBuilder;
		let graph = builder
			.compile(
				TaskId("t-skill-usage".to_string()),
				&PlanOutline {
					goal: "use an installed skill".to_string(),
					steps: vec![PlanStep {
						step_id: "use-installed-skill".to_string(),
						summary: "use installed skill".to_string(),
						required_capabilities: vec!["tool.invoke".to_string()],
						requires_approval: false,
						depends_on: Vec::new(),
						branch: None,
						loop_control: None,
					}],
				},
				&GraphBuildConfig::default(),
			)
			.expect("graph compilation should succeed");

		let node = graph
			.nodes
			.iter()
			.find(|node| node.node_id == NodeId("use-installed-skill".to_string()))
			.expect("skill usage node should exist");
		assert_eq!(node.budget_snapshot.token_budget, 4_000);
	}

	#[test]
	fn compile_branching_outline_with_join_validation() {
		let builder = ExecutionGraphBuilder;
		let graph = builder
			.compile(
				TaskId("t-branch".to_string()),
				&PlanOutline {
					goal: "branch".to_string(),
					steps: vec![
						PlanStep {
							step_id: "fetch".to_string(),
							summary: "fetch".to_string(),
							required_capabilities: vec![],
							requires_approval: false,
							depends_on: Vec::new(),
							branch: None,
							loop_control: None,
						},
						PlanStep {
							step_id: "analyze-a".to_string(),
							summary: "analyze a".to_string(),
							required_capabilities: vec![],
							requires_approval: false,
							depends_on: vec!["fetch".to_string()],
							branch: None,
							loop_control: None,
						},
						PlanStep {
							step_id: "analyze-b".to_string(),
							summary: "analyze b".to_string(),
							required_capabilities: vec![],
							requires_approval: false,
							depends_on: vec!["fetch".to_string()],
							branch: None,
							loop_control: None,
						},
					],
				},
				&GraphBuildConfig::default(),
			)
			.expect("graph compilation should succeed");

		let validation_parents = graph
			.edges
			.iter()
			.filter(|edge| edge.to == NodeId("validation-gate".to_string()))
			.map(|edge| edge.from.0.clone())
			.collect::<Vec<_>>();
		assert_eq!(validation_parents.len(), 2);
		assert!(validation_parents.contains(&"analyze-a".to_string()));
		assert!(validation_parents.contains(&"analyze-b".to_string()));
		assert!(graph.edges.iter().any(|edge| {
			edge.from == NodeId("validation-gate".to_string())
				&& edge.to == NodeId("aggregation-gate".to_string())
		}));
	}

	#[test]
	fn compile_dependency_edges_with_success_conditions() {
		let builder = ExecutionGraphBuilder;
		let graph = builder
			.compile(
				TaskId("t-conditions".to_string()),
				&PlanOutline {
					goal: "conditions".to_string(),
					steps: vec![
						PlanStep {
							step_id: "extract".to_string(),
							summary: "extract".to_string(),
							required_capabilities: vec![],
							requires_approval: false,
							depends_on: Vec::new(),
							branch: None,
							loop_control: None,
						},
						PlanStep {
							step_id: "analyze".to_string(),
							summary: "analyze".to_string(),
							required_capabilities: vec![],
							requires_approval: false,
							depends_on: vec!["extract".to_string()],
							branch: None,
							loop_control: None,
						},
					],
				},
				&GraphBuildConfig::default(),
			)
			.expect("graph compilation should succeed");

		assert!(graph.edges.iter().any(|edge| {
			edge.from == NodeId("extract".to_string())
				&& edge.to == NodeId("analyze".to_string())
				&& edge.condition == TaskEdgeCondition::OnSuccess
		}));
	}

	#[test]
	fn reject_outline_with_missing_dependency() {
		let builder = ExecutionGraphBuilder;
		let error = builder
			.compile(
				TaskId("t-error".to_string()),
				&PlanOutline {
					goal: "invalid".to_string(),
					steps: vec![PlanStep {
						step_id: "step-1".to_string(),
						summary: "invalid".to_string(),
						required_capabilities: vec![],
						requires_approval: false,
						depends_on: vec!["unknown".to_string()],
						branch: None,
						loop_control: None,
					}],
				},
				&GraphBuildConfig::default(),
			)
			.expect_err("missing dependency should fail graph compilation");

		assert!(matches!(error, GraphBuildError::MissingDependency { .. }));
	}
}
