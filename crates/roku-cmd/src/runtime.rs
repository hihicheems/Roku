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

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use roku_agent_runtime::GenericAgentRuntime;
use roku_api_gateway::{Gateway, RawRequest};
use roku_artifact_store::ArtifactStore;
use roku_common_types::{
	ApprovalDecision, ApprovalId, ArtifactId, PlanningModeHint, ResponseEnvelope, RuntimeError,
	TaskId, TaskState,
};
use roku_experiment_registry::ExperimentRegistry;
use roku_llm_adapter::{OpenRouterConfig, build_openrouter_router_with_metrics};
use roku_observability::{InMemoryAuditSink, LogLevel, LogRecord, Metrics, emit_global_log};
use roku_orchestrator::is_valid_transition;
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
	let (artifact_store, experiment_registry) = build_runtime_data_plane_from_env();

	if let Some(store_config) = store_config {
		return Ok(RuntimeService::new_with_data_plane_and_runtime_and_metrics(
			Box::new(connect_postgres_task_repository(&store_config)?),
			Box::new(connect_postgres_event_repository(&store_config)?),
			Box::new(connect_postgres_approval_repository(&store_config)?),
			Box::new(connect_postgres_result_repository(&store_config)?),
			artifact_store,
			experiment_registry,
			Arc::new(InMemoryAuditSink::default()),
			runtime,
			metrics,
			planner,
		));
	}

	log_state_store_backend("in-memory", None);

	Ok(RuntimeService::new_with_data_plane_and_runtime_and_metrics(
		Box::new(roku_state_store::InMemoryTaskRepository::default()),
		Box::new(roku_state_store::InMemoryEventRepository::default()),
		Box::new(roku_state_store::InMemoryApprovalRepository::default()),
		Box::new(roku_state_store::InMemoryResultRepository::default()),
		artifact_store,
		experiment_registry,
		Arc::new(InMemoryAuditSink::default()),
		runtime,
		metrics,
		planner,
	))
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

pub(crate) fn show_artifacts_from_env(task_id: &str) -> Result<String, CommandError> {
	let service = build_stateful_runtime_service_from_env()?;
	let artifacts = service
		.list_artifacts(&TaskId(task_id.to_string()))
		.map_err(CommandError::Runtime)?;

	serde_json::to_string_pretty(&artifacts)
		.map_err(|error| CommandError::OutputEncoding(error.to_string()))
}

pub(crate) fn show_artifact_content_from_env(
	task_id: &str,
	artifact_id: &str,
) -> Result<String, CommandError> {
	let service = build_stateful_runtime_service_from_env()?;
	service
		.get_artifact_content(
			&TaskId(task_id.to_string()),
			&ArtifactId(artifact_id.to_string()),
		)
		.map_err(CommandError::Runtime)?
		.ok_or_else(|| {
			CommandError::Usage(format!(
				"artifact content not found for task={} artifact={artifact_id}",
				task_id
			))
		})
}

pub(crate) fn download_artifact_from_env(
	task_id: &str,
	artifact_id: &str,
	output_path: &Path,
) -> Result<String, CommandError> {
	let content = show_artifact_content_from_env(task_id, artifact_id)?;
	if let Some(parent) = output_path.parent() {
		fs::create_dir_all(parent).map_err(CommandError::Io)?;
	}
	fs::write(output_path, content).map_err(CommandError::Io)?;
	Ok(output_path.display().to_string())
}

pub(crate) fn show_experiment_from_env(task_id: &str) -> Result<String, CommandError> {
	let service = build_stateful_runtime_service_from_env()?;
	let experiment = service
		.get_experiment_run(&TaskId(task_id.to_string()))
		.map_err(CommandError::Runtime)?
		.ok_or_else(|| CommandError::Usage(format!("experiment not found for task: {task_id}")))?;

	serde_json::to_string_pretty(&experiment)
		.map_err(|error| CommandError::OutputEncoding(error.to_string()))
}

pub(crate) fn replay_task_from_env(task_id: &str) -> Result<String, CommandError> {
	let service = build_stateful_runtime_service_from_env()?;
	let task_id = TaskId(task_id.to_string());
	let task = service
		.get_task(&task_id)
		.map_err(CommandError::Runtime)?
		.ok_or_else(|| CommandError::Usage(format!("task not found: {}", task_id.0)))?;
	let events = service
		.list_task_events(&task_id)
		.map_err(CommandError::Runtime)?;

	let replayed_state = events.last().map(|event| event.to).unwrap_or(task.state);
	let transitions_valid = events
		.iter()
		.all(|event| is_valid_transition(event.from, event.to));
	let chain_consistent = events
		.windows(2)
		.all(|window| window[0].to == window[1].from);
	let snapshot_matches_replay = task.state == replayed_state;
	let recoverable = is_recoverable_state(task.state);

	serde_json::to_string_pretty(&json!({
		"task_id": task.task_id.0,
		"persisted_state": format!("{:?}", task.state),
		"replayed_state": format!("{:?}", replayed_state),
		"event_count": events.len(),
		"transitions_valid": transitions_valid,
		"chain_consistent": chain_consistent,
		"snapshot_matches_replay": snapshot_matches_replay,
		"recoverable": recoverable,
		"events": events,
	}))
	.map_err(|error| CommandError::OutputEncoding(error.to_string()))
}

pub(crate) fn resume_task_from_env(task_id: &str) -> Result<String, CommandError> {
	let service = build_live_runtime_service_from_env()?;
	let response = service
		.resume_task(&TaskId(task_id.to_string()))
		.map_err(CommandError::Runtime)?;

	serde_json::to_string_pretty(&response)
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
	let (artifact_store, experiment_registry) = build_runtime_data_plane_from_env();

	if let Some(store_config) = store_config {
		return Ok(RuntimeService::new_with_data_plane(
			Box::new(connect_postgres_task_repository(&store_config)?),
			Box::new(connect_postgres_event_repository(&store_config)?),
			Box::new(connect_postgres_approval_repository(&store_config)?),
			Box::new(connect_postgres_result_repository(&store_config)?),
			artifact_store,
			experiment_registry,
			Arc::new(InMemoryAuditSink::default()),
		));
	}

	log_state_store_backend("in-memory", None);
	Ok(RuntimeService::new_with_data_plane(
		Box::new(roku_state_store::InMemoryTaskRepository::default()),
		Box::new(roku_state_store::InMemoryEventRepository::default()),
		Box::new(roku_state_store::InMemoryApprovalRepository::default()),
		Box::new(roku_state_store::InMemoryResultRepository::default()),
		artifact_store,
		experiment_registry,
		Arc::new(InMemoryAuditSink::default()),
	))
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

fn build_runtime_data_plane_from_env() -> (ArtifactStore, ExperimentRegistry) {
	let config = RuntimeDataPlaneConfig::from_env();
	log_data_plane_backend("artifact-store", &config.artifact_store_path);
	log_data_plane_backend("experiment-registry", &config.experiment_registry_path);
	(
		ArtifactStore::file_backed(config.artifact_store_path),
		ExperimentRegistry::file_backed(config.experiment_registry_path),
	)
}

fn log_data_plane_backend(component: &str, path: &Path) {
	let _ = emit_global_log(
		LogRecord::new(
			"roku-cmd",
			LogLevel::Info,
			format!("using file-backed {component}"),
		)
		.with_field("path", path.display().to_string()),
	);
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RuntimeDataPlaneConfig {
	artifact_store_path: PathBuf,
	experiment_registry_path: PathBuf,
}

impl RuntimeDataPlaneConfig {
	fn from_env() -> Self {
		let base_dir = env::var("ROKU_RUNTIME_DATA_DIR")
			.ok()
			.filter(|value| !value.trim().is_empty())
			.map(PathBuf::from)
			.unwrap_or_else(|| PathBuf::from("state"));
		let artifact_store_path = env::var("ROKU_ARTIFACT_STORE_PATH")
			.ok()
			.filter(|value| !value.trim().is_empty())
			.map(PathBuf::from)
			.unwrap_or_else(|| base_dir.join("artifacts.json"));
		let experiment_registry_path = env::var("ROKU_EXPERIMENT_REGISTRY_PATH")
			.ok()
			.filter(|value| !value.trim().is_empty())
			.map(PathBuf::from)
			.unwrap_or_else(|| base_dir.join("experiments.json"));

		Self {
			artifact_store_path,
			experiment_registry_path,
		}
	}
}

fn is_recoverable_state(state: TaskState) -> bool {
	matches!(
		state,
		TaskState::Planning
			| TaskState::GraphBuilding
			| TaskState::Delegating
			| TaskState::Executing
			| TaskState::WaitingApproval
			| TaskState::Validating
			| TaskState::Aggregating
			| TaskState::Failed
	)
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
