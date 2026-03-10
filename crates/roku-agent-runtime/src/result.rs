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

use roku_common_types::{AgentInstanceSpec, EvidenceItem, ResultEnvelope, ResultStatus, TaskNode};
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
	error: ToolRuntimeError,
) -> ResultEnvelope {
	let error_code = tool_error_code(&error);
	let payload = json!({
		"error_code": error_code,
		"message": error.to_string(),
		"tool_name": tool_name,
		"worker_id": worker_id,
		"node_id": node.node_id.0,
	});

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
