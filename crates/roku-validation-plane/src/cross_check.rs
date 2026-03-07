use std::collections::HashSet;

use roku_common_types::{ResultStatus, ValidationEvidenceSet};

use crate::ValidationConfig;

pub(crate) fn run_cross_checks(
	config: &ValidationConfig,
	evidence_set: &ValidationEvidenceSet,
	failures: &mut Vec<String>,
) {
	if !config.enable_cross_checks {
		return;
	}

	let result = &evidence_set.result;
	let mut seen_artifact_refs = HashSet::new();
	for artifact_ref in result
		.evidence
		.iter()
		.filter(|item| item.kind == "artifact_ref")
		.map(|item| item.value.as_str())
	{
		if !seen_artifact_refs.insert(artifact_ref.to_string()) {
			failures.push(format!(
				"duplicate artifact evidence detected: {artifact_ref}"
			));
		}
	}

	for artifact in &evidence_set.artifacts {
		if artifact.schema_version != result.schema_version {
			failures.push(format!(
				"artifact schema {} does not match result schema {}",
				artifact.schema_version, result.schema_version
			));
		}
	}

	if matches!(result.status, ResultStatus::Error) && result.confidence > 0.5 {
		failures.push("error status result cannot have confidence above 0.5".to_string());
	}
}
