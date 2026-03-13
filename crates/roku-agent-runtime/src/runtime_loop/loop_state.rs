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

use roku_common_types::ResourceSelector;
use serde::{Deserialize, Serialize};

use crate::router::RouteDecision;
use crate::runtime_loop::{LoopContext, StepRecord, ToolObservation};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoopStatus {
	Received,
	Classified,
	LoopRunning,
	AwaitingUser,
	Succeeded,
	Failed,
	Stopped,
}

/// Source-of-truth runtime state for a single ReAct loop run.
///
/// ## Why this exists
/// `LoopState` is the mutable state machine for runtime loop execution. It captures the current
/// goal, routing seed, budgets, visible tools, replay history, and the latest grounded
/// observation so each next-step decision can be derived from one canonical state object.
///
/// ## Fields
/// - `run_id`: Stable identifier for this loop instance.
/// - `request_id`: Original request identifier.
/// - `session_id`: Session identifier used for ask-user resume semantics.
/// - `goal`: User-visible goal for the current run.
/// - `route_decision`: Initial route seed that constrains the loop.
/// - `status`: Current lifecycle state of the loop.
/// - `step_index`: Index of the latest recorded step.
/// - `remaining_step_budget`: Remaining loop steps before forced termination.
/// - `remaining_recovery_budget`: Remaining recovery opportunities after non-terminal errors.
/// - `working_directory`: Current working directory after prior steps.
/// - `visible_tools`: Tools visible for the next decision round.
/// - `bound_resources`: Resources already bound to the loop.
/// - `history`: Recorded step facts for replay and context projection.
/// - `last_observation`: Latest grounded tool observation, if any.
///
/// ## Invariants
/// - `history` is append-only within a run.
/// - `last_observation` must reflect the most recent tool observation recorded in `history`.
/// - `visible_tools` may be recomputed between rounds, but the current round must treat this
///   field as the active visibility truth.
///
/// ## Non-Goals
/// - `LoopState` is not the prompt projection passed directly to the model.
/// - `LoopState` does not encode a multi-step plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoopState {
	pub run_id: String,
	pub request_id: String,
	pub session_id: String,
	pub goal: String,
	pub route_decision: RouteDecision,
	pub status: LoopStatus,
	pub step_index: u32,
	pub remaining_step_budget: u32,
	pub remaining_recovery_budget: u32,
	pub working_directory: String,
	pub visible_tools: Vec<String>,
	pub bound_resources: Vec<ResourceSelector>,
	pub history: Vec<StepRecord>,
	pub last_observation: Option<ToolObservation>,
}

impl LoopState {
	pub fn new(run_id: impl Into<String>, context: &LoopContext) -> Self {
		Self {
			run_id: run_id.into(),
			request_id: context.request_id.clone(),
			session_id: context.session_id.clone(),
			goal: context.goal.clone(),
			route_decision: context.route_decision.clone(),
			status: LoopStatus::LoopRunning,
			step_index: 0,
			remaining_step_budget: 4,
			remaining_recovery_budget: 2,
			working_directory: context.working_directory.clone(),
			visible_tools: context.visible_tools.clone(),
			bound_resources: context.bound_resources.clone(),
			history: Vec::new(),
			last_observation: context.last_observation.clone(),
		}
	}

	pub fn record_step(&mut self, step: StepRecord) {
		self.step_index = step.step_index;
		self.remaining_step_budget = step.remaining_step_budget_after;
		self.remaining_recovery_budget = step.remaining_recovery_budget_after;
		self.working_directory = step.working_directory_after.clone();
		match &step.observation {
			Some(crate::runtime_loop::StepObservation::Tool(observation)) => {
				self.last_observation = Some(observation.clone());
			}
			Some(crate::runtime_loop::StepObservation::AskUser { .. })
			| Some(crate::runtime_loop::StepObservation::FinalMessage { .. })
			| None => {}
		}
		self.status = match step.action {
			crate::runtime_loop::StepAction::CallTool => LoopStatus::LoopRunning,
			crate::runtime_loop::StepAction::AskUser => LoopStatus::AwaitingUser,
			crate::runtime_loop::StepAction::FinalAnswer => LoopStatus::Succeeded,
			crate::runtime_loop::StepAction::Fail => LoopStatus::Failed,
		};
		self.history.push(step);
	}
}
