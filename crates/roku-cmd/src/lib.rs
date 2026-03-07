//! Roku command runtime bootstrap.

use roku_api_gateway::{Gateway, RawRequest};
use roku_common_types::{ResponseEnvelope, RuntimeError};
pub use roku_runtime_service::RunMode;
use roku_runtime_service::RuntimeService;

pub fn run_once(goal: &str) -> Result<ResponseEnvelope, RuntimeError> {
	run_with_mode(goal, RunMode::Normal)
}

pub fn run_with_mode(goal: &str, mode: RunMode) -> Result<ResponseEnvelope, RuntimeError> {
	let gateway = Gateway;
	let service = RuntimeService::default();
	let request = gateway.normalize(
		RawRequest {
			session_id: "session-1".to_string(),
			goal: goal.to_string(),
		},
		1,
	);

	service.execute_with_mode(request, mode)
}

#[cfg(test)]
mod tests {
	use super::*;
	use roku_common_types::ResponseStatus;

	#[test]
	fn run_once_returns_success() {
		let response = run_once("analyze market").expect("pipeline should succeed");
		assert!(matches!(response.status, ResponseStatus::Succeeded));
	}

	#[test]
	fn run_with_missing_evidence_fails_validation() {
		let response = run_with_mode("analyze market", RunMode::MissingEvidence)
			.expect("pipeline should execute and fail validation");
		assert!(matches!(response.status, ResponseStatus::Failed));
		assert!(response.message.contains("evidence is required"));
	}

	#[test]
	fn run_with_capability_denied_fails() {
		let response = run_with_mode("analyze market", RunMode::CapabilityDenied)
			.expect("pipeline should execute and fail with capability denial");
		assert!(matches!(response.status, ResponseStatus::Failed));
		assert!(response.message.contains("capability denied"));
	}
}
