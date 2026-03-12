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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoopState {
	pub run_id: String,
	pub request_id: String,
	pub session_id: String,
	pub route_decision: RouteDecision,
	pub status: LoopStatus,
	pub step_index: u32,
	pub remaining_step_budget: u32,
	pub remaining_recovery_budget: u32,
	pub working_directory: String,
	pub visible_tools: Vec<String>,
	pub history: Vec<StepRecord>,
	pub last_observation: Option<ToolObservation>,
}

impl LoopState {
	pub fn new(run_id: impl Into<String>, context: &LoopContext) -> Self {
		Self {
			run_id: run_id.into(),
			request_id: context.request_id.clone(),
			session_id: context.session_id.clone(),
			route_decision: context.route_decision.clone(),
			status: LoopStatus::LoopRunning,
			step_index: 0,
			remaining_step_budget: 4,
			remaining_recovery_budget: 2,
			working_directory: context.working_directory.clone(),
			visible_tools: context.visible_tools.clone(),
			history: Vec::new(),
			last_observation: context.last_observation.clone(),
		}
	}

	pub fn record_step(&mut self, step: StepRecord) {
		self.step_index = step.step_index;
		self.remaining_step_budget = step.remaining_step_budget_after;
		self.remaining_recovery_budget = step.remaining_recovery_budget_after;
		self.working_directory = step.working_directory_after.clone();
		self.history.push(step);
	}
}
