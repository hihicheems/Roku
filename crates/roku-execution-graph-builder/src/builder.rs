use roku_common_types::{NodeId, PlanOutline, TaskEdge, TaskGraph, TaskId, TaskNode, TaskNodeKind};

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
	) -> TaskGraph {
		let mut nodes = Vec::new();
		let mut edges = Vec::new();
		let mut previous_node = None;

		for step in &outline.steps {
			let node_id = NodeId(step.step_id.clone());
			nodes.push(TaskNode {
				node_id: node_id.clone(),
				kind: TaskNodeKind::Execution,
				description: step.summary.clone(),
				capabilities: step.required_capabilities.clone(),
			});

			if let Some(previous_node_id) = previous_node {
				edges.push(TaskEdge {
					from: previous_node_id,
					to: node_id.clone(),
				});
			}
			previous_node = Some(node_id.clone());

			if cfg.include_approval_gate && step.requires_approval {
				let approval_id = NodeId(format!("{}-approval", step.step_id));
				nodes.push(TaskNode {
					node_id: approval_id.clone(),
					kind: TaskNodeKind::Approval,
					description: "Approval gate".to_string(),
					capabilities: vec!["approve.action".to_string()],
				});
				edges.push(TaskEdge {
					from: node_id,
					to: approval_id.clone(),
				});
				previous_node = Some(approval_id);
			}
		}

		if cfg.include_validation_gate {
			let validation_id = NodeId("validation-gate".to_string());
			nodes.push(TaskNode {
				node_id: validation_id.clone(),
				kind: TaskNodeKind::Validation,
				description: "Validation gate".to_string(),
				capabilities: vec!["validate.result".to_string()],
			});
			if let Some(previous_node_id) = previous_node {
				edges.push(TaskEdge {
					from: previous_node_id,
					to: validation_id,
				});
			}
		}

		TaskGraph {
			task_id,
			nodes,
			edges,
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use roku_common_types::{PlanOutline, PlanStep, TaskId, TaskNodeKind};

	#[test]
	fn compile_outline_to_graph() {
		let builder = ExecutionGraphBuilder;
		let graph = builder.compile(
			TaskId("t1".to_string()),
			&PlanOutline {
				goal: "g".to_string(),
				steps: vec![PlanStep {
					step_id: "s1".to_string(),
					summary: "do".to_string(),
					required_capabilities: vec![],
					requires_approval: true,
				}],
			},
			&GraphBuildConfig::default(),
		);

		assert_eq!(graph.nodes.len(), 3);
		assert_eq!(graph.nodes[1].kind, TaskNodeKind::Approval);
		assert_eq!(graph.nodes[2].kind, TaskNodeKind::Validation);
	}
}
