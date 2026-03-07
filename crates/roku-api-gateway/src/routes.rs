use std::sync::atomic::Ordering;

use actix_web::http::StatusCode;
use actix_web::{HttpResponse, Responder, web};
use roku_common_types::{ApprovalDecision, ApprovalId, RuntimeError, TaskId};

use crate::executor::{GatewayAppState, RawRequest};
use crate::models::{
	ApprovalDecisionRequest, ErrorResponse, HealthResponse, SubmitRequest, SubmitResponse,
	approval_ticket_response, artifact_response, experiment_response, response_status_label,
};

pub fn configure_routes(cfg: &mut web::ServiceConfig) {
	cfg.route("/health", web::get().to(health_handler));
	cfg.route("/v1/requests", web::post().to(submit_handler));
	cfg.route(
		"/v1/tasks/{task_id}/artifacts",
		web::get().to(get_task_artifacts_handler),
	);
	cfg.route(
		"/v1/tasks/{task_id}/experiment",
		web::get().to(get_experiment_handler),
	);
	cfg.route(
		"/v1/approvals/{approval_id}",
		web::get().to(get_approval_handler),
	);
	cfg.route(
		"/v1/approvals/{approval_id}/decision",
		web::post().to(decide_approval_handler),
	);
}

pub async fn health_handler() -> impl Responder {
	HttpResponse::Ok().json(HealthResponse { status: "ok" })
}

pub async fn submit_handler(
	state: web::Data<GatewayAppState>,
	request: web::Json<SubmitRequest>,
) -> impl Responder {
	let seq = state.sequence.fetch_add(1, Ordering::Relaxed);
	let envelope = state.gateway.normalize(
		RawRequest {
			session_id: request.session_id.clone(),
			goal: request.goal.clone(),
		},
		seq,
	);

	match state.executor.execute(envelope) {
		Ok(response) => HttpResponse::Ok().json(SubmitResponse {
			request_id: response.request_id.0,
			status: response_status_label(response.status).to_string(),
			message: response.message,
			artifacts: response.artifacts,
		}),
		Err(error) => HttpResponse::build(StatusCode::INTERNAL_SERVER_ERROR).json(SubmitResponse {
			request_id: String::new(),
			status: "failed".to_string(),
			message: error.to_string(),
			artifacts: Vec::new(),
		}),
	}
}

pub async fn get_task_artifacts_handler(
	state: web::Data<GatewayAppState>,
	task_id: web::Path<String>,
) -> impl Responder {
	let task_id = TaskId(task_id.into_inner());
	match state.executor.list_artifacts(&task_id) {
		Ok(artifacts) => HttpResponse::Ok()
			.json(artifacts.into_iter().map(artifact_response).collect::<Vec<_>>()),
		Err(error) => HttpResponse::BadRequest().json(ErrorResponse {
			message: error.to_string(),
		}),
	}
}

pub async fn get_experiment_handler(
	state: web::Data<GatewayAppState>,
	task_id: web::Path<String>,
) -> impl Responder {
	let task_id = TaskId(task_id.into_inner());
	match state.executor.get_experiment_run(&task_id) {
		Ok(Some(run)) => HttpResponse::Ok().json(experiment_response(run)),
		Ok(None) => HttpResponse::NotFound().json(ErrorResponse {
			message: "experiment run not found".to_string(),
		}),
		Err(error) => HttpResponse::BadRequest().json(ErrorResponse {
			message: error.to_string(),
		}),
	}
}

pub async fn get_approval_handler(
	state: web::Data<GatewayAppState>,
	approval_id: web::Path<String>,
) -> impl Responder {
	let approval_id = ApprovalId(approval_id.into_inner());
	match state.executor.get_approval(&approval_id) {
		Ok(Some(ticket)) => HttpResponse::Ok().json(approval_ticket_response(ticket)),
		Ok(None) => HttpResponse::NotFound().json(ErrorResponse {
			message: "approval ticket not found".to_string(),
		}),
		Err(error) => HttpResponse::BadRequest().json(ErrorResponse {
			message: error.to_string(),
		}),
	}
}

pub async fn decide_approval_handler(
	state: web::Data<GatewayAppState>,
	approval_id: web::Path<String>,
	decision: web::Json<ApprovalDecisionRequest>,
) -> impl Responder {
	let approval_id = ApprovalId(approval_id.into_inner());
	let decision = ApprovalDecision {
		actor: decision.actor.clone(),
		approved: decision.approved,
		comment: decision.comment.clone(),
	};

	match state.executor.decide_approval(&approval_id, decision) {
		Ok(response) => HttpResponse::Ok().json(SubmitResponse {
			request_id: response.request_id.0,
			status: response_status_label(response.status).to_string(),
			message: response.message,
			artifacts: response.artifacts,
		}),
		Err(error) => HttpResponse::build(approval_error_status(&error)).json(ErrorResponse {
			message: error.to_string(),
		}),
	}
}

fn approval_error_status(error: &RuntimeError) -> StatusCode {
	if error.message.contains("not found") {
		StatusCode::NOT_FOUND
	} else if error.message.contains("already been decided")
		|| error.message.contains("does not match")
		|| error.message.contains("not waiting for approval")
	{
		StatusCode::CONFLICT
	} else {
		StatusCode::BAD_REQUEST
	}
}

#[cfg(test)]
mod tests {
	use std::sync::Arc;

	use actix_web::{App, test};

	use super::*;
	use crate::executor::{GatewayAppState, NoopExecutor};
	use crate::models::{ArtifactResponse, ExperimentResponse};

	#[actix_web::test]
	async fn health_route_is_ok() {
		let app = test::init_service(App::new().configure(configure_routes)).await;
		let request = test::TestRequest::get().uri("/health").to_request();
		let response = test::call_service(&app, request).await;

		assert_eq!(response.status(), StatusCode::OK);
	}

	#[actix_web::test]
	async fn submit_route_maps_request() {
		let state = web::Data::new(GatewayAppState::new(Arc::new(NoopExecutor)));
		let app = test::init_service(
			App::new()
				.app_data(state)
				.app_data(web::JsonConfig::default().limit(8 * 1024))
				.configure(configure_routes),
		)
		.await;

		let request = test::TestRequest::post()
			.uri("/v1/requests")
			.set_json(&SubmitRequest {
				session_id: "s1".to_string(),
				goal: "g1".to_string(),
			})
			.to_request();

		let response: SubmitResponse = test::call_and_read_body_json(&app, request).await;
		assert_eq!(response.status, "succeeded");
		assert_eq!(response.message, "accepted");
	}

	#[actix_web::test]
	async fn task_data_routes_map_executor_results() {
		let state = web::Data::new(GatewayAppState::new(Arc::new(NoopExecutor)));
		let app = test::init_service(
			App::new()
				.app_data(state)
				.app_data(web::JsonConfig::default().limit(8 * 1024))
				.configure(configure_routes),
		)
		.await;

		let artifact_request = test::TestRequest::get()
			.uri("/v1/tasks/task-1/artifacts")
			.to_request();
		let artifact_response: Vec<ArtifactResponse> =
			test::call_and_read_body_json(&app, artifact_request).await;
		assert!(artifact_response.is_empty());

		let experiment_request = test::TestRequest::get()
			.uri("/v1/tasks/task-1/experiment")
			.to_request();
		let experiment_response = test::call_service(&app, experiment_request).await;
		assert_eq!(experiment_response.status(), StatusCode::NOT_FOUND);

		let body: ErrorResponse = test::read_body_json(experiment_response).await;
		assert_eq!(body.message, "experiment run not found");
		let _: Option<ExperimentResponse> = None;
	}
}
