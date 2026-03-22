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
	CanonicalExecution, PolicyDecision, PolicyOutcome, PolicyReasonCode, RuntimeLoopExecutionTrace,
	RuntimeLoopExecutionTraceStage, RuntimeLoopExecutionTraceStageRecord,
};
use roku_plugin_tools::canonical_execution_for_builtin_tool_input;
use serde_json::Value;

use crate::runtime_loop::{StepObservation, StepRecord};

pub(crate) fn project_execution_traces(
	history: &[StepRecord],
) -> Vec<Option<RuntimeLoopExecutionTrace>> {
	let mut pending_approval_digests = HashSet::new();

	history
		.iter()
		.map(|step| {
			let evidence = StepExecutionEvidence::from_step(step)?;
			let digest = evidence.canonical_execution.digest.0.clone();
			let tool_name = evidence.canonical_execution.tool_name.clone();
			let approved_from_prior_ticket =
				evidence.executed && pending_approval_digests.remove(&digest);
			let mut stages = Vec::new();

			if approved_from_prior_ticket {
				stages.push(stage(RuntimeLoopExecutionTraceStage::ApprovalResolved));
				stages.push(stage(RuntimeLoopExecutionTraceStage::ExecutionStarted));
				stages.push(stage(RuntimeLoopExecutionTraceStage::ExecutionFinished));
			} else {
				stages.push(stage(RuntimeLoopExecutionTraceStage::Canonicalized));
				let policy_decision = evidence
					.policy_decision
					.clone()
					.unwrap_or_else(synthesized_allow_policy_decision);
				stages.push(stage_with_policy(
					RuntimeLoopExecutionTraceStage::PolicyDecided,
					policy_decision.clone(),
				));
				match policy_decision.outcome {
					PolicyOutcome::Allow => {
						if evidence.executed {
							stages.push(stage(RuntimeLoopExecutionTraceStage::ExecutionStarted));
							stages.push(stage(RuntimeLoopExecutionTraceStage::ExecutionFinished));
						}
					}
					PolicyOutcome::RequireApproval => {
						stages.push(stage(RuntimeLoopExecutionTraceStage::ApprovalRequested));
						pending_approval_digests.insert(digest.clone());
					}
					PolicyOutcome::Deny => {}
				}
			}

			if evidence.observation_recorded {
				stages.push(stage(RuntimeLoopExecutionTraceStage::ObservationRecorded));
			}

			Some(RuntimeLoopExecutionTrace {
				tool_name,
				digest,
				stages,
			})
		})
		.collect()
}

#[derive(Debug, Clone)]
struct StepExecutionEvidence {
	canonical_execution: CanonicalExecution,
	policy_decision: Option<PolicyDecision>,
	executed: bool,
	observation_recorded: bool,
}

impl StepExecutionEvidence {
	fn from_step(step: &StepRecord) -> Option<Self> {
		let tool_name = step.decision.tool_name.as_deref()?;
		if tool_name != "command.run" {
			return None;
		}

		let raw_tool_output = step.raw_tool_output.as_ref();
		let policy_decision = raw_tool_output.and_then(policy_decision_from_payload);
		let canonical_execution = raw_tool_output
			.and_then(canonical_execution_from_payload)
			.or_else(|| {
				step.decision.arguments.as_ref().and_then(|arguments| {
					canonical_execution_for_builtin_tool_input(tool_name, arguments)
				})
			})?;

		let executed = raw_tool_output.is_some()
			&& !matches!(
				policy_decision.as_ref().map(|decision| decision.outcome),
				Some(PolicyOutcome::Deny | PolicyOutcome::RequireApproval)
			);
		let observation_recorded = matches!(step.observation, Some(StepObservation::Tool(_)));

		Some(Self {
			canonical_execution,
			policy_decision,
			executed,
			observation_recorded,
		})
	}
}

fn canonical_execution_from_payload(payload: &Value) -> Option<CanonicalExecution> {
	payload
		.get("canonical_execution")
		.cloned()
		.or_else(|| payload.get("data")?.get("canonical_execution").cloned())
		.and_then(|value| serde_json::from_value(value).ok())
}

fn policy_decision_from_payload(payload: &Value) -> Option<PolicyDecision> {
	serde_json::from_value(payload.get("policy_decision")?.clone()).ok()
}

fn synthesized_allow_policy_decision() -> PolicyDecision {
	PolicyDecision {
		outcome: PolicyOutcome::Allow,
		reason_code: PolicyReasonCode::AllowedByPolicy,
		approval_requirement: None,
	}
}

fn stage(stage: RuntimeLoopExecutionTraceStage) -> RuntimeLoopExecutionTraceStageRecord {
	RuntimeLoopExecutionTraceStageRecord {
		stage,
		policy_decision: None,
	}
}

fn stage_with_policy(
	stage: RuntimeLoopExecutionTraceStage,
	policy_decision: PolicyDecision,
) -> RuntimeLoopExecutionTraceStageRecord {
	RuntimeLoopExecutionTraceStageRecord {
		stage,
		policy_decision: Some(policy_decision),
	}
}
