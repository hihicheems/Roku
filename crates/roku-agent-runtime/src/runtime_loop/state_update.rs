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

use crate::runtime_loop::{LoopState, ToolObservation};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InterpretedObservation {
	pub raw_observation: ToolObservation,
	pub continue_allowed: bool,
	pub remaining_step_budget: u32,
	pub remaining_recovery_budget: u32,
	pub new_working_directory: Option<String>,
	pub visible_tools: Vec<String>,
}

pub fn interpret_observation(
	state: &LoopState,
	raw_observation: ToolObservation,
	new_working_directory: Option<String>,
) -> InterpretedObservation {
	InterpretedObservation {
		continue_allowed: !raw_observation.terminal && state.remaining_step_budget > 0,
		remaining_step_budget: state.remaining_step_budget.saturating_sub(1),
		remaining_recovery_budget: state.remaining_recovery_budget,
		new_working_directory,
		visible_tools: state.visible_tools.clone(),
		raw_observation,
	}
}
