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

impl DefaultPlanningEngine {
	pub fn decision_for_mode(&self, mode: PlanningMode, input: &PlanningInput) -> PlanningDecision {
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

		self.decision_for_mode(mode, input)
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
	fn choose_task_decomposition_for_high_complexity() {
		let engine = DefaultPlanningEngine;
		let result = engine.select(&PlanningInput {
			complexity_score: 8,
			uncertainty_score: 3,
			risk_level: RiskLevel::Low,
			budget_tokens: 9_000,
		});

		assert_eq!(result.mode, PlanningMode::TaskDecomposition);
		assert_eq!(result.max_branches, 2);
		assert!(result.hooks.contains(&PlanningHook::ObserveAction));
	}

	#[test]
	fn choose_react_for_low_risk_low_complexity_goal() {
		let engine = DefaultPlanningEngine;
		let result = engine.select(&PlanningInput {
			complexity_score: 2,
			uncertainty_score: 2,
			risk_level: RiskLevel::Low,
			budget_tokens: 1_500,
		});

		assert_eq!(result.mode, PlanningMode::ReAct);
		assert_eq!(result.max_iterations, 2);
		assert_eq!(result.max_branches, 1);
		assert_eq!(result.hooks, vec![PlanningHook::ObserveAction]);
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
	fn forced_mode_preserves_tree_search_budget_rules() {
		let engine = DefaultPlanningEngine;
		let result = engine.decision_for_mode(
			PlanningMode::TreeSearch,
			&PlanningInput {
				complexity_score: 1,
				uncertainty_score: 1,
				risk_level: RiskLevel::Low,
				budget_tokens: 3_000,
			},
		);

		assert_eq!(result.mode, PlanningMode::TreeSearch);
		assert_eq!(result.max_branches, 2);
		assert!(result.hooks.contains(&PlanningHook::BranchExpand));
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
