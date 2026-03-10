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
use roku_llm_adapter::LlmRouter;
use roku_planning_engine::PlanningDecision;
use roku_resource_catalog::ResourceCatalog;

use crate::planner::{AdaptiveTaskPlanner, TaskPlanner};
use crate::selection::{ResourceSelectionEngine, SelectionRoute};
use crate::strategies::{
	PlannerToolbox, build_conversation_steps, build_selected_skill_advisory_steps,
	build_selected_skill_executable_steps, build_selected_tool_steps, build_skill_install_steps,
};

pub struct LlmTaskPlanner {
	router: LlmRouter,
	selector: ResourceSelectionEngine,
	fallback: AdaptiveTaskPlanner,
	toolbox: PlannerToolbox,
}

impl LlmTaskPlanner {
	pub fn new(router: LlmRouter) -> Self {
		Self::with_resource_catalog(router, ResourceCatalog::default())
	}

	pub fn with_resource_catalog(router: LlmRouter, catalog: ResourceCatalog) -> Self {
		let toolbox = PlannerToolbox::from_catalog(&catalog);
		Self {
			router,
			selector: ResourceSelectionEngine::new(catalog.clone()),
			fallback: AdaptiveTaskPlanner::with_resource_catalog(catalog),
			toolbox,
		}
	}
}

impl TaskPlanner for LlmTaskPlanner {
	fn build_outline(&self, request: &RequestEnvelope, decision: &PlanningDecision) -> PlanOutline {
		match self.selector.select_with_llm(request, &self.router) {
			SelectionRoute::Conversation => PlanOutline {
				goal: request.goal.clone(),
				steps: build_conversation_steps(&request.goal),
			},
			SelectionRoute::InstallSkill {
				source_url,
				if_missing,
			} => PlanOutline {
				goal: request.goal.clone(),
				steps: build_skill_install_steps(
					&request.goal,
					&source_url,
					if_missing,
					&self.toolbox,
				),
			},
			SelectionRoute::UseSkillAdvisory { selector } => PlanOutline {
				goal: request.goal.clone(),
				steps: build_selected_skill_advisory_steps(&request.goal, selector),
			},
			SelectionRoute::UseSkillExecutable { selector } => PlanOutline {
				goal: request.goal.clone(),
				steps: build_selected_skill_executable_steps(
					&request.goal,
					selector,
					&self.toolbox,
				),
			},
			SelectionRoute::UseTools { selectors } => PlanOutline {
				goal: request.goal.clone(),
				steps: build_selected_tool_steps(&request.goal, &selectors),
			},
			SelectionRoute::PlannerDefault => self.fallback.build_outline(request, decision),
		}
	}
}
