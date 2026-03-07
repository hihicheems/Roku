//! Gateway request normalization and HTTP integration.

mod executor;
mod models;
mod routes;

pub use executor::{
	ApprovalExecutor, Gateway, GatewayAppState, GatewayExecutor, NoopExecutor, RawRequest,
	RequestExecutor, RuntimeServiceExecutor, TaskDataExecutor,
};
pub use models::{
	ApprovalDecisionRequest, ApprovalTicketResponse, ArtifactResponse, ErrorResponse,
	ExperimentMetricResponse, ExperimentResponse, HealthResponse, SubmitRequest, SubmitResponse,
};
pub use routes::{
	configure_routes, decide_approval_handler, get_approval_handler, get_experiment_handler,
	get_task_artifacts_handler, health_handler, submit_handler,
};
