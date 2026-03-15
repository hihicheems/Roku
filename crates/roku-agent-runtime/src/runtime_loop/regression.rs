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

use roku_common_types::RuntimeLoopTrace;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::runtime_loop::{RuntimeLoopTraceCheckReport, check_runtime_loop_trace};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegressionSuiteKind {
	Confusion,
	Boundary,
	OutputInterpretation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterpretedFlagExpectation {
	pub field: String,
	pub expected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RuntimeLoopRegressionExpectation {
	#[serde(default)]
	pub expected_tool: Option<String>,
	#[serde(default)]
	pub forbidden_tools: Vec<String>,
	#[serde(default)]
	pub expected_terminal_action: Option<String>,
	#[serde(default)]
	pub expected_error_type: Option<String>,
	#[serde(default)]
	pub interpreted_flags: Vec<InterpretedFlagExpectation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeLoopRegressionCaseReport {
	pub case_id: String,
	pub suite: RegressionSuiteKind,
	pub trace_check: RuntimeLoopTraceCheckReport,
	pub passed: bool,
	pub issues: Vec<String>,
}

pub fn evaluate_runtime_loop_regression_case(
	case_id: impl Into<String>,
	suite: RegressionSuiteKind,
	trace: &RuntimeLoopTrace,
	expectation: &RuntimeLoopRegressionExpectation,
) -> RuntimeLoopRegressionCaseReport {
	let trace_check = check_runtime_loop_trace(trace);
	let first_tool_step = trace
		.steps
		.iter()
		.find(|step| step.decision.action == "call_tool");
	let selected_tool = first_tool_step.and_then(|step| step.decision.tool_name.as_deref());
	let last_error_type =
		first_tool_step.and_then(|step| tool_step_error_type(step.observation.as_ref()));
	let mut issues = trace_check.issues.clone();

	if let Some(expected_tool) = expectation.expected_tool.as_deref()
		&& selected_tool != Some(expected_tool)
	{
		issues.push(format!(
			"expected first tool `{expected_tool}`, but observed `{}`",
			selected_tool.unwrap_or("none")
		));
	}
	for forbidden_tool in &expectation.forbidden_tools {
		if trace.steps.iter().any(|step| {
			step.decision.action == "call_tool"
				&& step.decision.tool_name.as_deref() == Some(forbidden_tool.as_str())
		}) {
			issues.push(format!(
				"trace unexpectedly used forbidden tool `{forbidden_tool}`"
			));
		}
	}
	if let Some(expected_terminal_action) = expectation.expected_terminal_action.as_deref()
		&& trace.final_outcome.terminal_action.as_deref() != Some(expected_terminal_action)
	{
		issues.push(format!(
			"expected terminal action `{expected_terminal_action}`, but observed `{}`",
			trace
				.final_outcome
				.terminal_action
				.as_deref()
				.unwrap_or("none")
		));
	}
	if let Some(expected_error_type) = expectation.expected_error_type.as_deref()
		&& last_error_type != Some(expected_error_type)
	{
		issues.push(format!(
			"expected tool error_type `{expected_error_type}`, but observed `{}`",
			last_error_type.unwrap_or("none")
		));
	}
	for flag in &expectation.interpreted_flags {
		let actual = first_tool_step
			.and_then(|step| interpreted_flag(step.interpreted_observation.as_ref(), &flag.field));
		if actual != Some(flag.expected) {
			issues.push(format!(
				"expected interpreted flag `{}`={}, but observed `{}`",
				flag.field,
				flag.expected,
				actual
					.map(|value| value.to_string())
					.unwrap_or_else(|| "none".to_string())
			));
		}
	}

	RuntimeLoopRegressionCaseReport {
		case_id: case_id.into(),
		suite,
		passed: issues.is_empty(),
		trace_check,
		issues,
	}
}

fn tool_step_error_type(observation: Option<&Value>) -> Option<&str> {
	observation?.get("error_type")?.as_str()
}

fn interpreted_flag(interpreted: Option<&Value>, field: &str) -> Option<bool> {
	interpreted?.get(field)?.as_bool()
}
