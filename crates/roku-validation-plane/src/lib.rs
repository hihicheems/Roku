//! Validation pipeline for child results.

use roku_common_types::{ResultEnvelope, ResultStatus, ValidationReport};

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
		let mut failures = Vec::new();

		self.run_schema_checks(result, &mut failures);
		self.run_semantic_checks(result, &mut failures);
		self.run_provenance_checks(result, &mut failures);
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

	fn run_provenance_checks(&self, result: &ResultEnvelope, failures: &mut Vec<String>) {
		if self.config.require_evidence && result.evidence.is_empty() {
			failures.push("evidence is required".to_string());
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
	use roku_common_types::{EvidenceItem, NodeId, ResultEnvelope, ResultStatus, TaskId};

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
		let report = pipeline.validate(&ResultEnvelope {
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
		});

		assert!(report.accepted);
	}
}
