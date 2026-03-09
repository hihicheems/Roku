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

use roku_common_types::{PlanOutline, RequestEnvelope};
use roku_planning_engine::{PlanningDecision, PlanningMode};
use roku_skill_registry::SkillRegistry;

use crate::shortcut::{ShortcutIntent, SkillShortcutResolver};
use crate::strategies::{
	build_decomposition_steps, build_explicit_skill_usage_steps, build_react_steps,
	build_refinement_steps, build_skill_install_steps, build_tree_search_steps,
};

pub trait TaskPlanner {
	fn build_outline(&self, request: &RequestEnvelope, decision: &PlanningDecision) -> PlanOutline;
}

#[derive(Clone)]
pub struct AdaptiveTaskPlanner {
	shortcut_resolver: SkillShortcutResolver,
}

impl AdaptiveTaskPlanner {
	pub fn with_skill_registry(skill_registry: SkillRegistry) -> Self {
		Self {
			shortcut_resolver: SkillShortcutResolver::new(skill_registry),
		}
	}
}

impl Default for AdaptiveTaskPlanner {
	fn default() -> Self {
		Self::with_skill_registry(SkillRegistry::disabled())
	}
}

impl TaskPlanner for AdaptiveTaskPlanner {
	fn build_outline(&self, request: &RequestEnvelope, decision: &PlanningDecision) -> PlanOutline {
		if let Some(shortcut) = self.shortcut_resolver.resolve_without_classifier(request) {
			return PlanOutline {
				goal: request.goal.clone(),
				steps: shortcut_steps(&request.goal, shortcut),
			};
		}

		let steps = match decision.mode {
			PlanningMode::ReAct => build_react_steps(&request.goal, decision),
			PlanningMode::TaskDecomposition => build_decomposition_steps(&request.goal, decision),
			PlanningMode::TreeSearch => build_tree_search_steps(&request.goal, decision),
			PlanningMode::IterativeRefinement => build_refinement_steps(&request.goal, decision),
		};

		PlanOutline {
			goal: request.goal.clone(),
			steps,
		}
	}
}

fn shortcut_steps(goal: &str, shortcut: ShortcutIntent) -> Vec<roku_common_types::PlanStep> {
	match shortcut {
		ShortcutIntent::EnsureSkillInstalled {
			source_url,
			if_missing,
		} => build_skill_install_steps(goal, &source_url, if_missing),
		ShortcutIntent::UseInstalledSkill { skill_name } => {
			build_explicit_skill_usage_steps(goal, &skill_name)
		}
	}
}
