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
			PlanningMode::ReAct => build_react_steps(decision),
			PlanningMode::TaskDecomposition => build_decomposition_steps(decision),
			PlanningMode::TreeSearch => build_tree_search_steps(decision),
			PlanningMode::IterativeRefinement => build_refinement_steps(decision),
		};

		PlanOutline {
			goal: request.goal.clone(),
			steps,
		}
	}
}
