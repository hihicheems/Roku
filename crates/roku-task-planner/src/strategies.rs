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

use roku_common_types::{PlanBranch, PlanLoopControl, PlanStep};
use roku_planning_engine::PlanningDecision;

pub(crate) fn build_skill_install_steps(
	goal: &str,
	source_url: &str,
	if_missing: bool,
) -> Vec<PlanStep> {
	vec![PlanStep {
		step_id: "install-skill".to_string(),
		summary: step_summary(
			goal,
			&format!(
				"Ensure requested skill from {source_url} is installed{}",
				if if_missing { " if it is missing" } else { "" }
			),
		),
		required_capabilities: vec!["skill.ensure_installed".to_string()],
		requires_approval: false,
		depends_on: Vec::new(),
		branch: None,
		loop_control: None,
	}]
}

pub(crate) fn build_explicit_skill_usage_steps(goal: &str, skill_name: &str) -> Vec<PlanStep> {
	vec![PlanStep {
		step_id: "use-installed-skill".to_string(),
		summary: step_summary(
			goal,
			&format!("Use installed skill `{skill_name}` for this request"),
		),
		required_capabilities: vec!["tool.invoke".to_string()],
		requires_approval: false,
		depends_on: Vec::new(),
		branch: None,
		loop_control: None,
	}]
}

pub(crate) fn build_react_steps(goal: &str, decision: &PlanningDecision) -> Vec<PlanStep> {
	if should_use_direct_react_action(goal) {
		return vec![PlanStep {
			step_id: "act-primary".to_string(),
			summary: step_summary(goal, "Execute primary action"),
			required_capabilities: vec!["tool.invoke".to_string()],
			requires_approval: false,
			depends_on: Vec::new(),
			branch: None,
			loop_control: None,
		}];
	}

	vec![
		PlanStep {
			step_id: "observe-context".to_string(),
			summary: step_summary(
				goal,
				&format!(
					"Observe context with max {} iteration(s)",
					decision.max_iterations
				),
			),
			required_capabilities: vec!["information.read".to_string()],
			requires_approval: false,
			depends_on: Vec::new(),
			branch: None,
			loop_control: None,
		},
		PlanStep {
			step_id: "act-primary".to_string(),
			summary: step_summary(goal, "Execute primary action"),
			required_capabilities: vec!["tool.invoke".to_string()],
			requires_approval: false,
			depends_on: vec!["observe-context".to_string()],
			branch: None,
			loop_control: None,
		},
	]
}

fn should_use_direct_react_action(goal: &str) -> bool {
	let normalized = goal.trim();
	if normalized.is_empty() || normalized.lines().count() > 1 {
		return false;
	}

	let short_goal = normalized.chars().count() <= 24;
	let lower = normalized.to_lowercase();
	let complex_markers = [
		"如何",
		"怎么",
		"步骤",
		"比较",
		"分析",
		"设计",
		"实现",
		"构建",
		"部署",
		"证明",
		"compare",
		"analyze",
		"design",
		"implement",
		"build ",
		"plan",
		"step by step",
	];

	short_goal
		&& !complex_markers
			.iter()
			.any(|marker| lower.contains(marker) || normalized.contains(marker))
}

pub(crate) fn build_decomposition_steps(goal: &str, decision: &PlanningDecision) -> Vec<PlanStep> {
	let mut steps = vec![
		PlanStep {
			step_id: "decompose-goal".to_string(),
			summary: step_summary(
				goal,
				&format!(
					"Decompose goal into branch tasks (max branches = {})",
					decision.max_branches
				),
			),
			required_capabilities: vec!["information.read".to_string()],
			requires_approval: false,
			depends_on: Vec::new(),
			branch: None,
			loop_control: None,
		},
		PlanStep {
			step_id: "branch-data".to_string(),
			summary: step_summary(goal, "Run data branch"),
			required_capabilities: vec!["data.read".to_string(), "tool.invoke".to_string()],
			requires_approval: false,
			depends_on: vec!["decompose-goal".to_string()],
			branch: Some(PlanBranch {
				branch_group: "task-decomposition".to_string(),
				branch_label: "data".to_string(),
			}),
			loop_control: None,
		},
		PlanStep {
			step_id: "branch-analysis".to_string(),
			summary: step_summary(goal, "Run analysis branch"),
			required_capabilities: vec!["research.analyze".to_string(), "tool.invoke".to_string()],
			requires_approval: false,
			depends_on: vec!["decompose-goal".to_string()],
			branch: Some(PlanBranch {
				branch_group: "task-decomposition".to_string(),
				branch_label: "analysis".to_string(),
			}),
			loop_control: None,
		},
	];
	if decision.max_branches >= 3 {
		steps.push(PlanStep {
			step_id: "branch-risk".to_string(),
			summary: step_summary(goal, "Run risk review branch"),
			required_capabilities: vec!["risk.review".to_string()],
			requires_approval: false,
			depends_on: vec!["decompose-goal".to_string()],
			branch: Some(PlanBranch {
				branch_group: "task-decomposition".to_string(),
				branch_label: "risk".to_string(),
			}),
			loop_control: None,
		});
	}

	let mut merge_dependencies = vec!["branch-data".to_string(), "branch-analysis".to_string()];
	if decision.max_branches >= 3 {
		merge_dependencies.push("branch-risk".to_string());
	}
	steps.push(PlanStep {
		step_id: "merge-branches".to_string(),
		summary: step_summary(goal, "Merge decomposition branch results"),
		required_capabilities: vec!["result.merge".to_string()],
		requires_approval: false,
		depends_on: merge_dependencies,
		branch: None,
		loop_control: None,
	});
	steps
}

pub(crate) fn build_tree_search_steps(goal: &str, decision: &PlanningDecision) -> Vec<PlanStep> {
	let mut steps = vec![PlanStep {
		step_id: "search-root".to_string(),
		summary: step_summary(goal, "Generate tree-search seed hypotheses"),
		required_capabilities: vec!["information.read".to_string()],
		requires_approval: false,
		depends_on: Vec::new(),
		branch: None,
		loop_control: None,
	}];

	let mut branch_ids = Vec::new();
	for branch in 1..=decision.max_branches {
		let branch_id = format!("search-branch-{branch}");
		steps.push(PlanStep {
			step_id: branch_id.clone(),
			summary: step_summary(goal, &format!("Explore tree branch {branch}")),
			required_capabilities: vec!["research.search".to_string(), "tool.invoke".to_string()],
			requires_approval: false,
			depends_on: vec!["search-root".to_string()],
			branch: Some(PlanBranch {
				branch_group: "tree-search".to_string(),
				branch_label: format!("branch-{branch}"),
			}),
			loop_control: None,
		});
		branch_ids.push(branch_id);
	}
	steps.push(PlanStep {
		step_id: "search-evaluate".to_string(),
		summary: step_summary(goal, "Evaluate candidate branches"),
		required_capabilities: vec!["research.evaluate".to_string()],
		requires_approval: false,
		depends_on: branch_ids,
		branch: None,
		loop_control: None,
	});
	steps
}

pub(crate) fn build_refinement_steps(goal: &str, decision: &PlanningDecision) -> Vec<PlanStep> {
	let draft_id = "draft-v1".to_string();
	let mut steps = vec![PlanStep {
		step_id: draft_id.clone(),
		summary: step_summary(goal, "Generate initial draft"),
		required_capabilities: vec!["research.draft".to_string()],
		requires_approval: false,
		depends_on: Vec::new(),
		branch: None,
		loop_control: None,
	}];
	let mut previous_step_id = draft_id;
	for iteration in 1..=decision.max_iterations {
		let critique_id = format!("critique-{iteration}");
		let improve_id = format!("improve-{iteration}");
		steps.push(PlanStep {
			step_id: critique_id.clone(),
			summary: step_summary(goal, &format!("Critique iteration {iteration}")),
			required_capabilities: vec!["review.critique".to_string()],
			requires_approval: false,
			depends_on: vec![previous_step_id.clone()],
			branch: None,
			loop_control: Some(PlanLoopControl {
				loop_id: "iterative-refinement".to_string(),
				iteration,
				max_iterations: decision.max_iterations,
			}),
		});
		steps.push(PlanStep {
			step_id: improve_id.clone(),
			summary: step_summary(goal, &format!("Improve draft from critique {iteration}")),
			required_capabilities: vec!["research.improve".to_string()],
			requires_approval: false,
			depends_on: vec![critique_id],
			branch: None,
			loop_control: Some(PlanLoopControl {
				loop_id: "iterative-refinement".to_string(),
				iteration,
				max_iterations: decision.max_iterations,
			}),
		});
		previous_step_id = improve_id;
	}

	steps.push(PlanStep {
		step_id: "final-review".to_string(),
		summary: step_summary(goal, "Produce refinement final answer"),
		required_capabilities: vec!["review.finalize".to_string()],
		requires_approval: true,
		depends_on: vec![previous_step_id],
		branch: None,
		loop_control: None,
	});
	steps
}

fn step_summary(goal: &str, action: &str) -> String {
	format!("Goal: {goal}\nStep: {action}")
}

#[cfg(test)]
mod tests {
	use roku_common_types::{RequestEnvelope, RequestId};
	use roku_planning_engine::PlanningMode;

	use super::*;
	use crate::planner::{AdaptiveTaskPlanner, TaskPlanner};

	fn decision(mode: PlanningMode, max_iterations: u8, max_branches: u8) -> PlanningDecision {
		PlanningDecision {
			mode,
			max_iterations,
			max_branches,
			hooks: Vec::new(),
		}
	}

	fn sample_request() -> RequestEnvelope {
		RequestEnvelope {
			request_id: RequestId("req-1".to_string()),
			session_id: "s1".to_string(),
			goal: "g".to_string(),
			planning_mode_hint: None,
			conversation_history: Vec::new(),
		}
	}

	#[test]
	fn task_decomposition_contains_branch_merge_dependencies() {
		let planner = AdaptiveTaskPlanner::default();
		let outline = planner.build_outline(
			&sample_request(),
			&decision(PlanningMode::TaskDecomposition, 4, 3),
		);

		let merge = outline
			.steps
			.iter()
			.find(|step| step.step_id == "merge-branches")
			.expect("merge step should exist");
		assert_eq!(merge.depends_on.len(), 3);
		assert_eq!(
			outline
				.steps
				.iter()
				.find(|step| step.step_id == "branch-data")
				.and_then(|step| step.branch.as_ref())
				.expect("branch metadata should exist")
				.branch_group,
			"task-decomposition"
		);
	}

	#[test]
	fn tree_search_contains_configured_branch_count() {
		let planner = AdaptiveTaskPlanner::default();
		let outline =
			planner.build_outline(&sample_request(), &decision(PlanningMode::TreeSearch, 4, 2));

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
		let planner = AdaptiveTaskPlanner::default();
		let outline = planner.build_outline(
			&sample_request(),
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
		assert_eq!(
			outline
				.steps
				.iter()
				.find(|step| step.step_id == "critique-1")
				.and_then(|step| step.loop_control.as_ref())
				.expect("loop metadata should exist")
				.max_iterations,
			2
		);
	}

	#[test]
	fn react_uses_single_direct_action_for_simple_chat_goal() {
		let planner = AdaptiveTaskPlanner::default();
		let mut request = sample_request();
		request.goal = "今天周几？".to_string();
		let outline = planner.build_outline(&request, &decision(PlanningMode::ReAct, 4, 1));

		assert_eq!(outline.steps.len(), 1);
		assert_eq!(outline.steps[0].step_id, "act-primary");
	}

	#[test]
	fn react_keeps_observe_then_act_for_complex_goal() {
		let planner = AdaptiveTaskPlanner::default();
		let mut request = sample_request();
		request.goal = "如何解决哥德巴赫猜想？".to_string();
		let outline = planner.build_outline(&request, &decision(PlanningMode::ReAct, 4, 1));

		assert_eq!(outline.steps.len(), 2);
		assert_eq!(outline.steps[0].step_id, "observe-context");
		assert_eq!(outline.steps[1].step_id, "act-primary");
	}

	#[test]
	fn build_skill_install_steps_uses_ensure_capability() {
		let steps = build_skill_install_steps(
			"Install the skill",
			"https://github.com/anthropics/skills/tree/main/skills/skill-creator",
			true,
		);

		assert_eq!(steps.len(), 1);
		assert_eq!(steps[0].step_id, "install-skill");
		assert_eq!(
			steps[0].required_capabilities,
			vec!["skill.ensure_installed".to_string()]
		);
		assert!(steps[0].summary.contains("if it is missing"));
	}

	#[test]
	fn build_explicit_skill_usage_steps_records_skill_name() {
		let steps = build_explicit_skill_usage_steps("Explain the eval workflow", "skill-creator");

		assert_eq!(steps.len(), 1);
		assert_eq!(steps[0].step_id, "use-installed-skill");
		assert_eq!(
			steps[0].required_capabilities,
			vec!["tool.invoke".to_string()]
		);
		assert!(steps[0].summary.contains("skill-creator"));
	}
}
