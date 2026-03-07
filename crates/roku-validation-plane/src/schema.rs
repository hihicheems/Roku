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
