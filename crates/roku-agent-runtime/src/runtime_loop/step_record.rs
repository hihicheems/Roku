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
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::runtime_loop::StepObservation;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepAction {
	CallTool,
	AskUser,
	FinalAnswer,
	Fail,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepRecord {
	pub step_index: u32,
	pub action: StepAction,
	pub tool_name: Option<String>,
	pub decision_reason: String,
	pub started_at: String,
	pub finished_at: String,
	pub tool_latency_ms: Option<u64>,
	pub observation: Option<StepObservation>,
	pub remaining_step_budget_after: u32,
	pub remaining_recovery_budget_after: u32,
	pub working_directory_after: String,
}

impl StepRecord {
	pub fn tool_call(
		step_index: u32,
		tool_name: impl Into<String>,
		decision_reason: impl Into<String>,
		observation: StepObservation,
		tool_latency_ms: Option<u64>,
		remaining_step_budget_after: u32,
		remaining_recovery_budget_after: u32,
		working_directory_after: impl Into<String>,
	) -> Self {
		let timestamp = now_rfc3339();
		Self {
			step_index,
			action: StepAction::CallTool,
			tool_name: Some(tool_name.into()),
			decision_reason: decision_reason.into(),
			started_at: timestamp.clone(),
			finished_at: timestamp,
			tool_latency_ms,
			observation: Some(observation),
			remaining_step_budget_after,
			remaining_recovery_budget_after,
			working_directory_after: working_directory_after.into(),
		}
	}

	pub fn terminal(
		step_index: u32,
		action: StepAction,
		decision_reason: impl Into<String>,
		observation: Option<StepObservation>,
		remaining_step_budget_after: u32,
		remaining_recovery_budget_after: u32,
		working_directory_after: impl Into<String>,
	) -> Self {
		let timestamp = now_rfc3339();
		Self {
			step_index,
			action,
			tool_name: None,
			decision_reason: decision_reason.into(),
			started_at: timestamp.clone(),
			finished_at: timestamp,
			tool_latency_ms: None,
			observation,
			remaining_step_budget_after,
			remaining_recovery_budget_after,
			working_directory_after: working_directory_after.into(),
		}
	}
}

fn now_rfc3339() -> String {
	OffsetDateTime::now_utc()
		.format(&Rfc3339)
		.unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}
