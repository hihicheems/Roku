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

use crate::strategies::{
	build_decomposition_steps, build_react_steps, build_refinement_steps, build_tree_search_steps,
};

pub trait TaskPlanner {
	fn build_outline(&self, request: &RequestEnvelope, decision: &PlanningDecision) -> PlanOutline;
}

#[derive(Debug, Default)]
pub struct AdaptiveTaskPlanner;

impl TaskPlanner for AdaptiveTaskPlanner {
	fn build_outline(&self, request: &RequestEnvelope, decision: &PlanningDecision) -> PlanOutline {
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
