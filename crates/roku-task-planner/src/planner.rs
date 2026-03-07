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
