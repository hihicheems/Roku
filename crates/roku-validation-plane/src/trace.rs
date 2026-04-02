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

pub(crate) fn run_trace_checks(trace: &RuntimeLoopTrace, failures: &mut Vec<String>) {
	if trace.schema_version != RuntimeLoopTrace::schema_version() {
		failures.push(format!(
			"runtime loop trace uses unexpected schema `{}`",
			trace.schema_version
		));
	}
	if trace.step_count != trace.steps.len() {
		failures.push(format!(
			"runtime loop trace step_count={} does not match steps.len()={}",
			trace.step_count,
			trace.steps.len()
		));
	}
	if trace.steps.is_empty() {
		failures.push("runtime loop trace does not contain any recorded steps".to_string());
		return;
	}
	for step in &trace.steps {
		if step.visible_tools_before.is_empty() {
			failures.push(format!(
				"runtime loop trace step {} is missing visible_tools_before",
				step.step_index
			));
		}
		if step.visible_resources_before.is_none() {
			failures.push(format!(
				"runtime loop trace step {} is missing visible_resources_before",
				step.step_index
			));
		}
		if step.decision.reason.trim().is_empty() {
			failures.push(format!(
				"runtime loop trace step {} is missing decision.reason",
				step.step_index
			));
		}
		if step.decision.action == "call_tool" {
			if step.decision.tool_name.is_none() {
				failures.push(format!(
					"runtime loop trace step {} call_tool is missing tool_name",
					step.step_index
				));
			}
			if step.raw_tool_output.is_none() {
				failures.push(format!(
					"runtime loop trace step {} is missing raw_tool_output",
					step.step_index
				));
			}
			if step.observation.is_none() {
				failures.push(format!(
					"runtime loop trace step {} is missing observation",
					step.step_index
				));
			}
			if step.interpreted_observation.is_none() {
				failures.push(format!(
					"runtime loop trace step {} is missing interpreted_observation",
					step.step_index
				));
			}
		}
	}
	let last_step = trace.steps.last();
	if trace.final_outcome.terminal_action.is_none() {
		failures.push("runtime loop trace is missing final_outcome.terminal_action".to_string());
	} else if let (Some(last_step), Some(terminal_action)) =
		(last_step, trace.final_outcome.terminal_action.as_deref())
		&& last_step.decision.action != terminal_action
	{
		failures.push(format!(
			"runtime loop trace final_outcome.terminal_action `{terminal_action}` does not match last step action `{}`",
			last_step.decision.action
		));
	}
}

#[cfg(test)]
mod tests {
	use roku_common_types::{
		RuntimeLoopTrace, RuntimeLoopTraceDecision, RuntimeLoopTraceOutcome, RuntimeLoopTraceStep,
	};
	use serde_json::json;

	use super::run_trace_checks;

	fn sample_trace() -> RuntimeLoopTrace {
		RuntimeLoopTrace {
			schema_version: RuntimeLoopTrace::schema_version().to_string(),
			run_id: "loop-1".to_string(),
			status: "succeeded".to_string(),
			step_count: 2,
			steps: vec![
				RuntimeLoopTraceStep {
					step_index: 1,
					decision: RuntimeLoopTraceDecision {
						action: "call_tool".to_string(),
						tool_name: Some("command.run".to_string()),
						arguments: Some(json!({ "command": "pwd" })),
						reason: "run explicit command".to_string(),
						final_message: None,
					},
					visible_tools_before: vec![
						"command.run".to_string(),
						"general.execute".to_string(),
					],
					visible_resources_before: Some(vec![]),
					started_at: "2026-01-01T00:00:00Z".to_string(),
					finished_at: "2026-01-01T00:00:00Z".to_string(),
					tool_latency_ms: Some(10),
					raw_tool_output: Some(json!({"ok": true})),
					observation: Some(json!({"kind": "tool"})),
					interpreted_observation: Some(json!({"continue_allowed": true})),
					execution_trace: None,
					remaining_step_budget_after: 3,
					remaining_recovery_budget_after: 2,
					working_directory_after: "/workspace".to_string(),
				},
				RuntimeLoopTraceStep {
					step_index: 2,
					decision: RuntimeLoopTraceDecision {
						action: "final_answer".to_string(),
						tool_name: None,
						arguments: None,
						reason: "finish".to_string(),
						final_message: Some("/workspace".to_string()),
					},
					visible_tools_before: vec![
						"command.run".to_string(),
						"general.execute".to_string(),
					],
					visible_resources_before: Some(vec![]),
					started_at: "2026-01-01T00:00:01Z".to_string(),
					finished_at: "2026-01-01T00:00:01Z".to_string(),
					tool_latency_ms: None,
					raw_tool_output: None,
					observation: Some(json!({"kind": "final_message"})),
					interpreted_observation: None,
					execution_trace: None,
					remaining_step_budget_after: 2,
					remaining_recovery_budget_after: 2,
					working_directory_after: "/workspace".to_string(),
				},
			],
			final_outcome: RuntimeLoopTraceOutcome {
				status: "succeeded".to_string(),
				terminal_action: Some("final_answer".to_string()),
				final_message: Some("/workspace".to_string()),
			},
		}
	}

	#[test]
	fn accepts_runtime_loop_trace_with_required_fields() {
		let trace = sample_trace();
		let mut failures = Vec::new();
		run_trace_checks(&trace, &mut failures);
		assert!(failures.is_empty(), "unexpected failures: {failures:?}");
	}

	#[test]
	fn rejects_runtime_loop_trace_missing_tool_step_layers() {
		let mut trace = sample_trace();
		trace.steps[0].raw_tool_output = None;
		trace.steps[0].observation = None;
		trace.steps[0].interpreted_observation = None;
		let mut failures = Vec::new();
		run_trace_checks(&trace, &mut failures);
		assert!(
			failures
				.iter()
				.any(|failure| failure.contains("raw_tool_output"))
		);
		assert!(
			failures
				.iter()
				.any(|failure| failure.contains("observation"))
		);
		assert!(
			failures
				.iter()
				.any(|failure| failure.contains("interpreted_observation"))
		);
	}
}
