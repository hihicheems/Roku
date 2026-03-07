//! Planning strategy selection.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanningMode {
	ReAct,
	TaskDecomposition,
	TreeSearch,
	IterativeRefinement,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskLevel {
	Low,
	Medium,
	High,
}

#[derive(Debug, Clone)]
pub struct PlanningInput {
	pub complexity_score: u8,
	pub uncertainty_score: u8,
	pub risk_level: RiskLevel,
	pub budget_tokens: u64,
}

#[derive(Debug, Clone)]
pub struct PlanningDecision {
	pub mode: PlanningMode,
	pub max_iterations: u8,
}

pub trait StrategySelector {
	fn select(&self, input: &PlanningInput) -> PlanningDecision;
}

#[derive(Debug, Default)]
pub struct DefaultPlanningEngine;

impl StrategySelector for DefaultPlanningEngine {
	fn select(&self, input: &PlanningInput) -> PlanningDecision {
		let mode = if input.uncertainty_score >= 8 {
			PlanningMode::TreeSearch
		} else if input.complexity_score >= 7 {
			PlanningMode::TaskDecomposition
		} else if matches!(input.risk_level, RiskLevel::High) {
			PlanningMode::IterativeRefinement
		} else {
			PlanningMode::ReAct
		};

		let max_iterations = if input.budget_tokens < 2_000 { 2 } else { 4 };

		PlanningDecision {
			mode,
			max_iterations,
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn choose_tree_search_for_high_uncertainty() {
		let engine = DefaultPlanningEngine;
		let result = engine.select(&PlanningInput {
			complexity_score: 3,
			uncertainty_score: 9,
			risk_level: RiskLevel::Low,
			budget_tokens: 10_000,
		});
		assert_eq!(result.mode, PlanningMode::TreeSearch);
	}
}
