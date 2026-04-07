// Copyright 2025 itscheems
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use async_trait::async_trait;
use roku_common_types::{
	ApprovalDecision, ApprovalId, ApprovalTicket, Artifact, ArtifactId, ExperimentRun,
	RequestEnvelope, RequestId, ResponseEnvelope, ResponseStatus, RuntimeError, TaskId,
	TaskReplayReport,
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

#[async_trait]
pub trait RequestExecutor: Send + Sync {
	async fn execute(&self, request: RequestEnvelope) -> Result<ResponseEnvelope, RuntimeError>;
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
	fn get_task_replay_report(
		&self,
		task_id: &TaskId,
	) -> Result<Option<TaskReplayReport>, RuntimeError>;
}

pub trait GatewayExecutor: RequestExecutor + ApprovalExecutor + TaskDataExecutor {}

impl<T> GatewayExecutor for T where T: RequestExecutor + ApprovalExecutor + TaskDataExecutor {}

#[derive(Debug, Default)]
pub struct NoopExecutor;

#[async_trait]
impl RequestExecutor for NoopExecutor {
	async fn execute(&self, request: RequestEnvelope) -> Result<ResponseEnvelope, RuntimeError> {
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

	fn get_task_replay_report(
		&self,
		_task_id: &TaskId,
	) -> Result<Option<TaskReplayReport>, RuntimeError> {
		Ok(None)
	}
}

pub struct RuntimeServiceExecutor {
	service: Arc<RuntimeService>,
	/// Lazily created multi-thread runtime. Only initialized when running
	/// under actix's current-thread runtime (where `block_in_place` panics).
	/// In multi-thread contexts (roku-cmd, tests), requests run directly on
	/// the current runtime and this is never created — avoiding the
	/// "Cannot drop a runtime in async context" panic on cleanup.
	fallback_runtime: std::sync::OnceLock<Arc<tokio::runtime::Runtime>>,
}

impl RuntimeServiceExecutor {
	pub fn new(service: Arc<RuntimeService>) -> Self {
		Self {
			service,
			fallback_runtime: std::sync::OnceLock::new(),
		}
	}

	fn get_or_create_fallback_runtime(&self) -> &Arc<tokio::runtime::Runtime> {
		self.fallback_runtime.get_or_init(|| {
			Arc::new(
				tokio::runtime::Builder::new_multi_thread()
					.enable_all()
					.build()
					.expect("fallback multi-thread runtime for gateway executor"),
			)
		})
	}
}

#[async_trait]
impl RequestExecutor for RuntimeServiceExecutor {
	async fn execute(&self, request: RequestEnvelope) -> Result<ResponseEnvelope, RuntimeError> {
		let service = self.service.clone();
		// In a multi-thread runtime (roku-cmd, tests), block_in_place works
		// natively — run directly. Under actix's current-thread runtime,
		// delegate to a dedicated multi-thread runtime.
		if matches!(
			tokio::runtime::Handle::current().runtime_flavor(),
			tokio::runtime::RuntimeFlavor::MultiThread
		) {
			service.execute(request).await
		} else {
			self.get_or_create_fallback_runtime()
				.spawn(async move { service.execute(request).await })
				.await
				.map_err(|join_error| RuntimeError::new(join_error.to_string()))?
		}
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

	fn get_task_replay_report(
		&self,
		task_id: &TaskId,
	) -> Result<Option<TaskReplayReport>, RuntimeError> {
		self.service.get_task_replay_report(task_id)
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
