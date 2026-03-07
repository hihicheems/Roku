use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use roku_common_types::{
	ApprovalDecision, ApprovalId, ApprovalTicket, Artifact, ArtifactId, ExperimentRun,
	RequestEnvelope, RequestId, ResponseEnvelope, ResponseStatus, RuntimeError, TaskId,
};
use roku_runtime_service::RuntimeService;

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
			planning_mode_hint: None,
			conversation_history: Vec::new(),
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

pub trait TaskDataExecutor: Send + Sync {
	fn list_artifacts(&self, task_id: &TaskId) -> Result<Vec<Artifact>, RuntimeError>;
	fn get_experiment_run(&self, task_id: &TaskId) -> Result<Option<ExperimentRun>, RuntimeError>;
	fn get_artifact_content(
		&self,
		task_id: &TaskId,
		artifact_id: &ArtifactId,
	) -> Result<Option<String>, RuntimeError>;
}

pub trait GatewayExecutor: RequestExecutor + ApprovalExecutor + TaskDataExecutor {}

impl<T> GatewayExecutor for T where T: RequestExecutor + ApprovalExecutor + TaskDataExecutor {}

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

impl TaskDataExecutor for NoopExecutor {
	fn list_artifacts(&self, _task_id: &TaskId) -> Result<Vec<Artifact>, RuntimeError> {
		Ok(Vec::new())
	}

	fn get_experiment_run(&self, _task_id: &TaskId) -> Result<Option<ExperimentRun>, RuntimeError> {
		Ok(None)
	}

	fn get_artifact_content(
		&self,
		_task_id: &TaskId,
		_artifact_id: &ArtifactId,
	) -> Result<Option<String>, RuntimeError> {
		Ok(None)
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

impl TaskDataExecutor for RuntimeServiceExecutor {
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
