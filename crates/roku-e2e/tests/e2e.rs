use std::sync::Arc;

use actix_web::{App, web};
use roku_api_gateway::{
	GatewayAppState, RequestExecutor, RuntimeServiceExecutor, SubmitRequest, SubmitResponse,
	configure_routes,
};
use roku_cmd::{RunMode, run_once, run_with_mode};
use roku_common_types::{RequestEnvelope, ResponseStatus, RuntimeError};
use roku_runtime_service::RuntimeService;

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

#[actix_web::test]
async fn http_gateway_executes_runtime_service() {
	let service = Arc::new(RuntimeService::default());
	let state = web::Data::new(GatewayAppState::new(Arc::new(RuntimeServiceExecutor::new(
		service,
	))));
	let app = actix_web::test::init_service(
		App::new()
			.app_data(state)
			.app_data(web::JsonConfig::default().limit(8 * 1024))
			.configure(configure_routes),
	)
	.await;

	let request = actix_web::test::TestRequest::post()
		.uri("/v1/requests")
		.set_json(&SubmitRequest {
			session_id: "http-session".to_string(),
			goal: "build execution graph".to_string(),
		})
		.to_request();

	let response: SubmitResponse = actix_web::test::call_and_read_body_json(&app, request).await;
	assert_eq!(response.status, "succeeded");
	assert_eq!(response.message, "task succeeded");
	assert!(response.request_id.starts_with("req-"));
}

struct FixedModeExecutor {
	service: RuntimeService,
	mode: RunMode,
}

impl RequestExecutor for FixedModeExecutor {
	fn execute(
		&self,
		request: RequestEnvelope,
	) -> Result<roku_common_types::ResponseEnvelope, RuntimeError> {
		self.service.execute_with_mode(request, self.mode)
	}
}

#[actix_web::test]
async fn http_gateway_reports_validation_failure() {
	let executor = Arc::new(FixedModeExecutor {
		service: RuntimeService::default(),
		mode: RunMode::MissingEvidence,
	});
	let state = web::Data::new(GatewayAppState::new(executor));
	let app = actix_web::test::init_service(
		App::new()
			.app_data(state)
			.app_data(web::JsonConfig::default().limit(8 * 1024))
			.configure(configure_routes),
	)
	.await;

	let request = actix_web::test::TestRequest::post()
		.uri("/v1/requests")
		.set_json(&SubmitRequest {
			session_id: "http-session".to_string(),
			goal: "build execution graph".to_string(),
		})
		.to_request();

	let response: SubmitResponse = actix_web::test::call_and_read_body_json(&app, request).await;
	assert_eq!(response.status, "failed");
	assert!(response.message.contains("evidence is required"));
}
