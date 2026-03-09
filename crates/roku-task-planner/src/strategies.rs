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

use roku_common_types::{PlanBranch, PlanLoopControl, PlanStep, ResourceSelector};
use roku_planning_engine::PlanningDecision;
use roku_resource_catalog::{ResourceCatalog, ResourceKind};

#[derive(Debug, Clone)]
pub(crate) struct PlannerToolbox {
	research_tool: String,
	data_tool: String,
	review_tool: String,
	skill_install_tool: String,
}

impl PlannerToolbox {
	pub(crate) fn from_catalog(catalog: &ResourceCatalog) -> Self {
		Self {
			research_tool: catalog_tool_name(catalog, "research", "research.synthesize"),
			data_tool: catalog_tool_name(catalog, "data", "data.execute"),
			review_tool: catalog_tool_name(catalog, "review", "review.assess"),
			skill_install_tool: catalog_tool_name(
				catalog,
				"skill_install",
				"skill.ensure_installed",
			),
		}
	}
}

pub(crate) fn build_conversation_steps(goal: &str) -> Vec<PlanStep> {
	vec![step(
		"conversation",
		goal,
		"Answer directly without external resources",
	)]
}

pub(crate) fn build_skill_install_steps(
	goal: &str,
	source_url: &str,
	if_missing: bool,
	toolbox: &PlannerToolbox,
) -> Vec<PlanStep> {
	vec![resource_step(
		"install-skill",
		goal,
		&format!(
			"Ensure requested skill from {source_url} is installed{}",
			if if_missing { " if it is missing" } else { "" }
		),
		vec![ResourceSelector::tool(&toolbox.skill_install_tool)],
	)]
}

pub(crate) fn build_selected_skill_steps(goal: &str, selector: ResourceSelector) -> Vec<PlanStep> {
	vec![resource_step(
		"use-installed-skill",
		goal,
		&format!("Use selected skill `{}` for this request", selector.name()),
		vec![selector],
	)]
}

pub(crate) fn build_selected_tool_steps(
	goal: &str,
	selectors: &[ResourceSelector],
) -> Vec<PlanStep> {
	selectors
		.iter()
		.enumerate()
		.map(|(index, selector)| {
			resource_step(
				&format!("use-tool-{}", index + 1),
				goal,
				&format!("Use selected tool `{}`", selector.name()),
				vec![selector.clone()],
			)
		})
		.collect()
}

pub(crate) fn build_react_steps(
	goal: &str,
	decision: &PlanningDecision,
	toolbox: &PlannerToolbox,
) -> Vec<PlanStep> {
	if should_use_direct_react_action(goal) {
		return vec![step("act-primary", goal, "Execute primary action")];
	}

	vec![
		resource_step(
			"observe-context",
			goal,
			&format!(
				"Observe context with max {} iteration(s)",
				decision.max_iterations
			),
			vec![ResourceSelector::tool(&toolbox.research_tool)],
		),
		step_with_dep(
			"act-primary",
			goal,
			"Execute primary action",
			vec!["observe-context".to_string()],
		),
	]
}

pub(crate) fn build_decomposition_steps(
	goal: &str,
	decision: &PlanningDecision,
	toolbox: &PlannerToolbox,
) -> Vec<PlanStep> {
	let mut steps = vec![
		resource_step(
			"decompose-goal",
			goal,
			&format!(
				"Decompose goal into branch tasks (max branches = {})",
				decision.max_branches
			),
			vec![ResourceSelector::tool(&toolbox.research_tool)],
		),
		branched_resource_step(
			"branch-data",
			goal,
			"Run data branch",
			vec![ResourceSelector::tool(&toolbox.data_tool)],
			"task-decomposition",
			"data",
		),
		branched_resource_step(
			"branch-analysis",
			goal,
			"Run analysis branch",
			vec![ResourceSelector::tool(&toolbox.research_tool)],
			"task-decomposition",
			"analysis",
		),
	];
	steps[1].depends_on = vec!["decompose-goal".to_string()];
	steps[2].depends_on = vec!["decompose-goal".to_string()];

	if decision.max_branches >= 3 {
		let mut risk = branched_resource_step(
			"branch-risk",
			goal,
			"Run risk review branch",
			vec![ResourceSelector::tool(&toolbox.review_tool)],
			"task-decomposition",
			"risk",
		);
		risk.depends_on = vec!["decompose-goal".to_string()];
		steps.push(risk);
	}

	let mut merge_dependencies = vec!["branch-data".to_string(), "branch-analysis".to_string()];
	if decision.max_branches >= 3 {
		merge_dependencies.push("branch-risk".to_string());
	}
	steps.push(step_with_dep(
		"merge-branches",
		goal,
		"Merge decomposition branch results",
		merge_dependencies,
	));
	steps
}

pub(crate) fn build_tree_search_steps(
	goal: &str,
	decision: &PlanningDecision,
	toolbox: &PlannerToolbox,
) -> Vec<PlanStep> {
	let mut steps = vec![resource_step(
		"search-root",
		goal,
		"Generate tree-search seed hypotheses",
		vec![ResourceSelector::tool(&toolbox.research_tool)],
	)];

	let mut branch_ids = Vec::new();
	for branch in 1..=decision.max_branches {
		let branch_id = format!("search-branch-{branch}");
		let mut step = branched_resource_step(
			&branch_id,
			goal,
			&format!("Explore tree branch {branch}"),
			vec![ResourceSelector::tool(&toolbox.research_tool)],
			"tree-search",
			&format!("branch-{branch}"),
		);
		step.depends_on = vec!["search-root".to_string()];
		steps.push(step);
		branch_ids.push(branch_id);
	}
	steps.push(resource_step_with_dep(
		"search-evaluate",
		goal,
		"Evaluate candidate branches",
		vec![ResourceSelector::tool(&toolbox.review_tool)],
		branch_ids,
	));
	steps
}

pub(crate) fn build_refinement_steps(
	goal: &str,
	decision: &PlanningDecision,
	toolbox: &PlannerToolbox,
) -> Vec<PlanStep> {
	let draft_id = "draft-v1".to_string();
	let mut steps = vec![step(&draft_id, goal, "Generate initial draft")];
	let mut previous_step_id = draft_id;
	for iteration in 1..=decision.max_iterations {
		let critique_id = format!("critique-{iteration}");
		let improve_id = format!("improve-{iteration}");
		steps.push(looped_resource_step(
			&critique_id,
			goal,
			&format!("Critique iteration {iteration}"),
			vec![ResourceSelector::tool(&toolbox.review_tool)],
			vec![previous_step_id.clone()],
			iteration,
			decision.max_iterations,
		));
		steps.push(looped_step(
			&improve_id,
			goal,
			&format!("Improve draft from critique {iteration}"),
			vec![critique_id],
			iteration,
			decision.max_iterations,
		));
		previous_step_id = improve_id;
	}

	let mut final_step = resource_step_with_dep(
		"final-review",
		goal,
		"Produce refinement final answer",
		vec![ResourceSelector::tool(&toolbox.review_tool)],
		vec![previous_step_id],
	);
	final_step.requires_approval = true;
	steps.push(final_step);
	steps
}

fn step(step_id: &str, goal: &str, action: &str) -> PlanStep {
	PlanStep {
		step_id: step_id.to_string(),
		summary: step_summary(goal, action),
		resource_selectors: Vec::new(),
		required_capabilities: Vec::new(),
		requires_approval: false,
		depends_on: Vec::new(),
		branch: None,
		loop_control: None,
	}
}

fn step_with_dep(step_id: &str, goal: &str, action: &str, depends_on: Vec<String>) -> PlanStep {
	let mut step = step(step_id, goal, action);
	step.depends_on = depends_on;
	step
}

fn catalog_tool_name(catalog: &ResourceCatalog, role: &str, fallback: &str) -> String {
	catalog
		.entries()
		.iter()
		.find(|entry| entry.kind == ResourceKind::Tool && entry.role.as_deref() == Some(role))
		.map(|entry| entry.name.clone())
		.unwrap_or_else(|| fallback.to_string())
}

fn resource_step(
	step_id: &str,
	goal: &str,
	action: &str,
	resource_selectors: Vec<ResourceSelector>,
) -> PlanStep {
	let mut step = step(step_id, goal, action);
	step.resource_selectors = resource_selectors;
	step
}

fn resource_step_with_dep(
	step_id: &str,
	goal: &str,
	action: &str,
	resource_selectors: Vec<ResourceSelector>,
	depends_on: Vec<String>,
) -> PlanStep {
	let mut step = resource_step(step_id, goal, action, resource_selectors);
	step.depends_on = depends_on;
	step
}

fn branched_resource_step(
	step_id: &str,
	goal: &str,
	action: &str,
	resource_selectors: Vec<ResourceSelector>,
	branch_group: &str,
	branch_label: &str,
) -> PlanStep {
	let mut step = resource_step(step_id, goal, action, resource_selectors);
	step.branch = Some(PlanBranch {
		branch_group: branch_group.to_string(),
		branch_label: branch_label.to_string(),
	});
	step
}

fn looped_step(
	step_id: &str,
	goal: &str,
	action: &str,
	depends_on: Vec<String>,
	iteration: u8,
	max_iterations: u8,
) -> PlanStep {
	let mut step = step_with_dep(step_id, goal, action, depends_on);
	step.loop_control = Some(PlanLoopControl {
		loop_id: "iterative-refinement".to_string(),
		iteration,
		max_iterations,
	});
	step
}

fn looped_resource_step(
	step_id: &str,
	goal: &str,
	action: &str,
	resource_selectors: Vec<ResourceSelector>,
	depends_on: Vec<String>,
	iteration: u8,
	max_iterations: u8,
) -> PlanStep {
	let mut step = resource_step_with_dep(step_id, goal, action, resource_selectors, depends_on);
	step.loop_control = Some(PlanLoopControl {
		loop_id: "iterative-refinement".to_string(),
		iteration,
		max_iterations,
	});
	step
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

fn step_summary(goal: &str, action: &str) -> String {
	format!("Goal: {goal}\nStep: {action}")
}
