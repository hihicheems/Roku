use roku_cmd::{RunMode, run_once, run_with_mode};
use roku_common_types::ResponseStatus;

#[test]
fn e2e_happy_path_succeeds() {
	let response = run_once("build execution graph").expect("pipeline should run");
	assert!(matches!(response.status, ResponseStatus::Succeeded));
}

#[test]
fn e2e_validation_failure_path_is_reported() {
	let response = run_with_mode("build execution graph", RunMode::MissingEvidence)
		.expect("pipeline should return failed response");
	assert!(matches!(response.status, ResponseStatus::Failed));
	assert!(response.message.contains("evidence is required"));
}

#[test]
fn e2e_capability_denied_path_is_reported() {
	let response = run_with_mode("build execution graph", RunMode::CapabilityDenied)
		.expect("pipeline should return failed response");
	assert!(matches!(response.status, ResponseStatus::Failed));
	assert!(response.message.contains("capability denied"));
}
