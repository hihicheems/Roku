//! Gateway request normalization and HTTP integration.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use actix_web::http::StatusCode;
use actix_web::{HttpResponse, Responder, web};
use roku_common_types::{
	ApprovalDecision, ApprovalId, ApprovalStatus, ApprovalTicket, RequestEnvelope, RequestId,
	ResponseEnvelope, ResponseStatus, RuntimeError,
};
use roku_runtime_service::RuntimeService;
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

pub trait ApprovalExecutor: Send + Sync {
	fn get_approval(
		&self,
		approval_id: &ApprovalId,
	) -> Result<Option<ApprovalTicket>, RuntimeError>;
	fn decide_approval(
		&self,
		approval_id: &ApprovalId,
		decision: ApprovalDecision,
	) -> Result<ResponseEnvelope, RuntimeError>;
}

pub trait GatewayExecutor: RequestExecutor + ApprovalExecutor {}

impl<T> GatewayExecutor for T where T: RequestExecutor + ApprovalExecutor {}

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

impl ApprovalExecutor for NoopExecutor {
	fn get_approval(
		&self,
		_approval_id: &ApprovalId,
	) -> Result<Option<ApprovalTicket>, RuntimeError> {
		Ok(None)
	}

	fn decide_approval(
		&self,
		_approval_id: &ApprovalId,
		_decision: ApprovalDecision,
	) -> Result<ResponseEnvelope, RuntimeError> {
		Err(RuntimeError::new("approval executor is not configured"))
	}
}

pub struct RuntimeServiceExecutor {
	service: Arc<RuntimeService>,
}

impl RuntimeServiceExecutor {
	pub fn new(service: Arc<RuntimeService>) -> Self {
		Self { service }
	}
}

impl RequestExecutor for RuntimeServiceExecutor {
	fn execute(&self, request: RequestEnvelope) -> Result<ResponseEnvelope, RuntimeError> {
		self.service.execute(request)
	}
}

impl ApprovalExecutor for RuntimeServiceExecutor {
	fn get_approval(
		&self,
		approval_id: &ApprovalId,
	) -> Result<Option<ApprovalTicket>, RuntimeError> {
		self.service.get_approval(approval_id)
	}

	fn decide_approval(
		&self,
		approval_id: &ApprovalId,
		decision: ApprovalDecision,
	) -> Result<ResponseEnvelope, RuntimeError> {
		self.service.decide_approval(approval_id, decision)
	}
}

pub struct GatewayAppState {
	pub gateway: Gateway,
	pub executor: Arc<dyn GatewayExecutor>,
	pub sequence: AtomicU64,
}

impl GatewayAppState {
	pub fn new(executor: Arc<dyn GatewayExecutor>) -> Self {
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalDecisionRequest {
	pub actor: String,
	pub approved: bool,
	pub comment: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalTicketResponse {
	pub approval_id: String,
	pub task_id: String,
	pub request_id: String,
	pub node_id: String,
	pub status: String,
	pub summary: String,
	pub decided_by: Option<String>,
	pub comment: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorResponse {
	pub message: String,
}

pub fn configure_routes(cfg: &mut web::ServiceConfig) {
	cfg.route("/health", web::get().to(health_handler));
	cfg.route("/v1/requests", web::post().to(submit_handler));
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

fn response_status_label(status: ResponseStatus) -> &'static str {
	match status {
		ResponseStatus::Succeeded => "succeeded",
		ResponseStatus::PendingApproval => "pending_approval",
		ResponseStatus::Failed => "failed",
	}
}

fn approval_ticket_response(ticket: ApprovalTicket) -> ApprovalTicketResponse {
	ApprovalTicketResponse {
		approval_id: ticket.approval_id.0,
		task_id: ticket.task_id.0,
		request_id: ticket.request_id.0,
		node_id: ticket.node_id.0,
		status: approval_status_label(ticket.status).to_string(),
		summary: ticket.summary,
		decided_by: ticket.decided_by,
		comment: ticket.comment,
	}
}

fn approval_status_label(status: ApprovalStatus) -> &'static str {
	match status {
		ApprovalStatus::Pending => "pending",
		ApprovalStatus::Approved => "approved",
		ApprovalStatus::Rejected => "rejected",
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
