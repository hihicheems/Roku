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

use roku_common_types::{ResultEnvelope, ValidationEvidenceSet, ValidationReport};

use crate::ValidationConfig;
use crate::cross_check::run_cross_checks;
use crate::policy::run_policy_checks;
use crate::provenance::run_provenance_checks;
use crate::schema::run_schema_checks;
use crate::semantic::run_semantic_checks;

#[derive(Debug, Default)]
pub struct ValidationPipeline {
	config: ValidationConfig,
}

impl ValidationPipeline {
	pub fn with_config(config: ValidationConfig) -> Self {
		Self { config }
	}

	pub fn validate(&self, result: &ResultEnvelope) -> ValidationReport {
		self.validate_evidence_set(&ValidationEvidenceSet {
			result: result.clone(),
			artifacts: Vec::new(),
		})
	}

	pub fn validate_evidence_set(&self, evidence_set: &ValidationEvidenceSet) -> ValidationReport {
		let mut failures = Vec::new();
		let result = &evidence_set.result;

		run_schema_checks(&self.config, result, &mut failures);
		run_semantic_checks(result, &mut failures);
		run_provenance_checks(&self.config, evidence_set, &mut failures);
		run_policy_checks(&self.config, result, &mut failures);
		run_cross_checks(&self.config, evidence_set, &mut failures);

		ValidationReport {
			accepted: failures.is_empty(),
			failures,
		}
	}
}

#[cfg(test)]
mod tests {
	use roku_common_types::{
		Artifact, ArtifactId, ArtifactMetadataEntry, EvidenceItem, NodeId, ResultEnvelope,
		ResultStatus, RuntimeLoopTrace, RuntimeLoopTraceDecision, RuntimeLoopTraceOutcome,
		RuntimeLoopTraceStep, TaskId, ValidationEvidenceSet,
	};
	use serde_json::json;

	use super::*;

	fn artifact(uri: &str, schema_version: &str) -> Artifact {
		Artifact {
			artifact_id: ArtifactId(format!("artifact-{uri}")),
			task_id: TaskId("t1".to_string()),
			node_id: NodeId("n1".to_string()),
			kind: "node_result".to_string(),
			uri: uri.to_string(),
			schema_version: schema_version.to_string(),
			checksum: "bytes:53".to_string(),
			metadata: vec![ArtifactMetadataEntry {
				key: "producer".to_string(),
				value: "agent".to_string(),
			}],
		}
	}

	#[test]
	fn reject_result_without_evidence() {
		let pipeline = ValidationPipeline::default();
		let report = pipeline.validate(&ResultEnvelope {
			task_id: TaskId("t1".to_string()),
			node_id: NodeId("n1".to_string()),
			producer: "agent".to_string(),
			schema_version: "v1".to_string(),
			status: ResultStatus::Ok,
			payload: "p".to_string(),
			evidence: Vec::<EvidenceItem>::new(),
			confidence: 0.8,
		});

		assert!(!report.accepted);
	}

	#[test]
	fn reject_backtest_payload_missing_required_fields() {
		let pipeline = ValidationPipeline::default();
		let report = pipeline.validate(&ResultEnvelope {
			task_id: TaskId("t1".to_string()),
			node_id: NodeId("n1".to_string()),
			producer: "agent".to_string(),
			schema_version: "backtest_report.v1".to_string(),
			status: ResultStatus::Ok,
			payload: "{}".to_string(),
			evidence: vec![EvidenceItem {
				kind: "artifact_ref".to_string(),
				value: "artifact://1".to_string(),
			}],
			confidence: 0.9,
		});

		assert!(!report.accepted);
		assert!(
			report
				.failures
				.iter()
				.any(|failure| failure.contains("annual_return"))
		);
	}

	#[test]
	fn accept_valid_backtest_payload() {
		let pipeline = ValidationPipeline::default();
		let report = pipeline.validate_evidence_set(&ValidationEvidenceSet {
			result: ResultEnvelope {
				task_id: TaskId("t1".to_string()),
				node_id: NodeId("n1".to_string()),
				producer: "agent".to_string(),
				schema_version: "backtest_report.v1".to_string(),
				status: ResultStatus::Ok,
				payload: r#"{"annual_return":0.1,"sharpe":1.2,"max_drawdown":0.2}"#.to_string(),
				evidence: vec![EvidenceItem {
					kind: "artifact_ref".to_string(),
					value: "artifact://1".to_string(),
				}],
				confidence: 0.9,
			},
			artifacts: vec![artifact("artifact://1", "backtest_report.v1")],
		});

		assert!(report.accepted);
	}

	#[test]
	fn reject_missing_artifact_reference() {
		let pipeline = ValidationPipeline::default();
		let report = pipeline.validate_evidence_set(&ValidationEvidenceSet {
			result: ResultEnvelope {
				task_id: TaskId("t1".to_string()),
				node_id: NodeId("n1".to_string()),
				producer: "agent".to_string(),
				schema_version: "result.v1".to_string(),
				status: ResultStatus::Ok,
				payload: "payload".to_string(),
				evidence: vec![EvidenceItem {
					kind: "artifact_ref".to_string(),
					value: "artifact://missing".to_string(),
				}],
				confidence: 0.9,
			},
			artifacts: Vec::new(),
		});

		assert!(!report.accepted);
		assert!(
			report
				.failures
				.iter()
				.any(|failure| failure.contains("artifact evidence could not be resolved"))
		);
	}

	#[test]
	fn reject_duplicate_artifact_evidence_in_cross_check() {
		let pipeline = ValidationPipeline::default();
		let report = pipeline.validate_evidence_set(&ValidationEvidenceSet {
			result: ResultEnvelope {
				task_id: TaskId("t1".to_string()),
				node_id: NodeId("n1".to_string()),
				producer: "agent".to_string(),
				schema_version: "result.v1".to_string(),
				status: ResultStatus::Ok,
				payload: "payload".to_string(),
				evidence: vec![
					EvidenceItem {
						kind: "artifact_ref".to_string(),
						value: "artifact://1".to_string(),
					},
					EvidenceItem {
						kind: "artifact_ref".to_string(),
						value: "artifact://1".to_string(),
					},
				],
				confidence: 0.9,
			},
			artifacts: vec![artifact("artifact://1", "result.v1")],
		});

		assert!(!report.accepted);
		assert!(
			report
				.failures
				.iter()
				.any(|failure| failure.contains("duplicate artifact evidence"))
		);
	}

	#[test]
	fn reject_artifact_schema_mismatch_in_cross_check() {
		let pipeline = ValidationPipeline::default();
		let report = pipeline.validate_evidence_set(&ValidationEvidenceSet {
			result: ResultEnvelope {
				task_id: TaskId("t1".to_string()),
				node_id: NodeId("n1".to_string()),
				producer: "agent".to_string(),
				schema_version: "result.v1".to_string(),
				status: ResultStatus::Ok,
				payload: "payload".to_string(),
				evidence: vec![EvidenceItem {
					kind: "artifact_ref".to_string(),
					value: "artifact://1".to_string(),
				}],
				confidence: 0.9,
			},
			artifacts: vec![artifact("artifact://1", "other.v1")],
		});

		assert!(!report.accepted);
		assert!(
			report
				.failures
				.iter()
				.any(|failure| failure.contains("artifact schema"))
		);
	}

	#[test]
	fn reject_runtime_loop_trace_missing_required_step_layers() {
		let pipeline = ValidationPipeline::default();
		let trace = RuntimeLoopTrace {
			schema_version: RuntimeLoopTrace::schema_version().to_string(),
			run_id: "loop-1".to_string(),
			status: "failed".to_string(),
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
					started_at: "2026-01-01T00:00:00Z".to_string(),
					finished_at: "2026-01-01T00:00:00Z".to_string(),
					tool_latency_ms: Some(10),
					raw_tool_output: None,
					observation: None,
					interpreted_observation: None,
					execution_trace: None,
					remaining_step_budget_after: 3,
					remaining_recovery_budget_after: 2,
					working_directory_after: "/workspace".to_string(),
				},
				RuntimeLoopTraceStep {
					step_index: 2,
					decision: RuntimeLoopTraceDecision {
						action: "fail".to_string(),
						tool_name: None,
						arguments: None,
						reason: "fail after invalid trace".to_string(),
						final_message: Some("failed".to_string()),
					},
					visible_tools_before: vec![
						"command.run".to_string(),
						"general.execute".to_string(),
					],
					started_at: "2026-01-01T00:00:01Z".to_string(),
					finished_at: "2026-01-01T00:00:01Z".to_string(),
					tool_latency_ms: None,
					raw_tool_output: None,
					observation: Some(json!({"kind": "final_message", "final_message": "failed"})),
					interpreted_observation: None,
					execution_trace: None,
					remaining_step_budget_after: 2,
					remaining_recovery_budget_after: 2,
					working_directory_after: "/workspace".to_string(),
				},
			],
			final_outcome: RuntimeLoopTraceOutcome {
				status: "failed".to_string(),
				terminal_action: Some("fail".to_string()),
				final_message: Some("failed".to_string()),
			},
		};
		let report = pipeline.validate(&ResultEnvelope {
			task_id: TaskId("t1".to_string()),
			node_id: NodeId("n1".to_string()),
			producer: "runtime-loop".to_string(),
			schema_version: "result.v1".to_string(),
			status: ResultStatus::Ok,
			payload: json!({
				"message": "failed",
				"probe_trace": trace,
			})
			.to_string(),
			evidence: vec![EvidenceItem {
				kind: "runtime".to_string(),
				value: "runtime-loop".to_string(),
			}],
			confidence: 0.9,
		});

		assert!(!report.accepted);
		assert!(
			report
				.failures
				.iter()
				.any(|failure| failure.contains("raw_tool_output"))
		);
	}
}
