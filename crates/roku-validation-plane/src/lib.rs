//! Validation pipeline for child results.

use roku_common_types::{ResultEnvelope, ResultStatus, ValidationReport};

#[derive(Debug, Clone)]
pub struct ValidationConfig {
	pub require_evidence: bool,
	pub min_confidence: f32,
}

impl Default for ValidationConfig {
	fn default() -> Self {
		Self {
			require_evidence: true,
			min_confidence: 0.1,
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

		if result.schema_version.trim().is_empty() {
			failures.push("schema_version is empty".to_string());
		}

		if matches!(result.status, ResultStatus::Ok) && result.payload.trim().is_empty() {
			failures.push("payload is empty for ok status".to_string());
		}

		if self.config.require_evidence && result.evidence.is_empty() {
			failures.push("evidence is required".to_string());
		}

		if result.confidence < self.config.min_confidence {
			failures.push("confidence below minimum".to_string());
		}

		ValidationReport {
			accepted: failures.is_empty(),
			failures,
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
}
