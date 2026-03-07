//! Validation pipeline for child results.

use roku_common_types::{ResultEnvelope, ResultStatus, ValidationEvidenceSet, ValidationReport};

#[derive(Debug, Clone)]
pub struct ValidationConfig {
	pub require_evidence: bool,
	pub min_confidence: f32,
	pub require_non_empty_producer: bool,
}

impl Default for ValidationConfig {
	fn default() -> Self {
		Self {
			require_evidence: true,
			min_confidence: 0.1,
			require_non_empty_producer: true,
		}
	}
}

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

		self.run_schema_checks(result, &mut failures);
		self.run_semantic_checks(result, &mut failures);
		self.run_provenance_checks(evidence_set, &mut failures);
		self.run_policy_checks(result, &mut failures);

		ValidationReport {
			accepted: failures.is_empty(),
			failures,
		}
	}

	fn run_schema_checks(&self, result: &ResultEnvelope, failures: &mut Vec<String>) {
		if result.schema_version.trim().is_empty() {
			failures.push("schema_version is empty".to_string());
		}

		if matches!(result.status, ResultStatus::Ok) && result.payload.trim().is_empty() {
			failures.push("payload is empty for ok status".to_string());
		}

		if self.config.require_non_empty_producer && result.producer.trim().is_empty() {
			failures.push("producer is empty".to_string());
		}
	}

	fn run_semantic_checks(&self, result: &ResultEnvelope, failures: &mut Vec<String>) {
		if result.schema_version.starts_with("backtest_report.v1")
			&& matches!(result.status, ResultStatus::Ok)
		{
			match serde_json::from_str::<serde_json::Value>(&result.payload) {
				Ok(value) => {
					for field in ["annual_return", "sharpe", "max_drawdown"] {
						if value.get(field).is_none() {
							failures.push(format!("backtest payload missing field: {field}"));
						}
					}
				}
				Err(error) => failures.push(format!("backtest payload is not valid json: {error}")),
			}
		}
	}

	fn run_provenance_checks(
		&self,
		evidence_set: &ValidationEvidenceSet,
		failures: &mut Vec<String>,
	) {
		let result = &evidence_set.result;
		if self.config.require_evidence && result.evidence.is_empty() {
			failures.push("evidence is required".to_string());
		}

		for evidence in result
			.evidence
			.iter()
			.filter(|item| item.kind == "artifact_ref")
		{
			match evidence_set
				.artifacts
				.iter()
				.find(|artifact| artifact.uri == evidence.value)
			{
				Some(artifact) => {
					if artifact.task_id != result.task_id {
						failures.push(format!(
							"artifact {} does not belong to task {}",
							artifact.uri, result.task_id.0
						));
					}
					if artifact.node_id != result.node_id {
						failures.push(format!(
							"artifact {} does not belong to node {}",
							artifact.uri, result.node_id.0
						));
					}
				}
				None => failures.push(format!(
					"artifact evidence could not be resolved: {}",
					evidence.value
				)),
			}
		}
	}

	fn run_policy_checks(&self, result: &ResultEnvelope, failures: &mut Vec<String>) {
		if result.confidence < self.config.min_confidence {
			failures.push("confidence below minimum".to_string());
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use roku_common_types::{
		Artifact, ArtifactId, ArtifactMetadataEntry, EvidenceItem, NodeId, ResultEnvelope,
		ResultStatus, TaskId, ValidationEvidenceSet,
	};

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
			artifacts: vec![Artifact {
				artifact_id: ArtifactId("artifact-1".to_string()),
				task_id: TaskId("t1".to_string()),
				node_id: NodeId("n1".to_string()),
				kind: "node_result".to_string(),
				uri: "artifact://1".to_string(),
				schema_version: "backtest_report.v1".to_string(),
				checksum: "bytes:53".to_string(),
				metadata: vec![ArtifactMetadataEntry {
					key: "producer".to_string(),
					value: "agent".to_string(),
				}],
			}],
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
}
