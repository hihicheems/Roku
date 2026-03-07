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
		ResultStatus, TaskId, ValidationEvidenceSet,
	};

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
}
