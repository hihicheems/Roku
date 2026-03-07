use std::sync::Arc;

use actix_web::{App, web};
use roku_api_gateway::{
	ApprovalDecisionRequest, ApprovalExecutor, ApprovalTicketResponse, ArtifactContentResponse,
	ArtifactResponse, ExperimentResponse, GatewayAppState, RequestExecutor, RuntimeServiceExecutor,
	SubmitRequest, SubmitResponse, TaskDataExecutor, configure_routes,
};
use roku_cmd::{RunMode, run_once, run_with_mode};
use roku_common_types::{
	ApprovalDecision, ApprovalId, Artifact, ArtifactId, ExperimentRun, RequestEnvelope,
	ResponseStatus, RuntimeError, TaskId,
};
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

#[test]
fn e2e_pending_approval_path_is_reported() {
	let response = run_with_mode("build execution graph", RunMode::ApprovalRequired)
		.expect("pipeline should return pending approval response");
	assert!(matches!(response.status, ResponseStatus::PendingApproval));
	assert!(response.message.contains("approval required"));
}

#[test]
fn e2e_dead_letter_path_is_reported() {
	let response = run_with_mode("build execution graph", RunMode::RetryExhausted)
		.expect("pipeline should return dead-letter response");
	assert!(matches!(response.status, ResponseStatus::Failed));
	assert!(response.message.contains("dead-lettered"));
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
	assert!(!response.message.is_empty());
	assert_eq!(response.message, "generic execution completed");
	assert!(response.request_id.starts_with("req-"));
}

#[actix_web::test]
async fn http_gateway_exposes_task_artifacts_and_experiment() {
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
	let task_id = format!("task-{}", response.request_id);

	let artifacts_request = actix_web::test::TestRequest::get()
		.uri(&format!("/v1/tasks/{task_id}/artifacts"))
		.to_request();
	let artifacts: Vec<ArtifactResponse> =
		actix_web::test::call_and_read_body_json(&app, artifacts_request).await;
	assert_eq!(artifacts.len(), 2);
	assert!(
		artifacts
			.iter()
			.all(|artifact| artifact.uri.starts_with("artifact://"))
	);
	let artifact_id = artifacts
		.first()
		.expect("artifact should exist")
		.artifact_id
		.clone();

	let content_request = actix_web::test::TestRequest::get()
		.uri(&format!(
			"/v1/tasks/{task_id}/artifacts/{artifact_id}/content"
		))
		.to_request();
	let content_response: ArtifactContentResponse =
		actix_web::test::call_and_read_body_json(&app, content_request).await;
	assert_eq!(content_response.artifact_id, artifact_id);
	assert!(!content_response.content.is_empty());

	let download_request = actix_web::test::TestRequest::get()
		.uri(&format!(
			"/v1/tasks/{task_id}/artifacts/{artifact_id}/download"
		))
		.to_request();
	let download_body = actix_web::test::call_and_read_body(&app, download_request).await;
	assert!(!download_body.is_empty());

	let experiment_request = actix_web::test::TestRequest::get()
		.uri(&format!("/v1/tasks/{task_id}/experiment"))
		.to_request();
	let experiment: ExperimentResponse =
		actix_web::test::call_and_read_body_json(&app, experiment_request).await;
	assert_eq!(experiment.status, "succeeded");
	assert_eq!(experiment.artifact_ids.len(), 2);
}

struct FixedModeExecutor {
	service: Arc<RuntimeService>,
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

impl ApprovalExecutor for FixedModeExecutor {
	fn get_approval(
		&self,
		approval_id: &ApprovalId,
	) -> Result<Option<roku_common_types::ApprovalTicket>, RuntimeError> {
		self.service.get_approval(approval_id)
	}

	fn decide_approval(
		&self,
		approval_id: &ApprovalId,
		decision: ApprovalDecision,
	) -> Result<roku_common_types::ResponseEnvelope, RuntimeError> {
		self.service.decide_approval(approval_id, decision)
	}
}

impl TaskDataExecutor for FixedModeExecutor {
	fn list_artifacts(&self, task_id: &TaskId) -> Result<Vec<Artifact>, RuntimeError> {
		self.service.list_artifacts(task_id)
	}

	fn get_experiment_run(&self, task_id: &TaskId) -> Result<Option<ExperimentRun>, RuntimeError> {
		self.service.get_experiment_run(task_id)
	}

	fn get_artifact_content(
		&self,
		task_id: &TaskId,
		artifact_id: &ArtifactId,
	) -> Result<Option<String>, RuntimeError> {
		self.service.get_artifact_content(task_id, artifact_id)
	}
}

#[actix_web::test]
async fn http_gateway_reports_validation_failure() {
	let executor = Arc::new(FixedModeExecutor {
		service: Arc::new(RuntimeService::default()),
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

#[actix_web::test]
async fn http_gateway_reports_pending_approval() {
	let executor = Arc::new(FixedModeExecutor {
		service: Arc::new(RuntimeService::default()),
		mode: RunMode::ApprovalRequired,
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
	assert_eq!(response.status, "pending_approval");
	assert!(response.message.contains("approval required"));
}

#[actix_web::test]
async fn http_gateway_approval_roundtrip_resumes_task() {
	let service = Arc::new(RuntimeService::default());
	let executor = Arc::new(FixedModeExecutor {
		service: service.clone(),
		mode: RunMode::ApprovalRequired,
	});
	let state = web::Data::new(GatewayAppState::new(executor));
	let app = actix_web::test::init_service(
		App::new()
			.app_data(state)
			.app_data(web::JsonConfig::default().limit(8 * 1024))
			.configure(configure_routes),
	)
	.await;

	let submit_request = actix_web::test::TestRequest::post()
		.uri("/v1/requests")
		.set_json(&SubmitRequest {
			session_id: "http-session".to_string(),
			goal: "build execution graph".to_string(),
		})
		.to_request();

	let pending: SubmitResponse =
		actix_web::test::call_and_read_body_json(&app, submit_request).await;
	assert_eq!(pending.status, "pending_approval");
	let approval_id = pending.artifacts[0]
		.trim_start_matches("approval://")
		.to_string();

	let get_request = actix_web::test::TestRequest::get()
		.uri(&format!("/v1/approvals/{approval_id}"))
		.to_request();
	let ticket: ApprovalTicketResponse =
		actix_web::test::call_and_read_body_json(&app, get_request).await;
	assert_eq!(ticket.status, "pending");

	let decision_request = actix_web::test::TestRequest::post()
		.uri(&format!("/v1/approvals/{approval_id}/decision"))
		.set_json(&ApprovalDecisionRequest {
			actor: "reviewer".to_string(),
			approved: true,
			comment: Some("ship it".to_string()),
		})
		.to_request();
	let resumed: SubmitResponse =
		actix_web::test::call_and_read_body_json(&app, decision_request).await;
	assert_eq!(resumed.status, "succeeded");

	let get_request = actix_web::test::TestRequest::get()
		.uri(&format!("/v1/approvals/{approval_id}"))
		.to_request();
	let ticket: ApprovalTicketResponse =
		actix_web::test::call_and_read_body_json(&app, get_request).await;
	assert_eq!(ticket.status, "approved");
	assert_eq!(ticket.decided_by.as_deref(), Some("reviewer"));
}
