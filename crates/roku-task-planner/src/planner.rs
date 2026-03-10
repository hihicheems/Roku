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
use roku_resource_catalog::ResourceCatalog;

use crate::selection::{ResourceSelectionEngine, SelectionRoute};
use crate::strategies::{
	PlannerToolbox, build_conversation_steps, build_decomposition_steps, build_react_steps,
	build_refinement_steps, build_selected_skill_advisory_steps,
	build_selected_skill_executable_steps, build_selected_tool_steps, build_skill_install_steps,
	build_tree_search_steps,
};

pub trait TaskPlanner {
	fn build_outline(&self, request: &RequestEnvelope, decision: &PlanningDecision) -> PlanOutline;
}

#[derive(Clone)]
pub struct AdaptiveTaskPlanner {
	selector: ResourceSelectionEngine,
	toolbox: PlannerToolbox,
}

impl AdaptiveTaskPlanner {
	pub fn with_resource_catalog(catalog: ResourceCatalog) -> Self {
		let toolbox = PlannerToolbox::from_catalog(&catalog);
		Self {
			selector: ResourceSelectionEngine::new(catalog),
			toolbox,
		}
	}
}

impl Default for AdaptiveTaskPlanner {
	fn default() -> Self {
		Self::with_resource_catalog(ResourceCatalog::default())
	}
}

impl TaskPlanner for AdaptiveTaskPlanner {
	fn build_outline(&self, request: &RequestEnvelope, decision: &PlanningDecision) -> PlanOutline {
		let steps = match self.selector.select_without_llm(request) {
			SelectionRoute::Conversation => build_conversation_steps(&request.goal),
			SelectionRoute::InstallSkill {
				source_url,
				if_missing,
			} => build_skill_install_steps(&request.goal, &source_url, if_missing, &self.toolbox),
			SelectionRoute::UseSkillAdvisory { selector } => {
				build_selected_skill_advisory_steps(&request.goal, selector)
			}
			SelectionRoute::UseSkillExecutable { selector } => {
				build_selected_skill_executable_steps(&request.goal, selector, &self.toolbox)
			}
			SelectionRoute::UseTools { selectors } => {
				build_selected_tool_steps(&request.goal, &selectors)
			}
			SelectionRoute::PlannerDefault => match decision.mode {
				PlanningMode::ReAct => build_react_steps(&request.goal, decision, &self.toolbox),
				PlanningMode::TaskDecomposition => {
					build_decomposition_steps(&request.goal, decision, &self.toolbox)
				}
				PlanningMode::TreeSearch => {
					build_tree_search_steps(&request.goal, decision, &self.toolbox)
				}
				PlanningMode::IterativeRefinement => {
					build_refinement_steps(&request.goal, decision, &self.toolbox)
				}
			},
		};

		PlanOutline {
			goal: request.goal.clone(),
			steps,
		}
	}
}
