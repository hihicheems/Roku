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

use std::collections::HashSet;

use roku_common_types::{
	RuntimeLoopExecutionTraceStage, RuntimeLoopTrace, RuntimeLoopTraceDecision,
	RuntimeLoopTraceOutcome, RuntimeLoopTraceStep,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::runtime_loop::execution_trace::project_execution_traces;
use crate::runtime_loop::{LoopState, LoopStatus, StepAction, StepObservation};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeLoopTraceCheckReport {
	pub schema_version: String,
	pub step_count_matches: bool,
	pub decisions_captured: bool,
	pub visible_tools_captured: bool,
	pub visible_resources_captured: bool,
	pub tool_steps_capture_raw_output: bool,
	pub tool_steps_capture_normalized_observation: bool,
	pub tool_steps_capture_interpreted_observation: bool,
	pub command_execution_traces_captured: bool,
	pub execution_trace_stage_order_valid: bool,
	pub execution_trace_digests_aligned: bool,
	pub terminal_outcome_captured: bool,
	pub final_outcome_matches_last_step: bool,
	pub issues: Vec<String>,
}

pub fn runtime_loop_trace(loop_state: &LoopState) -> RuntimeLoopTrace {
	let execution_traces = project_execution_traces(&loop_state.history);

	RuntimeLoopTrace {
		schema_version: RuntimeLoopTrace::schema_version().to_string(),
		run_id: loop_state.run_id.clone(),
		status: loop_status_label(loop_state.status).to_string(),
		step_count: loop_state.history.len(),
		steps: loop_state
			.history
			.iter()
			.zip(execution_traces)
			.map(|(step, execution_trace)| RuntimeLoopTraceStep {
				step_index: step.step_index,
				decision: RuntimeLoopTraceDecision {
					action: step_action_label(step.action).to_string(),
					tool_name: step.decision.tool_name.clone(),
					arguments: step.decision.arguments.clone(),
					reason: step.decision.reason.clone(),
					final_message: step.decision.final_message.clone(),
				},
				visible_tools_before: step.visible_tools_before.clone(),
				visible_resources_before: Some(step.visible_resources_before.clone()),
				started_at: step.started_at.clone(),
				finished_at: step.finished_at.clone(),
				tool_latency_ms: step.tool_latency_ms,
				raw_tool_output: step.raw_tool_output.clone(),
				observation: step
					.observation
					.as_ref()
					.map(|value| serde_json::to_value(value).unwrap_or(serde_json::Value::Null)),
				interpreted_observation: step
					.interpreted_observation
					.as_ref()
					.map(|value| serde_json::to_value(value).unwrap_or(serde_json::Value::Null)),
				execution_trace,
				remaining_step_budget_after: step.remaining_step_budget_after,
				remaining_recovery_budget_after: step.remaining_recovery_budget_after,
				working_directory_after: step.working_directory_after.clone(),
			})
			.collect(),
		final_outcome: RuntimeLoopTraceOutcome {
			status: loop_status_label(loop_state.status).to_string(),
			terminal_action: loop_state.history.last().and_then(|step| {
				matches!(
					step.action,
					StepAction::AskUser
						| StepAction::FinalAnswer
						| StepAction::Fail | StepAction::Stop
				)
				.then(|| step_action_label(step.action).to_string())
			}),
			final_message: loop_state.history.last().and_then(step_final_message),
		},
	}
}

pub fn check_runtime_loop_trace(trace: &RuntimeLoopTrace) -> RuntimeLoopTraceCheckReport {
	let step_count_matches = trace.step_count == trace.steps.len();
	let decisions_captured = trace.steps.iter().all(|step| {
		let action_known = matches!(
			step.decision.action.as_str(),
			"call_tool" | "ask_user" | "final_answer" | "fail" | "stop" | "compact_boundary"
		);
		let reason_present = !step.decision.reason.trim().is_empty();
		let call_tool_has_name =
			step.decision.action != "call_tool" || step.decision.tool_name.is_some();
		action_known && reason_present && call_tool_has_name
	});
	let visible_tools_captured = trace.steps.iter().all(|step| {
		step.decision.action == "compact_boundary" || !step.visible_tools_before.is_empty()
	});
	let visible_resources_captured = trace
		.steps
		.iter()
		.all(|step| step.visible_resources_before.is_some());
	let tool_steps = trace
		.steps
		.iter()
		.filter(|step| step.decision.action == "call_tool")
		.collect::<Vec<_>>();
	let tool_steps_capture_raw_output =
		tool_steps.iter().all(|step| step.raw_tool_output.is_some());
	let tool_steps_capture_normalized_observation =
		tool_steps.iter().all(|step| step.observation.is_some());
	let tool_steps_capture_interpreted_observation = tool_steps
		.iter()
		.all(|step| step.interpreted_observation.is_some());
	let command_tool_steps = tool_steps
		.iter()
		.filter(|step| step.decision.tool_name.as_deref() == Some("command.run"))
		.collect::<Vec<_>>();
	let command_execution_traces_captured = command_tool_steps
		.iter()
		.filter(|step| step.raw_tool_output.is_some())
		.all(|step| step.execution_trace.is_some());
	let execution_trace_stage_order_valid = execution_trace_stage_order_valid(trace);
	let execution_trace_digests_aligned =
		trace.steps.iter().all(step_execution_trace_digests_aligned);
	let terminal_outcome_captured = trace.final_outcome.terminal_action.is_some();
	let final_outcome_matches_last_step = trace.steps.last().is_some_and(|last_step| {
		trace
			.final_outcome
			.terminal_action
			.as_deref()
			.is_some_and(|action| action == last_step.decision.action)
	});

	let mut issues = Vec::new();
	if trace.schema_version != RuntimeLoopTrace::schema_version() {
		issues.push(format!(
			"unexpected trace schema version `{}`",
			trace.schema_version
		));
	}
	if !step_count_matches {
		issues.push(format!(
			"trace step_count={} does not match steps.len()={}",
			trace.step_count,
			trace.steps.len()
		));
	}
	if !decisions_captured {
		issues.push("trace steps are missing a valid NextStepDecision".to_string());
	}
	if !visible_tools_captured {
		issues.push("trace steps did not capture visible tools for the decision round".to_string());
	}
	if !visible_resources_captured {
		issues.push(
			"trace steps did not capture visible resources for the decision round".to_string(),
		);
	}
	if !tool_steps_capture_raw_output {
		issues.push("at least one tool step is missing raw tool output".to_string());
	}
	if !tool_steps_capture_normalized_observation {
		issues.push("at least one tool step is missing a normalized observation".to_string());
	}
	if !tool_steps_capture_interpreted_observation {
		issues.push("at least one tool step is missing an interpreted observation".to_string());
	}
	if !command_execution_traces_captured {
		issues.push(
			"at least one command.run tool step is missing structured execution trace evidence"
				.to_string(),
		);
	}
	if !execution_trace_stage_order_valid {
		issues.push("execution trace stages do not form a valid causal ordering".to_string());
	}
	if !execution_trace_digests_aligned {
		issues.push("execution trace digest alignment does not match payload evidence".to_string());
	}
	if !terminal_outcome_captured {
		issues.push("trace did not capture a terminal outcome".to_string());
	}
	if terminal_outcome_captured && !final_outcome_matches_last_step {
		issues.push(
			"trace terminal outcome does not match the final recorded step decision".to_string(),
		);
	}

	RuntimeLoopTraceCheckReport {
		schema_version: trace.schema_version.clone(),
		step_count_matches,
		decisions_captured,
		visible_tools_captured,
		visible_resources_captured,
		tool_steps_capture_raw_output,
		tool_steps_capture_normalized_observation,
		tool_steps_capture_interpreted_observation,
		command_execution_traces_captured,
		execution_trace_stage_order_valid,
		execution_trace_digests_aligned,
		terminal_outcome_captured,
		final_outcome_matches_last_step,
		issues,
	}
}

fn execution_trace_stage_order_valid(trace: &RuntimeLoopTrace) -> bool {
	let mut pending_approval_digests = HashSet::<&str>::new();
	for step in &trace.steps {
		let Some(execution_trace) = step.execution_trace.as_ref() else {
			continue;
		};
		let stages = execution_trace
			.stages
			.iter()
			.map(|stage| stage.stage)
			.collect::<Vec<_>>();
		let monotonically_ordered = stages
			.windows(2)
			.all(|window| stage_rank(window[0]) <= stage_rank(window[1]));
		let approval_requested =
			stage_index(&stages, RuntimeLoopExecutionTraceStage::ApprovalRequested);
		let approval_resolved =
			stage_index(&stages, RuntimeLoopExecutionTraceStage::ApprovalResolved);
		let execution_started =
			stage_index(&stages, RuntimeLoopExecutionTraceStage::ExecutionStarted);
		let execution_finished =
			stage_index(&stages, RuntimeLoopExecutionTraceStage::ExecutionFinished);
		let observation_recorded =
			stage_index(&stages, RuntimeLoopExecutionTraceStage::ObservationRecorded);
		if !monotonically_ordered {
			return false;
		}
		if approval_requested.is_some() && execution_started.is_some() {
			return false;
		}
		if let Some(requested) = approval_requested {
			if execution_finished.is_some()
				|| observation_recorded.is_none_or(|index| requested > index)
			{
				return false;
			}
			pending_approval_digests.insert(execution_trace.digest.as_str());
		}
		if let Some(resolved) = approval_resolved {
			if !pending_approval_digests.remove(execution_trace.digest.as_str()) {
				return false;
			}
			if execution_started.is_none_or(|started| resolved > started) {
				return false;
			}
		}
		if let (Some(finished), Some(observed)) = (execution_finished, observation_recorded)
			&& finished > observed
		{
			return false;
		}
	}

	true
}

fn stage_rank(stage: RuntimeLoopExecutionTraceStage) -> usize {
	match stage {
		RuntimeLoopExecutionTraceStage::Canonicalized => 0,
		RuntimeLoopExecutionTraceStage::PolicyDecided => 1,
		RuntimeLoopExecutionTraceStage::ApprovalRequested => 2,
		RuntimeLoopExecutionTraceStage::ApprovalResolved => 3,
		RuntimeLoopExecutionTraceStage::ExecutionStarted => 4,
		RuntimeLoopExecutionTraceStage::ExecutionFinished => 5,
		RuntimeLoopExecutionTraceStage::ObservationRecorded => 6,
	}
}

fn stage_index(
	stages: &[RuntimeLoopExecutionTraceStage],
	target: RuntimeLoopExecutionTraceStage,
) -> Option<usize> {
	stages.iter().position(|stage| *stage == target)
}

fn step_execution_trace_digests_aligned(step: &RuntimeLoopTraceStep) -> bool {
	let Some(execution_trace) = step.execution_trace.as_ref() else {
		return true;
	};

	let raw_output_digest = step
		.raw_tool_output
		.as_ref()
		.and_then(payload_digest)
		.is_none_or(|digest| digest == execution_trace.digest);
	let observation_digest = step
		.observation
		.as_ref()
		.and_then(payload_digest)
		.is_none_or(|digest| digest == execution_trace.digest);
	let policy_stages_structured = execution_trace.stages.iter().all(|stage| {
		stage.stage != RuntimeLoopExecutionTraceStage::PolicyDecided
			|| stage.policy_decision.is_some()
	});

	raw_output_digest && observation_digest && policy_stages_structured
}

fn payload_digest(payload: &Value) -> Option<&str> {
	payload
		.get("digest")
		.and_then(Value::as_str)
		.or_else(|| payload.get("data")?.get("digest").and_then(Value::as_str))
}

fn step_final_message(step: &crate::runtime_loop::StepRecord) -> Option<String> {
	match step.observation.as_ref() {
		Some(StepObservation::AskUser { final_message })
		| Some(StepObservation::FinalMessage { final_message }) => Some(final_message.clone()),
		Some(StepObservation::Tool(observation)) => Some(observation.message.clone()),
		None => step.decision.final_message.clone(),
	}
}

fn loop_status_label(status: LoopStatus) -> &'static str {
	match status {
		LoopStatus::Received => "received",
		LoopStatus::Classified => "classified",
		LoopStatus::LoopRunning => "loop_running",
		LoopStatus::AwaitingUser => "awaiting_user",
		LoopStatus::Succeeded => "succeeded",
		LoopStatus::Failed => "failed",
		LoopStatus::Stopped => "stopped",
	}
}

fn step_action_label(action: StepAction) -> &'static str {
	match action {
		StepAction::CallTool => "call_tool",
		StepAction::AskUser => "ask_user",
		StepAction::FinalAnswer => "final_answer",
		StepAction::Fail => "fail",
		StepAction::Stop => "stop",
		StepAction::CompactBoundary => "compact_boundary",
	}
}

#[cfg(test)]
mod tests {
	use serde_json::json;

	use super::{check_runtime_loop_trace, runtime_loop_trace};
	use crate::router::{IntentFamily, RouteDecision, RouteRisk};
	use crate::runtime_loop::{
		InterpretedObservation, LoopContext, LoopState, NextStepAction, NextStepDecision,
		StepObservation, StepRecord, ToolObservation,
	};
	use roku_common_types::{
		ApprovalRequirement, ApprovalRequirementScope, PolicyDecision, PolicyOutcome,
		PolicyReasonCode, ResourceSelector, RuntimeLoopExecutionTraceStage, ToolOutputEnvelope,
	};
	use roku_plugin_tools::canonical_execution_for_builtin_tool_input;

	fn loop_state() -> LoopState {
		let context = LoopContext {
			request_id: "req-trace".to_string(),
			session_id: "session-trace".to_string(),
			goal: "Run `pwd`".to_string(),
			workspace_root: "/workspace".to_string(),
			working_directory: "/workspace".to_string(),
			visible_tools: vec!["command.run".to_string(), "general.execute".to_string()],
			bound_resources: vec![ResourceSelector::tool("command.run".to_string())],
			route_decision: RouteDecision::new(
				IntentFamily::CodeExec,
				0.95,
				false,
				RouteRisk::Medium,
				vec!["command.run".to_string()],
				Vec::new(),
				Vec::new(),
				"explicit command",
			),
			last_observation: None,
		};
		LoopState::new("loop-trace", &context)
	}

	#[test]
	fn runtime_loop_trace_captures_decision_and_visible_tools() {
		let mut state = loop_state();
		let observation = ToolObservation {
			ok: true,
			tool_name: "command.run".to_string(),
			error_type: None,
			terminal: false,
			data: json!({"stdout": "/workspace\n"}),
			message: "/workspace".to_string(),
		};
		let interpreted = InterpretedObservation {
			raw_observation: observation.clone(),
			continue_allowed: true,
			should_ask_user: false,
			should_emit_final_answer: false,
			should_fail: false,
			terminal: false,
			budget_exhausted: false,
			recovery_exhausted: false,
			remaining_step_budget: 3,
			remaining_recovery_budget: 2,
			new_working_directory: None,
			visible_tools: vec!["command.run".to_string(), "general.execute".to_string()],
		};
		state.record_step(StepRecord::tool_call(
			1,
			NextStepDecision {
				action: NextStepAction::CallTool,
				tool_name: Some("command.run".to_string()),
				arguments: Some(json!({ "command": "pwd" })),
				tool_calls: None,
				reason: "run explicit command".to_string(),
				final_message: None,
			},
			vec!["command.run".to_string(), "general.execute".to_string()],
			state.bound_resources.clone(),
			json!({"ok": true}),
			StepObservation::Tool(observation),
			interpreted,
			Some(10),
			3,
			2,
			"/workspace",
		));
		state.record_step(StepRecord::terminal(
			2,
			crate::runtime_loop::StepAction::FinalAnswer,
			NextStepDecision {
				action: NextStepAction::FinalAnswer,
				tool_name: None,
				arguments: None,
				tool_calls: None,
				reason: "answer from grounded command".to_string(),
				final_message: Some("/workspace".to_string()),
			},
			vec!["command.run".to_string(), "general.execute".to_string()],
			state.bound_resources.clone(),
			Some(StepObservation::FinalMessage {
				final_message: "/workspace".to_string(),
			}),
			2,
			2,
			"/workspace",
		));

		let trace = runtime_loop_trace(&state);
		let report = check_runtime_loop_trace(&trace);
		assert_eq!(trace.step_count, 2);
		assert_eq!(trace.steps[0].decision.action, "call_tool");
		assert_eq!(
			trace.steps[0].visible_tools_before,
			vec!["command.run".to_string(), "general.execute".to_string()]
		);
		assert_eq!(
			trace.steps[0].visible_resources_before,
			Some(vec![ResourceSelector::tool("command.run".to_string())])
		);
		assert!(trace.steps[0].execution_trace.is_some());
		assert!(report.command_execution_traces_captured);
		assert!(report.visible_resources_captured);
		assert!(report.execution_trace_stage_order_valid);
		assert!(report.execution_trace_digests_aligned);
		assert!(
			report.issues.is_empty(),
			"unexpected trace issues: {report:?}"
		);
	}

	#[test]
	fn runtime_loop_trace_aligns_approval_and_execution_causality_by_digest() {
		let mut state = loop_state();
		let current_directory = std::env::current_dir()
			.expect("current directory should exist")
			.display()
			.to_string();
		let decision_arguments = json!({
			"command": "pwd",
			"cwd": current_directory.clone(),
		});
		let canonical_execution =
			canonical_execution_for_builtin_tool_input("command.run", &decision_arguments)
				.expect("command.run arguments should canonicalize");
		let digest = canonical_execution.digest.0.clone();
		let approval_payload = json!({
			"error_code": "approval_required",
			"message": "approval required",
			"policy_decision": {
				"outcome": "require_approval",
				"reason_code": "approval_required_by_untrusted_program",
				"approval_requirement": {
					"scope": "invocation",
					"reason_code": "approval_required_by_untrusted_program"
				}
			},
			"canonical_execution": canonical_execution,
			"digest": digest.clone(),
		});
		let approval_observation = ToolObservation {
			ok: false,
			tool_name: "command.run".to_string(),
			error_type: Some("approval_required".to_string()),
			terminal: false,
			data: json!({
				"error_code": "approval_required",
			}),
			message: "approval required".to_string(),
		};
		let approval_interpreted = InterpretedObservation {
			raw_observation: approval_observation.clone(),
			continue_allowed: false,
			should_ask_user: false,
			should_emit_final_answer: false,
			should_fail: true,
			terminal: false,
			budget_exhausted: false,
			recovery_exhausted: false,
			remaining_step_budget: 3,
			remaining_recovery_budget: 2,
			new_working_directory: None,
			visible_tools: vec!["command.run".to_string()],
		};
		state.record_step(StepRecord::tool_call(
			1,
			NextStepDecision {
				action: NextStepAction::CallTool,
				tool_name: Some("command.run".to_string()),
				arguments: Some(decision_arguments.clone()),
				tool_calls: None,
				reason: "request approval".to_string(),
				final_message: None,
			},
			state.visible_tools.clone(),
			state.bound_resources.clone(),
			approval_payload,
			StepObservation::Tool(approval_observation),
			approval_interpreted,
			Some(5),
			3,
			2,
			"/workspace",
		));
		let resumed_output = ToolOutputEnvelope::new(
			true,
			None::<String>,
			false,
			"/workspace".to_string(),
			json!({
				"command": "pwd",
				"argv": ["pwd"],
				"program": "pwd",
				"cwd": current_directory,
				"stdout": "/workspace\n",
				"stderr": "",
				"exit_code": 0,
				"truncated": false,
				"digest": digest.clone(),
			}),
		)
		.into_value();
		let resumed_observation = ToolObservation {
			ok: true,
			tool_name: "command.run".to_string(),
			error_type: None,
			terminal: false,
			data: json!({
				"command": "pwd",
				"stdout": "/workspace\n",
				"digest": digest.clone(),
			}),
			message: "/workspace".to_string(),
		};
		let resumed_interpreted = InterpretedObservation {
			raw_observation: resumed_observation.clone(),
			continue_allowed: true,
			should_ask_user: false,
			should_emit_final_answer: false,
			should_fail: false,
			terminal: false,
			budget_exhausted: false,
			recovery_exhausted: false,
			remaining_step_budget: 2,
			remaining_recovery_budget: 2,
			new_working_directory: None,
			visible_tools: vec!["command.run".to_string()],
		};
		state.record_step(StepRecord::tool_call(
			2,
			NextStepDecision {
				action: NextStepAction::CallTool,
				tool_name: Some("command.run".to_string()),
				arguments: Some(decision_arguments),
				tool_calls: None,
				reason: "resume approved command".to_string(),
				final_message: None,
			},
			state.visible_tools.clone(),
			state.bound_resources.clone(),
			resumed_output,
			StepObservation::Tool(resumed_observation),
			resumed_interpreted,
			Some(12),
			2,
			2,
			"/workspace",
		));
		state.record_step(StepRecord::terminal(
			3,
			crate::runtime_loop::StepAction::FinalAnswer,
			NextStepDecision {
				action: NextStepAction::FinalAnswer,
				tool_name: None,
				arguments: None,
				tool_calls: None,
				reason: "finish after resumed execution".to_string(),
				final_message: Some("/workspace".to_string()),
			},
			state.visible_tools.clone(),
			state.bound_resources.clone(),
			Some(StepObservation::FinalMessage {
				final_message: "/workspace".to_string(),
			}),
			1,
			2,
			"/workspace",
		));

		let trace = runtime_loop_trace(&state);
		let report = check_runtime_loop_trace(&trace);
		let approval_trace = trace.steps[0]
			.execution_trace
			.as_ref()
			.expect("approval step should carry execution trace");
		assert_eq!(approval_trace.digest, digest);
		assert_eq!(
			approval_trace
				.stages
				.iter()
				.map(|stage| stage.stage)
				.collect::<Vec<_>>(),
			vec![
				RuntimeLoopExecutionTraceStage::Canonicalized,
				RuntimeLoopExecutionTraceStage::PolicyDecided,
				RuntimeLoopExecutionTraceStage::ApprovalRequested,
				RuntimeLoopExecutionTraceStage::ObservationRecorded,
			]
		);
		assert_eq!(
			approval_trace.stages[1].policy_decision,
			Some(PolicyDecision {
				outcome: PolicyOutcome::RequireApproval,
				reason_code: PolicyReasonCode::ApprovalRequiredByUntrustedProgram,
				approval_requirement: Some(ApprovalRequirement {
					scope: ApprovalRequirementScope::Invocation,
					reason_code: PolicyReasonCode::ApprovalRequiredByUntrustedProgram,
				}),
			})
		);
		let resumed_trace = trace.steps[1]
			.execution_trace
			.as_ref()
			.expect("resumed execution step should carry execution trace");
		assert_eq!(resumed_trace.digest, approval_trace.digest);
		assert_eq!(
			resumed_trace
				.stages
				.iter()
				.map(|stage| stage.stage)
				.collect::<Vec<_>>(),
			vec![
				RuntimeLoopExecutionTraceStage::ApprovalResolved,
				RuntimeLoopExecutionTraceStage::ExecutionStarted,
				RuntimeLoopExecutionTraceStage::ExecutionFinished,
				RuntimeLoopExecutionTraceStage::ObservationRecorded,
			]
		);
		assert!(report.command_execution_traces_captured);
		assert!(report.execution_trace_stage_order_valid);
		assert!(report.execution_trace_digests_aligned);
	}

	#[test]
	fn runtime_loop_trace_flags_digest_mismatch_between_trace_and_observation() {
		let mut state = loop_state();
		let observation = ToolObservation {
			ok: true,
			tool_name: "command.run".to_string(),
			error_type: None,
			terminal: false,
			data: json!({"stdout": "/workspace\n", "digest": "digest-other"}),
			message: "/workspace".to_string(),
		};
		let interpreted = InterpretedObservation {
			raw_observation: observation.clone(),
			continue_allowed: true,
			should_ask_user: false,
			should_emit_final_answer: false,
			should_fail: false,
			terminal: false,
			budget_exhausted: false,
			recovery_exhausted: false,
			remaining_step_budget: 3,
			remaining_recovery_budget: 2,
			new_working_directory: None,
			visible_tools: vec!["command.run".to_string(), "general.execute".to_string()],
		};
		state.record_step(StepRecord::tool_call(
			1,
			NextStepDecision {
				action: NextStepAction::CallTool,
				tool_name: Some("command.run".to_string()),
				arguments: Some(json!({ "command": "pwd" })),
				tool_calls: None,
				reason: "run explicit command".to_string(),
				final_message: None,
			},
			vec!["command.run".to_string(), "general.execute".to_string()],
			state.bound_resources.clone(),
			json!({
				"ok": true,
				"data": {
					"digest": "digest-other"
				}
			}),
			StepObservation::Tool(observation),
			interpreted,
			Some(10),
			3,
			2,
			"/workspace",
		));
		state.record_step(StepRecord::terminal(
			2,
			crate::runtime_loop::StepAction::FinalAnswer,
			NextStepDecision {
				action: NextStepAction::FinalAnswer,
				tool_name: None,
				arguments: None,
				tool_calls: None,
				reason: "answer from grounded command".to_string(),
				final_message: Some("/workspace".to_string()),
			},
			vec!["command.run".to_string(), "general.execute".to_string()],
			state.bound_resources.clone(),
			Some(StepObservation::FinalMessage {
				final_message: "/workspace".to_string(),
			}),
			2,
			2,
			"/workspace",
		));

		let mut trace = runtime_loop_trace(&state);
		trace.steps[0]
			.execution_trace
			.as_mut()
			.expect("command step should carry execution trace")
			.digest = "digest-123".to_string();
		let report = check_runtime_loop_trace(&trace);

		assert!(!report.execution_trace_digests_aligned);
		assert!(
			report
				.issues
				.iter()
				.any(|issue| issue.contains("digest alignment"))
		);
	}
}
