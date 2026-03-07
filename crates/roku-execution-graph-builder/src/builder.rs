use std::collections::{HashMap, HashSet};

use roku_common_types::{
	AggregationMode, JoinPolicy, NodeId, PlanOutline, TaskEdge, TaskGraph, TaskId, TaskNode,
	TaskNodeKind,
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
}

impl Default for GraphBuildConfig {
	fn default() -> Self {
		Self {
			include_validation_gate: true,
			include_approval_gate: true,
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

		for step in &outline.steps {
			if !seen_steps.insert(step.step_id.clone()) {
				return Err(GraphBuildError::DuplicateStepId(step.step_id.clone()));
			}

			let execution_node_id = NodeId(step.step_id.clone());
			nodes.push(TaskNode {
				node_id: execution_node_id.clone(),
				kind: TaskNodeKind::Execution,
				description: step.summary.clone(),
				capabilities: step.required_capabilities.clone(),
				join_policy: JoinPolicy::AllParents,
				aggregation_mode: AggregationMode::CollectAll,
			});
			execution_nodes.insert(step.step_id.clone(), execution_node_id.clone());

			let terminal_node_id = if cfg.include_approval_gate && step.requires_approval {
				let approval_node_id = NodeId(format!("{}-approval", step.step_id));
				nodes.push(TaskNode {
					node_id: approval_node_id.clone(),
					kind: TaskNodeKind::Approval,
					description: "Approval gate".to_string(),
					capabilities: vec!["approve.action".to_string()],
					join_policy: JoinPolicy::AllParents,
					aggregation_mode: AggregationMode::CollectAll,
				});
				edges.push(TaskEdge {
					from: execution_node_id,
					to: approval_node_id.clone(),
				});
				approval_node_id
			} else {
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
				edges.push(TaskEdge {
					from: dependency_terminal.clone(),
					to: execution_node_id.clone(),
				});
			}
		}

		if cfg.include_validation_gate {
			let validation_id = NodeId("validation-gate".to_string());
			nodes.push(TaskNode {
				node_id: validation_id.clone(),
				kind: TaskNodeKind::Validation,
				description: "Validation gate".to_string(),
				capabilities: vec!["validate.result".to_string()],
				join_policy: JoinPolicy::AllParents,
				aggregation_mode: AggregationMode::CollectAll,
			});

			for terminal_node in terminal_step_nodes(outline, &terminal_nodes) {
				edges.push(TaskEdge {
					from: terminal_node,
					to: validation_id.clone(),
				});
			}
		}

		Ok(TaskGraph {
			task_id,
			nodes,
			edges,
		})
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
	use roku_common_types::{PlanOutline, PlanStep, TaskId, TaskNodeKind};

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
					}],
				},
				&GraphBuildConfig::default(),
			)
			.expect("graph compilation should succeed");

		assert_eq!(graph.nodes.len(), 3);
		assert_eq!(graph.nodes[1].kind, TaskNodeKind::Approval);
		assert_eq!(graph.nodes[2].kind, TaskNodeKind::Validation);
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
						},
						PlanStep {
							step_id: "analyze-a".to_string(),
							summary: "analyze a".to_string(),
							required_capabilities: vec![],
							requires_approval: false,
							depends_on: vec!["fetch".to_string()],
						},
						PlanStep {
							step_id: "analyze-b".to_string(),
							summary: "analyze b".to_string(),
							required_capabilities: vec![],
							requires_approval: false,
							depends_on: vec!["fetch".to_string()],
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
					}],
				},
				&GraphBuildConfig::default(),
			)
			.expect_err("missing dependency should fail graph compilation");

		assert!(matches!(error, GraphBuildError::MissingDependency { .. }));
	}
}
