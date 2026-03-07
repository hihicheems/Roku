//! Plan outline generation.

use roku_common_types::{PlanOutline, PlanStep, RequestEnvelope};
use roku_planning_engine::{PlanningDecision, PlanningMode};

pub trait TaskPlanner {
	fn build_outline(&self, request: &RequestEnvelope, decision: &PlanningDecision) -> PlanOutline;
}

#[derive(Debug, Default)]
pub struct SimpleTaskPlanner;

impl TaskPlanner for SimpleTaskPlanner {
	fn build_outline(&self, request: &RequestEnvelope, decision: &PlanningDecision) -> PlanOutline {
		let steps = match decision.mode {
			PlanningMode::ReAct => react_steps(decision),
			PlanningMode::TaskDecomposition => decomposition_steps(decision),
			PlanningMode::TreeSearch => tree_search_steps(decision),
			PlanningMode::IterativeRefinement => refinement_steps(decision),
		};

		PlanOutline {
			goal: request.goal.clone(),
			steps,
		}
	}
}

fn react_steps(decision: &PlanningDecision) -> Vec<PlanStep> {
	vec![
		PlanStep {
			step_id: "observe-context".to_string(),
			summary: format!(
				"Observe context with max {} iteration(s)",
				decision.max_iterations
			),
			required_capabilities: vec!["information.read".to_string()],
			requires_approval: false,
			depends_on: Vec::new(),
		},
		PlanStep {
			step_id: "act-primary".to_string(),
			summary: "Execute primary action".to_string(),
			required_capabilities: vec!["tool.invoke".to_string()],
			requires_approval: false,
			depends_on: vec!["observe-context".to_string()],
		},
	]
}

fn decomposition_steps(decision: &PlanningDecision) -> Vec<PlanStep> {
	let mut steps = vec![
		PlanStep {
			step_id: "decompose-goal".to_string(),
			summary: format!(
				"Decompose goal into branch tasks (max branches = {})",
				decision.max_branches
			),
			required_capabilities: vec!["information.read".to_string()],
			requires_approval: false,
			depends_on: Vec::new(),
		},
		PlanStep {
			step_id: "branch-data".to_string(),
			summary: "Run data branch".to_string(),
			required_capabilities: vec!["data.read".to_string(), "tool.invoke".to_string()],
			requires_approval: false,
			depends_on: vec!["decompose-goal".to_string()],
		},
		PlanStep {
			step_id: "branch-analysis".to_string(),
			summary: "Run analysis branch".to_string(),
			required_capabilities: vec!["research.analyze".to_string(), "tool.invoke".to_string()],
			requires_approval: false,
			depends_on: vec!["decompose-goal".to_string()],
		},
	];
	if decision.max_branches >= 3 {
		steps.push(PlanStep {
			step_id: "branch-risk".to_string(),
			summary: "Run risk review branch".to_string(),
			required_capabilities: vec!["risk.review".to_string()],
			requires_approval: false,
			depends_on: vec!["decompose-goal".to_string()],
		});
	}

	let mut merge_dependencies = vec!["branch-data".to_string(), "branch-analysis".to_string()];
	if decision.max_branches >= 3 {
		merge_dependencies.push("branch-risk".to_string());
	}
	steps.push(PlanStep {
		step_id: "merge-branches".to_string(),
		summary: "Merge decomposition branch results".to_string(),
		required_capabilities: vec!["result.merge".to_string()],
		requires_approval: false,
		depends_on: merge_dependencies,
	});
	steps
}

fn tree_search_steps(decision: &PlanningDecision) -> Vec<PlanStep> {
	let mut steps = vec![PlanStep {
		step_id: "search-root".to_string(),
		summary: "Generate tree-search seed hypotheses".to_string(),
		required_capabilities: vec!["information.read".to_string()],
		requires_approval: false,
		depends_on: Vec::new(),
	}];

	let mut branch_ids = Vec::new();
	for branch in 1..=decision.max_branches {
		let branch_id = format!("search-branch-{branch}");
		steps.push(PlanStep {
			step_id: branch_id.clone(),
			summary: format!("Explore tree branch {branch}"),
			required_capabilities: vec!["research.search".to_string(), "tool.invoke".to_string()],
			requires_approval: false,
			depends_on: vec!["search-root".to_string()],
		});
		branch_ids.push(branch_id);
	}
	steps.push(PlanStep {
		step_id: "search-evaluate".to_string(),
		summary: "Evaluate candidate branches".to_string(),
		required_capabilities: vec!["research.evaluate".to_string()],
		requires_approval: false,
		depends_on: branch_ids,
	});
	steps
}

fn refinement_steps(decision: &PlanningDecision) -> Vec<PlanStep> {
	let draft_id = "draft-v1".to_string();
	let mut steps = vec![PlanStep {
		step_id: draft_id.clone(),
		summary: "Generate initial draft".to_string(),
		required_capabilities: vec!["research.draft".to_string()],
		requires_approval: false,
		depends_on: Vec::new(),
	}];
	let mut previous = draft_id;
	for iteration in 1..=decision.max_iterations {
		let critique_id = format!("critique-{iteration}");
		let improve_id = format!("improve-{iteration}");
		steps.push(PlanStep {
			step_id: critique_id.clone(),
			summary: format!("Critique iteration {iteration}"),
			required_capabilities: vec!["review.critique".to_string()],
			requires_approval: false,
			depends_on: vec![previous.clone()],
		});
		steps.push(PlanStep {
			step_id: improve_id.clone(),
			summary: format!("Improve draft from critique {iteration}"),
			required_capabilities: vec!["research.improve".to_string()],
			requires_approval: false,
			depends_on: vec![critique_id],
		});
		previous = improve_id;
	}

	steps.push(PlanStep {
		step_id: "final-review".to_string(),
		summary: "Produce refinement final answer".to_string(),
		required_capabilities: vec!["review.finalize".to_string()],
		requires_approval: true,
		depends_on: vec![previous],
	});
	steps
}

#[cfg(test)]
mod tests {
	use super::*;

	fn decision(mode: PlanningMode, max_iterations: u8, max_branches: u8) -> PlanningDecision {
		PlanningDecision {
			mode,
			max_iterations,
			max_branches,
			hooks: Vec::new(),
		}
	}

	#[test]
	fn task_decomposition_contains_branch_merge_dependencies() {
		let planner = SimpleTaskPlanner;
		let outline = planner.build_outline(
			&RequestEnvelope {
				request_id: roku_common_types::RequestId("req-1".to_string()),
				session_id: "s1".to_string(),
				goal: "g".to_string(),
			},
			&decision(PlanningMode::TaskDecomposition, 4, 3),
		);

		let merge = outline
			.steps
			.iter()
			.find(|step| step.step_id == "merge-branches")
			.expect("merge step should exist");
		assert_eq!(merge.depends_on.len(), 3);
	}

	#[test]
	fn tree_search_contains_configured_branch_count() {
		let planner = SimpleTaskPlanner;
		let outline = planner.build_outline(
			&RequestEnvelope {
				request_id: roku_common_types::RequestId("req-1".to_string()),
				session_id: "s1".to_string(),
				goal: "g".to_string(),
			},
			&decision(PlanningMode::TreeSearch, 4, 2),
		);

		assert!(
			outline
				.steps
				.iter()
				.any(|step| step.step_id == "search-branch-1")
		);
		assert!(
			outline
				.steps
				.iter()
				.any(|step| step.step_id == "search-branch-2")
		);
		assert!(
			!outline
				.steps
				.iter()
				.any(|step| step.step_id == "search-branch-3")
		);
	}

	#[test]
	fn iterative_refinement_builds_critique_loop() {
		let planner = SimpleTaskPlanner;
		let outline = planner.build_outline(
			&RequestEnvelope {
				request_id: roku_common_types::RequestId("req-1".to_string()),
				session_id: "s1".to_string(),
				goal: "g".to_string(),
			},
			&decision(PlanningMode::IterativeRefinement, 2, 1),
		);

		assert!(
			outline
				.steps
				.iter()
				.any(|step| step.step_id == "critique-1")
		);
		assert!(outline.steps.iter().any(|step| step.step_id == "improve-2"));
		let final_review = outline
			.steps
			.iter()
			.find(|step| step.step_id == "final-review")
			.expect("final review should exist");
		assert_eq!(final_review.depends_on, vec!["improve-2".to_string()]);
		assert!(final_review.requires_approval);
	}
}
