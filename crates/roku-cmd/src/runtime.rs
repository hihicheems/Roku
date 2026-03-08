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
use roku_common_types::{
	ApprovalDecision, ApprovalId, PlanningModeHint, ResponseEnvelope, RuntimeError, TaskId,
};
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
use serde_json::json;

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
	let metrics = Arc::new(Metrics::default());
	let runtime_router = build_openrouter_router_with_metrics(config.clone(), metrics.clone())?;
	let planner_router = build_openrouter_router_with_metrics(config, metrics.clone())?;
	let runtime = GenericAgentRuntime::with_llm_router(runtime_router);
	let planner = Box::new(LlmTaskPlanner::new(planner_router));
	let store_config = PostgresStoreConfig::from_env()
		.map_err(|error| CommandError::StateStoreBootstrap(error.to_string()))?;

	if let Some(store_config) = store_config {
		return Ok(RuntimeService::new_with_data_plane_and_runtime_and_metrics(
			Box::new(connect_postgres_task_repository(&store_config)?),
			Box::new(connect_postgres_event_repository(&store_config)?),
			Box::new(connect_postgres_approval_repository(&store_config)?),
			Box::new(connect_postgres_result_repository(&store_config)?),
			ArtifactStore::default(),
			ExperimentRegistry::default(),
			Arc::new(InMemoryAuditSink::default()),
			runtime,
			metrics,
			planner,
		));
	}

	log_state_store_backend("in-memory", None);

	Ok(RuntimeService::in_memory_with_agent_runtime_planner_and_metrics(runtime, planner, metrics))
}

pub(crate) fn show_task_from_env(task_id: &str) -> Result<String, CommandError> {
	let service = build_stateful_runtime_service_from_env()?;
	let task_id = TaskId(task_id.to_string());
	let task = service
		.get_task(&task_id)
		.map_err(CommandError::Runtime)?
		.ok_or_else(|| CommandError::Usage(format!("task not found: {}", task_id.0)))?;
	let events = service
		.list_task_events(&task_id)
		.map_err(CommandError::Runtime)?;

	serde_json::to_string_pretty(&json!({
		"task": task,
		"events": events,
	}))
	.map_err(|error| CommandError::OutputEncoding(error.to_string()))
}

pub(crate) fn show_approval_from_env(approval_id: &str) -> Result<String, CommandError> {
	let service = build_stateful_runtime_service_from_env()?;
	let approval_id = ApprovalId(approval_id.to_string());
	let ticket = service
		.get_approval(&approval_id)
		.map_err(CommandError::Runtime)?
		.ok_or_else(|| CommandError::Usage(format!("approval not found: {}", approval_id.0)))?;

	serde_json::to_string_pretty(&ticket)
		.map_err(|error| CommandError::OutputEncoding(error.to_string()))
}

pub(crate) fn decide_approval_from_env(
	approval_id: &str,
	decision: ApprovalDecision,
) -> Result<String, CommandError> {
	let service = build_live_runtime_service_from_env()?;
	let response = service
		.decide_approval(&ApprovalId(approval_id.to_string()), decision)
		.map_err(CommandError::Runtime)?;

	serde_json::to_string_pretty(&response)
		.map_err(|error| CommandError::OutputEncoding(error.to_string()))
}

fn build_stateful_runtime_service_from_env() -> Result<RuntimeService, CommandError> {
	let store_config = PostgresStoreConfig::from_env()
		.map_err(|error| CommandError::StateStoreBootstrap(error.to_string()))?;

	if let Some(store_config) = store_config {
		return Ok(RuntimeService::new_with_data_plane(
			Box::new(connect_postgres_task_repository(&store_config)?),
			Box::new(connect_postgres_event_repository(&store_config)?),
			Box::new(connect_postgres_approval_repository(&store_config)?),
			Box::new(connect_postgres_result_repository(&store_config)?),
			ArtifactStore::default(),
			ExperimentRegistry::default(),
			Arc::new(InMemoryAuditSink::default()),
		));
	}

	log_state_store_backend("in-memory", None);
	Ok(RuntimeService::default())
}

fn connect_postgres_task_repository(
	config: &PostgresStoreConfig,
) -> Result<PostgresTaskRepository, CommandError> {
	log_state_store_backend("postgres", Some(config.schema.as_str()));
	PostgresTaskRepository::connect(config.clone())
		.map_err(|error| CommandError::StateStoreBootstrap(error.to_string()))
}

fn connect_postgres_event_repository(
	config: &PostgresStoreConfig,
) -> Result<PostgresEventRepository, CommandError> {
	PostgresEventRepository::connect(config.clone())
		.map_err(|error| CommandError::StateStoreBootstrap(error.to_string()))
}

fn connect_postgres_approval_repository(
	config: &PostgresStoreConfig,
) -> Result<PostgresApprovalRepository, CommandError> {
	PostgresApprovalRepository::connect(config.clone())
		.map_err(|error| CommandError::StateStoreBootstrap(error.to_string()))
}

fn connect_postgres_result_repository(
	config: &PostgresStoreConfig,
) -> Result<PostgresResultRepository, CommandError> {
	PostgresResultRepository::connect(config.clone())
		.map_err(|error| CommandError::StateStoreBootstrap(error.to_string()))
}

fn log_state_store_backend(kind: &str, schema: Option<&str>) {
	let mut record = LogRecord::new(
		"roku-cmd",
		LogLevel::Info,
		format!("using {kind} orchestration state store"),
	);
	if let Some(schema) = schema {
		record = record.with_field("schema", schema.to_string());
	}
	let _ = emit_global_log(record);
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
