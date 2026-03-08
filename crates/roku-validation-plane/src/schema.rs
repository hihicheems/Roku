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

use roku_common_types::{ResultEnvelope, ResultStatus};

use crate::ValidationConfig;

pub(crate) fn run_schema_checks(
	config: &ValidationConfig,
	result: &ResultEnvelope,
	failures: &mut Vec<String>,
) {
	if result.schema_version.trim().is_empty() {
		failures.push("schema_version is empty".to_string());
	}

	if matches!(result.status, ResultStatus::Ok) && result.payload.trim().is_empty() {
		failures.push("payload is empty for ok status".to_string());
	}

	if config.require_non_empty_producer && result.producer.trim().is_empty() {
		failures.push("producer is empty".to_string());
	}
}
