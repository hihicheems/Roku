//! Gateway request normalization and HTTP integration.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use actix_web::http::StatusCode;
use actix_web::{HttpResponse, Responder, web};
use roku_common_types::{
	RequestEnvelope, RequestId, ResponseEnvelope, ResponseStatus, RuntimeError,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct RawRequest {
	pub session_id: String,
	pub goal: String,
}

#[derive(Debug, Default)]
pub struct Gateway;

impl Gateway {
	pub fn normalize(&self, raw: RawRequest, seq: u64) -> RequestEnvelope {
		RequestEnvelope {
			request_id: RequestId(format!("req-{seq}")),
			session_id: raw.session_id,
			goal: raw.goal,
		}
	}
}

pub trait RequestExecutor: Send + Sync {
	fn execute(&self, request: RequestEnvelope) -> Result<ResponseEnvelope, RuntimeError>;
}

#[derive(Debug, Default)]
pub struct NoopExecutor;

impl RequestExecutor for NoopExecutor {
	fn execute(&self, request: RequestEnvelope) -> Result<ResponseEnvelope, RuntimeError> {
		Ok(ResponseEnvelope {
			request_id: request.request_id,
			status: ResponseStatus::Succeeded,
			message: "accepted".to_string(),
			artifacts: Vec::new(),
		})
	}
}

pub struct GatewayAppState {
	pub gateway: Gateway,
	pub executor: Arc<dyn RequestExecutor>,
	pub sequence: AtomicU64,
}

impl GatewayAppState {
	pub fn new(executor: Arc<dyn RequestExecutor>) -> Self {
		Self {
			gateway: Gateway,
			executor,
			sequence: AtomicU64::new(1),
		}
	}
}

#[derive(Debug, Clone, Serialize)]
pub struct HealthResponse {
	pub status: &'static str,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitRequest {
	pub session_id: String,
	pub goal: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitResponse {
	pub request_id: String,
	pub status: String,
	pub message: String,
	pub artifacts: Vec<String>,
}

pub fn configure_routes(cfg: &mut web::ServiceConfig) {
	cfg.route("/health", web::get().to(health_handler));
	cfg.route("/v1/requests", web::post().to(submit_handler));
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

fn response_status_label(status: ResponseStatus) -> &'static str {
	match status {
		ResponseStatus::Succeeded => "succeeded",
		ResponseStatus::Failed => "failed",
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use actix_web::{App, test};

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
}
