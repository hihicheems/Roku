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
