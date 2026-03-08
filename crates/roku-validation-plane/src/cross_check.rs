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
