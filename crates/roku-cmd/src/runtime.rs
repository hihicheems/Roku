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

use roku_agent_runtime::GenericAgentRuntime;
use roku_api_gateway::{Gateway, RawRequest};
use roku_artifact_store::ArtifactStore;
use roku_common_types::{PlanningModeHint, ResponseEnvelope, RuntimeError};
use roku_experiment_registry::ExperimentRegistry;
use roku_llm_adapter::{OpenRouterConfig, build_openrouter_router_with_metrics};
use roku_observability::{InMemoryAuditSink, LogLevel, LogRecord, Metrics, emit_global_log};
pub use roku_runtime_service::RunMode;
use roku_runtime_service::RuntimeService;
use roku_state_store::{
	PostgresApprovalRepository, PostgresEventRepository, PostgresResultRepository,
	PostgresStoreConfig, PostgresTaskRepository,
};
use roku_task_planner::llm::LlmTaskPlanner;

use crate::CommandError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExecutionRequestOptions {
	pub session_id: String,
	pub goal: String,
	pub planning_mode_hint: Option<PlanningModeHint>,
}

pub fn run_once(goal: &str) -> Result<ResponseEnvelope, RuntimeError> {
	run_with_mode_and_options(
		ExecutionRequestOptions {
			session_id: "session-1".to_string(),
			goal: goal.to_string(),
			planning_mode_hint: None,
		},
		RunMode::Normal,
	)
}

pub fn run_with_mode(goal: &str, mode: RunMode) -> Result<ResponseEnvelope, RuntimeError> {
	run_with_mode_and_options(
		ExecutionRequestOptions {
			session_id: "session-1".to_string(),
			goal: goal.to_string(),
			planning_mode_hint: None,
		},
		mode,
	)
}

pub(crate) fn run_with_mode_and_options(
	options: ExecutionRequestOptions,
	mode: RunMode,
) -> Result<ResponseEnvelope, RuntimeError> {
	let gateway = Gateway;
	let service = RuntimeService::default();
	let request = build_request(&gateway, options, 1);

	service.execute_with_mode(request, mode)
}

pub fn run_live_once_from_env(goal: &str) -> Result<ResponseEnvelope, CommandError> {
	run_live_once_with_options_from_env(ExecutionRequestOptions {
		session_id: "session-1".to_string(),
		goal: goal.to_string(),
		planning_mode_hint: None,
	})
}

pub(crate) fn run_live_once_with_options_from_env(
	options: ExecutionRequestOptions,
) -> Result<ResponseEnvelope, CommandError> {
	let gateway = Gateway;
	let service = build_live_runtime_service_from_env()?;
	let request = build_request(&gateway, options, 1);
	service.execute(request).map_err(CommandError::Runtime)
}

pub(crate) fn build_live_runtime_service_from_env() -> Result<RuntimeService, CommandError> {
	let config = OpenRouterConfig::from_env()?;
	let store_config = PostgresStoreConfig::from_env()
		.map_err(|error| CommandError::StateStoreBootstrap(error.to_string()))?;
	let metrics = Arc::new(Metrics::default());
	let runtime_router = build_openrouter_router_with_metrics(config.clone(), metrics.clone())?;
	let planner_router = build_openrouter_router_with_metrics(config, metrics.clone())?;
	let runtime = GenericAgentRuntime::with_llm_router(runtime_router);
	let planner = Box::new(LlmTaskPlanner::new(planner_router));

	if let Some(store_config) = store_config {
		let _ = emit_global_log(
			LogRecord::new(
				"roku-cmd",
				LogLevel::Info,
				"using postgres-backed orchestration state store",
			)
			.with_field("schema", store_config.schema.clone()),
		);

		return Ok(RuntimeService::new_with_data_plane_and_runtime_and_metrics(
			Box::new(
				PostgresTaskRepository::connect(store_config.clone())
					.map_err(|error| CommandError::StateStoreBootstrap(error.to_string()))?,
			),
			Box::new(
				PostgresEventRepository::connect(store_config.clone())
					.map_err(|error| CommandError::StateStoreBootstrap(error.to_string()))?,
			),
			Box::new(
				PostgresApprovalRepository::connect(store_config.clone())
					.map_err(|error| CommandError::StateStoreBootstrap(error.to_string()))?,
			),
			Box::new(
				PostgresResultRepository::connect(store_config)
					.map_err(|error| CommandError::StateStoreBootstrap(error.to_string()))?,
			),
			ArtifactStore::default(),
			ExperimentRegistry::default(),
			Arc::new(InMemoryAuditSink::default()),
			runtime,
			metrics,
			planner,
		));
	}

	let _ = emit_global_log(LogRecord::new(
		"roku-cmd",
		LogLevel::Info,
		"using in-memory orchestration state store",
	));

	Ok(RuntimeService::in_memory_with_agent_runtime_planner_and_metrics(runtime, planner, metrics))
}

fn build_request(
	gateway: &Gateway,
	options: ExecutionRequestOptions,
	seq: u64,
) -> roku_common_types::RequestEnvelope {
	let mut request = gateway.normalize(
		RawRequest {
			session_id: options.session_id,
			goal: options.goal,
		},
		seq,
	);
	request.planning_mode_hint = options.planning_mode_hint;
	request
}
