//! Gateway request normalization and HTTP integration.

mod executor;
mod models;
mod routes;

pub use executor::{
	ApprovalExecutor, Gateway, GatewayAppState, GatewayExecutor, NoopExecutor, RawRequest,
	RequestExecutor, RuntimeServiceExecutor,
};
pub use models::{
	ApprovalDecisionRequest, ApprovalTicketResponse, ErrorResponse, HealthResponse, SubmitRequest,
	SubmitResponse,
};
pub use routes::{
	configure_routes, decide_approval_handler, get_approval_handler, health_handler, submit_handler,
};
