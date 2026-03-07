use roku_common_types::ValidationEvidenceSet;

use crate::ValidationConfig;

pub(crate) fn run_provenance_checks(
	config: &ValidationConfig,
	evidence_set: &ValidationEvidenceSet,
	failures: &mut Vec<String>,
) {
	let result = &evidence_set.result;
	if config.require_evidence && result.evidence.is_empty() {
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
