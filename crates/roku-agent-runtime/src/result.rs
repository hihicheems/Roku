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

use roku_common_types::{
	AgentInstanceSpec, CanonicalExecution, EvidenceItem, PolicyDecision, PolicyOutcome,
	ResultEnvelope, ResultStatus, TaskNode,
};
use roku_plugin_host::{SandboxProfile, ToolExecutionResult, ToolRuntimeError};
use serde_json::{Value, json};

pub(crate) fn policy_rejection_result(spec: &AgentInstanceSpec, node: &TaskNode) -> ResultEnvelope {
	let payload = json!({
		"error_code": "policy_bindings_rejected",
		"message": "policy bindings rejected execution",
		"node_id": node.node_id.0,
	});

	ResultEnvelope {
		task_id: spec.context.task_id.clone(),
		node_id: node.node_id.clone(),
		producer: spec.instance_id.clone(),
		schema_version: "result.v1".to_string(),
		status: ResultStatus::Error,
		payload: serialize_payload(&payload),
		evidence: vec![EvidenceItem {
			kind: "policy".to_string(),
			value: "budget-exhausted".to_string(),
		}],
		confidence: 0.0,
	}
}

pub(crate) fn tool_success_result(
	spec: &AgentInstanceSpec,
	node: &TaskNode,
	worker_id: &str,
	tool_name: &str,
	execution: ToolExecutionResult,
	confidence: f32,
) -> ResultEnvelope {
	let message = execution
		.output
		.get("message")
		.and_then(Value::as_str)
		.unwrap_or(&node.description);
	let derived_evidence = derived_execution_evidence(&execution.output);
	let payload = json!({
		"worker_id": worker_id,
		"tool_name": tool_name,
		"message": message,
		"node_id": node.node_id.0,
		"summary": node.description,
		"attempts": execution.attempts,
		"elapsed_ms": execution.elapsed_ms,
		"output": execution.output,
	});

	ResultEnvelope {
		task_id: spec.context.task_id.clone(),
		node_id: node.node_id.clone(),
		producer: spec.instance_id.clone(),
		schema_version: "result.v1".to_string(),
		status: ResultStatus::Ok,
		payload: serialize_payload(&payload),
		evidence: {
			let mut evidence = vec![
				EvidenceItem {
					kind: "runtime".to_string(),
					value: worker_id.to_string(),
				},
				EvidenceItem {
					kind: "tool".to_string(),
					value: tool_name.to_string(),
				},
				EvidenceItem {
					kind: "output_fingerprint".to_string(),
					value: execution.output_fingerprint,
				},
				EvidenceItem {
					kind: "sandbox_profile".to_string(),
					value: sandbox_profile_label(&execution.sandbox_profile).to_string(),
				},
				EvidenceItem {
					kind: "tool_attempts".to_string(),
					value: execution.attempts.to_string(),
				},
				EvidenceItem {
					kind: "policy".to_string(),
					value: format!(
						"budget_tokens={},time_budget_ms={}",
						spec.policy_bindings.budget_tokens, spec.policy_bindings.time_budget_ms
					),
				},
			];
			evidence.extend(derived_evidence);
			evidence
		},
		confidence,
	}
}

pub(crate) fn tool_failure_result(
	spec: &AgentInstanceSpec,
	node: &TaskNode,
	worker_id: &str,
	tool_name: &str,
	canonical_execution: Option<CanonicalExecution>,
	error: ToolRuntimeError,
) -> ResultEnvelope {
	let error_code = tool_error_code(&error);
	let mut payload = json!({
		"error_code": error_code,
		"message": error.to_string(),
		"tool_name": tool_name,
		"worker_id": worker_id,
		"node_id": node.node_id.0,
	});
	if let Some(execution) = canonical_execution.as_ref() {
		payload["canonical_execution"] = serde_json::to_value(execution).unwrap_or(Value::Null);
		payload["digest"] = Value::String(execution.digest.0.clone());
	}
	if let Some(policy_decision) = error.policy_decision() {
		payload["policy_decision"] = serde_json::to_value(policy_decision).unwrap_or(Value::Null);
	}

	ResultEnvelope {
		task_id: spec.context.task_id.clone(),
		node_id: node.node_id.clone(),
		producer: spec.instance_id.clone(),
		schema_version: "result.v1".to_string(),
		status: ResultStatus::Error,
		payload: serialize_payload(&payload),
		evidence: vec![
			EvidenceItem {
				kind: "runtime".to_string(),
				value: worker_id.to_string(),
			},
			EvidenceItem {
				kind: "tool".to_string(),
				value: tool_name.to_string(),
			},
			EvidenceItem {
				kind: "tool_error".to_string(),
				value: error_code.to_string(),
			},
		],
		confidence: 0.0,
	}
}

fn tool_error_code(error: &ToolRuntimeError) -> &'static str {
	if let Some(policy_decision) = error.policy_decision() {
		return policy_error_code(policy_decision);
	}

	match error {
		ToolRuntimeError::ToolNotFound(_) => "tool_not_found",
		ToolRuntimeError::ToolAlreadyRegistered(_) => "tool_already_registered",
		ToolRuntimeError::InvalidDescriptor(_) => "invalid_descriptor",
		ToolRuntimeError::InputSchemaViolation { .. } => "input_schema_violation",
		ToolRuntimeError::CapabilityDenied { .. } => "capability_denied",
		ToolRuntimeError::Timeout { .. } => "timeout",
		ToolRuntimeError::ExecutionFailed { retriable, .. } => {
			if *retriable {
				"retriable_execution_failed"
			} else {
				"execution_failed"
			}
		}
	}
}

fn policy_error_code(policy_decision: &PolicyDecision) -> &'static str {
	match policy_decision.outcome {
		PolicyOutcome::Allow => "execution_failed",
		PolicyOutcome::Deny => "policy_denied",
		PolicyOutcome::RequireApproval => "approval_required",
	}
}

fn sandbox_profile_label(profile: &SandboxProfile) -> &'static str {
	match profile {
		SandboxProfile::NoIsolation => "no_isolation",
		SandboxProfile::ReadOnlyFs => "read_only_fs",
		SandboxProfile::PythonResearch => "python_research",
		SandboxProfile::ContainerRestricted => "container_restricted",
	}
}

fn serialize_payload(payload: &Value) -> String {
	match serde_json::to_string(payload) {
		Ok(serialized) => serialized,
		Err(error) => format!(r#"{{"error_code":"serialization_failure","message":"{error}"}}"#),
	}
}

fn derived_execution_evidence(output: &Value) -> Vec<EvidenceItem> {
	let output = tool_output_data(output);
	let mut evidence = Vec::new();
	if let Some(selected_skill) = output.get("selected_skill").and_then(Value::as_str) {
		evidence.push(EvidenceItem {
			kind: "selected_skill".to_string(),
			value: selected_skill.to_string(),
		});
	}
	if let Some(status) = output.get("validation_status").and_then(Value::as_str) {
		evidence.push(EvidenceItem {
			kind: "validation_status".to_string(),
			value: status.to_string(),
		});
	}
	if let Some(paths) = output.get("created_paths").and_then(Value::as_array) {
		evidence.extend(
			paths
				.iter()
				.filter_map(Value::as_str)
				.map(|path| EvidenceItem {
					kind: "created_path".to_string(),
					value: path.to_string(),
				}),
		);
	}
	if let Some(scripts) = output.get("executed_scripts").and_then(Value::as_array) {
		evidence.extend(
			scripts
				.iter()
				.filter_map(Value::as_str)
				.map(|path| EvidenceItem {
					kind: "executed_script".to_string(),
					value: path.to_string(),
				}),
		);
	}
	evidence
}

fn tool_output_data(output: &Value) -> &Value {
	output.get("data").unwrap_or(output)
}

#[cfg(test)]
mod tests {
	use super::*;
	use roku_common_types::{
		ApprovalRequirement, ApprovalRequirementScope, CanonicalDigest, ExecutionActionClass,
		ExecutionEnvPolicy, ExecutionEnvPolicyMode, ExecutionResourceScope, InvocationMode, NodeId,
		PolicyBindings, PolicyReasonCode, TaskId,
	};
	use roku_plugin_host::ToolRuntimeError;

	fn sample_spec() -> AgentInstanceSpec {
		AgentInstanceSpec {
			instance_id: "worker-1".to_string(),
			context: roku_common_types::AgentContext {
				task_id: TaskId("task-1".to_string()),
				node_id: NodeId("node-1".to_string()),
				summary: "run command".to_string(),
				resources: Vec::new(),
				conversation_history: Vec::new(),
				memory_context: String::new(),
			},
			capabilities: Vec::new(),
			capability_tokens: Vec::new(),
			policy_bindings: PolicyBindings {
				budget_tokens: 8_000,
				time_budget_ms: 60_000,
			},
		}
	}

	fn sample_node() -> TaskNode {
		TaskNode {
			node_id: NodeId("node-1".to_string()),
			description: "execute".to_string(),
			..TaskNode::default()
		}
	}

	fn sample_execution() -> CanonicalExecution {
		CanonicalExecution {
			tool_name: "command.run".to_string(),
			program: "rm".to_string(),
			argv: vec!["rm".to_string(), "-rf".to_string(), "tmp".to_string()],
			invocation_mode: InvocationMode::DirectExec,
			shell_context: None,
			cwd: "/workspace".to_string(),
			env_policy: ExecutionEnvPolicy {
				mode: ExecutionEnvPolicyMode::InheritSelected,
				allowed_keys: vec!["ROKU_COMMAND_SCOPE_ROOT".to_string()],
			},
			resource_scope: ExecutionResourceScope {
				working_directory: "/workspace".to_string(),
				resolved_targets: vec!["/workspace/tmp".to_string()],
				effective_read_roots: vec!["/workspace".to_string()],
				effective_write_roots: Vec::new(),
			},
			action_class: ExecutionActionClass::Exec,
			digest: CanonicalDigest("digest-123".to_string()),
		}
	}

	fn require_approval_error() -> ToolRuntimeError {
		ToolRuntimeError::ExecutionFailed {
			tool: "command.run".to_string(),
			attempts: 0,
			message:
				"policy_outcome=require_approval reason_code=approval_required_by_untrusted_program"
					.to_string(),
			retriable: false,
			policy_decision: Some(PolicyDecision {
				outcome: PolicyOutcome::RequireApproval,
				reason_code: PolicyReasonCode::ApprovalRequiredByUntrustedProgram,
				approval_requirement: Some(ApprovalRequirement {
					scope: ApprovalRequirementScope::Invocation,
					reason_code: PolicyReasonCode::ApprovalRequiredByUntrustedProgram,
				}),
			}),
		}
	}

	fn command_not_allowed_error() -> ToolRuntimeError {
		ToolRuntimeError::ExecutionFailed {
			tool: "command.run".to_string(),
			attempts: 0,
			message: "command_not_allowed: metacharacters are not allowed".to_string(),
			retriable: false,
			policy_decision: None,
		}
	}

	#[test]
	fn tool_failure_result_preserves_require_approval_fact() {
		let result = tool_failure_result(
			&sample_spec(),
			&sample_node(),
			"worker-1",
			"command.run",
			Some(sample_execution()),
			require_approval_error(),
		);
		let payload =
			serde_json::from_str::<Value>(&result.payload).expect("payload should decode");

		assert_eq!(payload["error_code"], "approval_required");
		assert_eq!(payload["digest"], Value::String("digest-123".to_string()));
		assert_eq!(
			payload["policy_decision"]["reason_code"],
			"approval_required_by_untrusted_program"
		);
		assert_eq!(payload["canonical_execution"]["program"], "rm");
	}

	#[test]
	fn tool_failure_result_preserves_canonical_execution_without_policy_decision() {
		let result = tool_failure_result(
			&sample_spec(),
			&sample_node(),
			"worker-1",
			"command.run",
			Some(sample_execution()),
			command_not_allowed_error(),
		);
		let payload =
			serde_json::from_str::<Value>(&result.payload).expect("payload should decode");

		assert_eq!(payload["error_code"], "execution_failed");
		assert_eq!(payload["digest"], Value::String("digest-123".to_string()));
		assert_eq!(payload["canonical_execution"]["program"], "rm");
		assert_eq!(payload.get("policy_decision"), None);
	}
}
