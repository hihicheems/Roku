use roku_common_types::ResultEnvelope;

use crate::ValidationConfig;

pub(crate) fn run_policy_checks(
	config: &ValidationConfig,
	result: &ResultEnvelope,
	failures: &mut Vec<String>,
) {
	if result.confidence < config.min_confidence {
		failures.push("confidence below minimum".to_string());
	}
}
