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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanningHook {
	ObserveAction,
	CritiqueRevise,
	BranchExpand,
}

#[derive(Debug, Clone)]
pub struct PlanningDecision {
	pub mode: PlanningMode,
	pub max_iterations: u8,
	pub max_branches: u8,
	pub hooks: Vec<PlanningHook>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanningStopReason {
	MaxIterations,
	BranchBudgetReached,
	TokenBudgetReached,
	Converged,
}

#[derive(Debug, Clone, Copy)]
pub struct PlanningLoopState {
	pub iteration: u8,
	pub expanded_branches: u8,
	pub consumed_tokens: u64,
	pub converged: bool,
}

impl PlanningDecision {
	pub fn stop_reason(
		&self,
		state: &PlanningLoopState,
		token_budget: u64,
	) -> Option<PlanningStopReason> {
		if state.converged {
			return Some(PlanningStopReason::Converged);
		}
		if state.iteration >= self.max_iterations {
			return Some(PlanningStopReason::MaxIterations);
		}
		if state.expanded_branches >= self.max_branches {
			return Some(PlanningStopReason::BranchBudgetReached);
		}
		if state.consumed_tokens >= token_budget {
			return Some(PlanningStopReason::TokenBudgetReached);
		}

		None
	}

	pub fn should_continue(&self, state: &PlanningLoopState, token_budget: u64) -> bool {
		self.stop_reason(state, token_budget).is_none()
	}
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

		let max_iterations = if input.budget_tokens < 2_000 {
			2
		} else if input.budget_tokens < 8_000 {
			4
		} else {
			6
		};
		let max_branches = match mode {
			PlanningMode::TreeSearch => {
				if input.budget_tokens < 8_000 {
					2
				} else {
					3
				}
			}
			PlanningMode::TaskDecomposition => 2,
			PlanningMode::IterativeRefinement => 1,
			PlanningMode::ReAct => 1,
		};
		let hooks = match mode {
			PlanningMode::ReAct => vec![PlanningHook::ObserveAction],
			PlanningMode::TaskDecomposition => vec![PlanningHook::ObserveAction],
			PlanningMode::TreeSearch => {
				vec![PlanningHook::BranchExpand, PlanningHook::CritiqueRevise]
			}
			PlanningMode::IterativeRefinement => vec![PlanningHook::CritiqueRevise],
		};

		PlanningDecision {
			mode,
			max_iterations,
			max_branches,
			hooks,
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
		assert_eq!(result.max_branches, 3);
		assert!(result.hooks.contains(&PlanningHook::BranchExpand));
	}

	#[test]
	fn choose_iterative_refinement_for_high_risk_simple_task() {
		let engine = DefaultPlanningEngine;
		let result = engine.select(&PlanningInput {
			complexity_score: 3,
			uncertainty_score: 4,
			risk_level: RiskLevel::High,
			budget_tokens: 3_000,
		});

		assert_eq!(result.mode, PlanningMode::IterativeRefinement);
		assert_eq!(result.max_branches, 1);
		assert!(result.hooks.contains(&PlanningHook::CritiqueRevise));
	}

	#[test]
	fn stop_reason_detects_iteration_limit() {
		let decision = PlanningDecision {
			mode: PlanningMode::ReAct,
			max_iterations: 3,
			max_branches: 1,
			hooks: vec![PlanningHook::ObserveAction],
		};
		let state = PlanningLoopState {
			iteration: 3,
			expanded_branches: 0,
			consumed_tokens: 120,
			converged: false,
		};

		assert_eq!(
			decision.stop_reason(&state, 1_000),
			Some(PlanningStopReason::MaxIterations)
		);
		assert!(!decision.should_continue(&state, 1_000));
	}
}
