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

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use roku_agent_runtime::{GenericAgentRuntime, PluginRegistrySnapshot, ToolCatalogConfig};
use roku_api_gateway::{Gateway, RawRequest};
use roku_artifact_store::ArtifactStore;
use roku_common_types::{
	ApprovalDecision, ApprovalId, ArtifactId, PlanningModeHint, ResponseEnvelope, RuntimeError,
	TaskId,
};
use roku_experiment_registry::ExperimentRegistry;
use roku_observability::{InMemoryAuditSink, LogLevel, LogRecord, Metrics, emit_global_log};
use roku_plugin_core::{PluginDisableReason, PluginPolicyConfig};
use roku_plugin_host::{
	PluginDiscoveryConfig, PluginStartupConfig, build_plugin_registry_snapshot,
	default_bundled_plugin_descriptors,
};
use roku_plugin_llm::build_openrouter_router_with_metrics;
use roku_plugin_skills::{SkillRegistry, SkillsRuntimeConfig};
pub use roku_runtime_service::RunMode;
use roku_runtime_service::{RuntimeModeReport, RuntimeService};
use roku_state_store::{
	SqliteApprovalRepository, SqliteDispatchQueue, SqliteEventRepository, SqliteResultRepository,
	SqliteStoreConfig, SqliteTaskRepository,
};
use serde_json::json;

use crate::CommandError;
use crate::runtime_config::{RuntimeConfigs, load_runtime_configs};
use crate::storage::LocalStorageLayout;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExecutionRequestOptions {
	pub session_id: String,
	pub goal: String,
	pub planning_mode_hint: Option<PlanningModeHint>,
	pub generated_skill_root: Option<std::path::PathBuf>,
}

pub fn run_once(goal: &str) -> Result<ResponseEnvelope, RuntimeError> {
	run_with_mode_and_options(
		ExecutionRequestOptions {
			session_id: "session-1".to_string(),
			goal: goal.to_string(),
			planning_mode_hint: None,
			generated_skill_root: None,
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
			generated_skill_root: None,
		},
		mode,
	)
}

pub(crate) fn run_with_mode_and_options(
	options: ExecutionRequestOptions,
	mode: RunMode,
) -> Result<ResponseEnvelope, RuntimeError> {
	apply_request_env_overrides(&options);
	let gateway = Gateway;
	let service = build_deterministic_runtime_service_from_env()
		.map_err(|error| RuntimeError::new(error.to_string()))?;
	let request = build_request(&gateway, options, next_cli_request_sequence());
	execute_with_service_and_mode(service, request, mode)
}

pub fn run_live_once_from_env(goal: &str) -> Result<ResponseEnvelope, CommandError> {
	run_live_once_with_options_from_env(ExecutionRequestOptions {
		session_id: "session-1".to_string(),
		goal: goal.to_string(),
		planning_mode_hint: None,
		generated_skill_root: None,
	})
}

pub(crate) fn run_live_once_with_options_from_env(
	options: ExecutionRequestOptions,
) -> Result<ResponseEnvelope, CommandError> {
	apply_request_env_overrides(&options);
	let gateway = Gateway;
	let service = build_live_runtime_service_from_env()?;
	let request = build_request(&gateway, options, next_cli_request_sequence());
	execute_with_service_and_mode(service, request, RunMode::Normal).map_err(CommandError::Runtime)
}

pub(crate) fn build_live_runtime_service_from_env() -> Result<RuntimeService, CommandError> {
	let (layout, bootstrap) = build_plugin_bootstrap_from_env()?;
	build_live_runtime_service_from_layout_and_bootstrap(&layout, bootstrap)
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

pub(crate) fn install_skill_from_env(source_url: &str) -> Result<String, CommandError> {
	let registry = build_skill_registry_from_env()?;
	format_install_skill_report(&registry, source_url, "cli")
}

pub(crate) fn show_skills_from_env() -> Result<String, CommandError> {
	let registry = build_skill_registry_from_env()?;
	format_skill_list(&registry)
}

pub(crate) fn show_skill_from_env(skill_name: &str) -> Result<String, CommandError> {
	let registry = build_skill_registry_from_env()?;
	format_skill_detail(&registry, skill_name)
}

fn format_install_skill_report(
	registry: &SkillRegistry,
	source_url: &str,
	activated_by: &str,
) -> Result<String, CommandError> {
	let report = registry.install_from_url(source_url, activated_by)?;
	serde_json::to_string_pretty(&report)
		.map_err(|error| CommandError::OutputEncoding(error.to_string()))
}

fn format_skill_list(registry: &SkillRegistry) -> Result<String, CommandError> {
	let skills = registry.list_skills()?;
	serde_json::to_string_pretty(&skills)
		.map_err(|error| CommandError::OutputEncoding(error.to_string()))
}

fn format_skill_detail(registry: &SkillRegistry, skill_name: &str) -> Result<String, CommandError> {
	let record = registry.get_skill(skill_name)?;
	let prompt_context = registry.render_prompt_context_for_skill(skill_name, 16_000)?;
	serde_json::to_string_pretty(&json!({
		"record": record,
		"prompt_context": prompt_context,
	}))
	.map_err(|error| CommandError::OutputEncoding(error.to_string()))
}

pub(crate) fn replay_task_from_env(task_id: &str) -> Result<String, CommandError> {
	let service = build_stateful_runtime_service_from_env()?;
	let task_id = TaskId(task_id.to_string());
	let report = service
		.get_task_replay_report(&task_id)
		.map_err(CommandError::Runtime)?
		.ok_or_else(|| CommandError::Usage(format!("task not found: {}", task_id.0)))?;

	serde_json::to_string_pretty(&report)
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
	let (layout, bootstrap) = build_plugin_bootstrap_from_env()?;
	let runtime =
		GenericAgentRuntime::with_skill_registry_tool_config_and_plugin_snapshot_and_runtime_config(
			bootstrap.skill_registry,
			bootstrap.tool_config,
			bootstrap.plugin_snapshot,
			bootstrap.runtime_configs.tools,
			bootstrap.runtime_configs.agent,
		);
	let store_config = sqlite_store_config(&layout);
	let (artifact_store, experiment_registry) = build_runtime_data_plane(&layout);

	Ok(RuntimeService::new_with_runtime_data_plane_and_metrics(
		roku_runtime_service::RuntimeDataPlane {
			task_repo: Box::new(connect_sqlite_task_repository(&store_config)?),
			event_repo: Box::new(connect_sqlite_event_repository(&store_config)?),
			approval_repo: Box::new(connect_sqlite_approval_repository(&store_config)?),
			result_repo: Box::new(connect_sqlite_result_repository(&store_config)?),
			dispatch_queue: Box::new(connect_sqlite_dispatch_queue(&store_config)?),
			artifact_store,
			experiment_registry,
		},
		Arc::new(InMemoryAuditSink::default()),
		runtime,
		Arc::new(Metrics::default()),
	))
}

fn build_skill_registry_from_env() -> Result<SkillRegistry, CommandError> {
	Ok(build_plugin_bootstrap_from_env()?.1.skill_registry)
}

fn load_tool_catalog_config(
	layout: &LocalStorageLayout,
) -> Result<ToolCatalogConfig, CommandError> {
	if !layout.tool_config_path.exists() {
		return Ok(ToolCatalogConfig::default());
	}
	ToolCatalogConfig::from_path(&layout.tool_config_path).map_err(CommandError::from)
}

fn load_plugin_policy_config(
	layout: &LocalStorageLayout,
) -> Result<PluginPolicyConfig, CommandError> {
	if !layout.plugin_config_path.exists() {
		return Ok(PluginPolicyConfig::default());
	}
	let content = fs::read_to_string(&layout.plugin_config_path).map_err(CommandError::Io)?;
	PluginPolicyConfig::from_toml(&content)
		.map_err(roku_plugin_host::PluginHostError::from)
		.map_err(CommandError::from)
}

fn connect_sqlite_task_repository(
	config: &SqliteStoreConfig,
) -> Result<SqliteTaskRepository, CommandError> {
	log_state_store_backend("sqlite", &config.path);
	SqliteTaskRepository::connect(config.clone())
		.map_err(|error| CommandError::StateStoreBootstrap(error.to_string()))
}

fn connect_sqlite_event_repository(
	config: &SqliteStoreConfig,
) -> Result<SqliteEventRepository, CommandError> {
	SqliteEventRepository::connect(config.clone())
		.map_err(|error| CommandError::StateStoreBootstrap(error.to_string()))
}

fn connect_sqlite_approval_repository(
	config: &SqliteStoreConfig,
) -> Result<SqliteApprovalRepository, CommandError> {
	SqliteApprovalRepository::connect(config.clone())
		.map_err(|error| CommandError::StateStoreBootstrap(error.to_string()))
}

fn connect_sqlite_result_repository(
	config: &SqliteStoreConfig,
) -> Result<SqliteResultRepository, CommandError> {
	SqliteResultRepository::connect(config.clone())
		.map_err(|error| CommandError::StateStoreBootstrap(error.to_string()))
}

fn connect_sqlite_dispatch_queue(
	config: &SqliteStoreConfig,
) -> Result<SqliteDispatchQueue, CommandError> {
	SqliteDispatchQueue::connect(config.clone())
		.map_err(|error| CommandError::StateStoreBootstrap(error.to_string()))
}

fn log_state_store_backend(kind: &str, path: &Path) {
	let mut record = LogRecord::new(
		"roku-cmd",
		LogLevel::Info,
		format!("using {kind} orchestration state store"),
	);
	record = record.with_field("path", path.display().to_string());
	let _ = emit_global_log(record);
}

fn build_runtime_data_plane(layout: &LocalStorageLayout) -> (ArtifactStore, ExperimentRegistry) {
	log_data_plane_backend("artifact-store", &layout.artifact_root);
	log_data_plane_backend("experiment-registry", &layout.experiment_root);
	(
		ArtifactStore::file_backed(layout.artifact_root.clone()),
		ExperimentRegistry::file_backed(layout.experiment_root.clone()),
	)
}

fn log_data_plane_backend(component: &str, path: &Path) {
	let _ = emit_global_log(
		LogRecord::new(
			"roku-cmd",
			LogLevel::Info,
			format!("using local file-backed {component}"),
		)
		.with_field("path", path.display().to_string()),
	);
}

#[derive(Clone)]
pub(crate) struct PluginBootstrap {
	pub(crate) plugin_snapshot: PluginRegistrySnapshot,
	pub(crate) tool_config: ToolCatalogConfig,
	pub(crate) skill_registry: SkillRegistry,
	pub(crate) runtime_configs: RuntimeConfigs,
}

pub(crate) fn build_plugin_bootstrap_from_env()
-> Result<(LocalStorageLayout, PluginBootstrap), CommandError> {
	let layout = LocalStorageLayout::from_env();
	layout.ensure_dirs().map_err(CommandError::Io)?;
	let bootstrap = build_plugin_bootstrap(&layout)?;
	Ok((layout, bootstrap))
}

fn build_plugin_bootstrap(layout: &LocalStorageLayout) -> Result<PluginBootstrap, CommandError> {
	let tool_config = load_tool_catalog_config(layout)?;
	let runtime_configs = load_runtime_configs(layout)?;
	let policy = load_plugin_policy_config(layout)?;
	let discovery = PluginDiscoveryConfig {
		explicit_paths: policy.paths.clone(),
		env_root: plugin_root_override_from_env(),
		workspace_root: layout.workspace_plugin_root.clone(),
		user_root: layout.user_plugin_root.clone(),
	};
	let bundled_descriptors = default_bundled_plugin_descriptors(
		&tool_config
			.tools
			.iter()
			.map(|tool| tool.name.clone())
			.collect::<Vec<_>>(),
	);
	let startup = PluginStartupConfig {
		discovery,
		policy,
		bundled_descriptors,
	};
	let plugin_snapshot = build_plugin_registry_snapshot(&startup)?;
	let skill_registry =
		build_skill_registry(layout, &plugin_snapshot, runtime_configs.skills.clone());

	Ok(PluginBootstrap {
		plugin_snapshot,
		tool_config,
		skill_registry,
		runtime_configs,
	})
}

pub(crate) fn ensure_plugin_enabled_for_command(
	plugin_snapshot: &PluginRegistrySnapshot,
	plugin_id: &str,
	command_name: &str,
) -> Result<(), CommandError> {
	if plugin_snapshot.is_plugin_enabled(plugin_id) {
		return Ok(());
	}

	let detail = plugin_snapshot
		.entry(plugin_id)
		.and_then(|entry| entry.disable_reason.as_ref())
		.map(|reason| format!("{reason:?}"))
		.unwrap_or_else(|| "not registered in startup inventory".to_string());
	Err(CommandError::Usage(format!(
		"{command_name} requires the `{plugin_id}` plugin, but it is unavailable ({detail})"
	)))
}

fn build_skill_registry(
	layout: &LocalStorageLayout,
	plugin_snapshot: &PluginRegistrySnapshot,
	skills_runtime_config: SkillsRuntimeConfig,
) -> SkillRegistry {
	if plugin_snapshot.is_plugin_enabled("skill-source-local") {
		log_data_plane_backend("skill-registry", &layout.skill_root);
		SkillRegistry::file_backed_with_config(layout.skill_root.clone(), skills_runtime_config)
	} else {
		let _ = emit_global_log(LogRecord::new(
			"roku-cmd",
			LogLevel::Warn,
			"local skill source plugin is disabled; skill registry will remain disabled",
		));
		SkillRegistry::disabled()
	}
}

fn build_deterministic_runtime_service_from_env() -> Result<RuntimeService, CommandError> {
	let (_, bootstrap) = build_plugin_bootstrap_from_env()?;
	let runtime =
		GenericAgentRuntime::with_skill_registry_tool_config_and_plugin_snapshot_and_runtime_config(
			bootstrap.skill_registry,
			bootstrap.tool_config,
			bootstrap.plugin_snapshot,
			bootstrap.runtime_configs.tools,
			bootstrap.runtime_configs.agent,
		);
	log_runtime_bootstrap_mode(&RuntimeModeReport::deterministic());
	Ok(RuntimeService::in_memory_with_agent_runtime(runtime)
		.with_runtime_mode_report(RuntimeModeReport::deterministic()))
}

pub(crate) fn build_live_runtime_service_from_layout_and_bootstrap(
	layout: &LocalStorageLayout,
	bootstrap: PluginBootstrap,
) -> Result<RuntimeService, CommandError> {
	let metrics = Arc::new(Metrics::default());
	let (runtime, runtime_mode) = build_live_runtime(bootstrap.clone(), metrics.clone())?;
	let store_config = sqlite_store_config(layout);
	let (artifact_store, experiment_registry) = build_runtime_data_plane(layout);

	Ok(RuntimeService::new_with_runtime_data_plane_and_metrics(
		roku_runtime_service::RuntimeDataPlane {
			task_repo: Box::new(connect_sqlite_task_repository(&store_config)?),
			event_repo: Box::new(connect_sqlite_event_repository(&store_config)?),
			approval_repo: Box::new(connect_sqlite_approval_repository(&store_config)?),
			result_repo: Box::new(connect_sqlite_result_repository(&store_config)?),
			dispatch_queue: Box::new(connect_sqlite_dispatch_queue(&store_config)?),
			artifact_store,
			experiment_registry,
		},
		Arc::new(InMemoryAuditSink::default()),
		runtime,
		metrics,
	)
	.with_runtime_mode_report(runtime_mode))
}

fn build_live_runtime(
	mut bootstrap: PluginBootstrap,
	metrics: Arc<Metrics>,
) -> Result<(GenericAgentRuntime, RuntimeModeReport), CommandError> {
	if !bootstrap.plugin_snapshot.is_plugin_enabled("openrouter") {
		let runtime_mode = RuntimeModeReport::live_react_fallback_to_deterministic(
			"openrouter plugin disabled by startup policy",
		);
		log_runtime_bootstrap_mode(&runtime_mode);
		return Ok((
			GenericAgentRuntime::with_skill_registry_tool_config_and_plugin_snapshot_and_runtime_config(
				bootstrap.skill_registry,
				bootstrap.tool_config,
				bootstrap.plugin_snapshot,
				bootstrap.runtime_configs.tools,
				bootstrap.runtime_configs.agent,
			),
			runtime_mode,
		));
	}

	let config = match openrouter_api_key_from_env() {
		Ok(api_key) => bootstrap
			.runtime_configs
			.openrouter
			.clone()
			.with_api_key(api_key),
		Err(error) => {
			let fallback_reason = format!("openrouter bootstrap failed: {error}");
			bootstrap.plugin_snapshot = bootstrap.plugin_snapshot.with_runtime_disable(
				"openrouter",
				PluginDisableReason::AdmissionRejected {
					detail: fallback_reason.clone(),
				},
			);
			let runtime_mode =
				RuntimeModeReport::live_react_fallback_to_deterministic(fallback_reason);
			log_runtime_bootstrap_mode(&runtime_mode);
			return Ok((
				GenericAgentRuntime::with_skill_registry_tool_config_and_plugin_snapshot_and_runtime_config(
					bootstrap.skill_registry,
					bootstrap.tool_config,
					bootstrap.plugin_snapshot,
					bootstrap.runtime_configs.tools,
					bootstrap.runtime_configs.agent,
				),
				runtime_mode,
			));
		}
	};
	let route_router = build_openrouter_router_with_metrics(config.clone(), metrics.clone())?;
	let execution_router = build_openrouter_router_with_metrics(config, metrics)?;
	let runtime_mode = RuntimeModeReport::live_react();
	log_runtime_bootstrap_mode(&runtime_mode);
	Ok((
		GenericAgentRuntime::with_route_and_execution_routers_skill_registry_tool_config_and_plugin_snapshot_and_runtime_config(
			route_router,
			execution_router,
			bootstrap.skill_registry,
			bootstrap.tool_config,
			bootstrap.plugin_snapshot,
			bootstrap.runtime_configs.tools,
			bootstrap.runtime_configs.agent,
		),
		runtime_mode,
	))
}

pub(crate) fn openrouter_api_key_from_env()
-> Result<String, roku_plugin_llm::OpenRouterBootstrapError> {
	std::env::var("OPENROUTER_API_KEY")
		.ok()
		.map(|value| value.trim().to_string())
		.filter(|value| !value.is_empty())
		.ok_or(roku_plugin_llm::OpenRouterBootstrapError::MissingEnv(
			"OPENROUTER_API_KEY",
		))
}

fn log_runtime_bootstrap_mode(runtime_mode: &RuntimeModeReport) {
	let mut record = LogRecord::new(
		"roku-cmd",
		if runtime_mode.fallback_reason.is_some() {
			LogLevel::Warn
		} else {
			LogLevel::Info
		},
		"runtime bootstrap resolved execution mode",
	)
	.with_field(
		"requested_runtime_mode",
		runtime_mode.requested.as_str().to_string(),
	)
	.with_field(
		"effective_runtime_mode",
		runtime_mode.effective.as_str().to_string(),
	);
	if let Some(reason) = runtime_mode.fallback_reason.as_deref() {
		record = record.with_field("fallback_reason", reason.to_string());
	}
	let _ = emit_global_log(record);
}

fn execute_with_service_and_mode(
	service: RuntimeService,
	request: roku_common_types::RequestEnvelope,
	mode: RunMode,
) -> Result<ResponseEnvelope, RuntimeError> {
	let runtime_mode = service.runtime_mode_report();
	let response = service.execute_with_mode(request, mode)?;
	Ok(annotate_cli_response_with_runtime_mode(
		response,
		&runtime_mode,
	))
}

fn annotate_cli_response_with_runtime_mode(
	mut response: ResponseEnvelope,
	runtime_mode: &RuntimeModeReport,
) -> ResponseEnvelope {
	let mut banner = format!(
		"[runtime requested={} effective={}]",
		runtime_mode.requested.as_str(),
		runtime_mode.effective.as_str()
	);
	if let Some(reason) = runtime_mode.fallback_reason.as_deref() {
		let sanitized_reason = reason.replace('\n', " ");
		banner.push_str(&format!(" fallback_reason={sanitized_reason}"));
	}
	response.message = format!("{banner}\n{}", response.message);
	response
}

fn plugin_root_override_from_env() -> Option<std::path::PathBuf> {
	std::env::var("ROKU_PLUGIN_ROOT")
		.ok()
		.filter(|value| !value.trim().is_empty())
		.map(|value| {
			let path = expand_home_path(value.trim());
			if path.is_absolute() {
				path
			} else {
				std::env::current_dir()
					.map(|cwd| cwd.join(path))
					.unwrap_or_else(|_| std::path::PathBuf::from("."))
			}
		})
}

fn expand_home_path(value: &str) -> std::path::PathBuf {
	if value == "~" {
		return std::env::var_os("HOME")
			.map(std::path::PathBuf::from)
			.unwrap_or_else(|| std::path::PathBuf::from(value));
	}
	if let Some(suffix) = value.strip_prefix("~/")
		&& let Some(home) = std::env::var_os("HOME")
	{
		return std::path::PathBuf::from(home).join(suffix);
	}
	std::path::PathBuf::from(value)
}

fn sqlite_store_config(layout: &LocalStorageLayout) -> SqliteStoreConfig {
	SqliteStoreConfig::new(layout.sqlite_path.clone())
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

fn next_cli_request_sequence() -> u64 {
	SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.map(|duration| duration.as_millis().min(u64::MAX as u128) as u64)
		.unwrap_or(1)
}

pub(crate) fn apply_request_env_overrides(options: &ExecutionRequestOptions) {
	if let Some(path) = &options.generated_skill_root {
		unsafe {
			std::env::set_var("ROKU_SKILL_ROOT", path);
			std::env::set_var("ROKU_GENERATED_SKILL_ROOT", path);
		}
	}
}

#[cfg(test)]
mod tests {
	use std::io::{Cursor, Write};
	use std::sync::Arc;

	use roku_common_types::{RequestId, ResponseEnvelope, ResponseStatus};
	use roku_plugin_skills::{
		DownloadedArchive, SkillArchiveFetcher, SkillRegistryError, SkillSource,
	};
	use serde_json::Value;

	use super::*;

	#[derive(Clone)]
	struct StaticArchiveFetcher {
		archive: DownloadedArchive,
	}

	impl SkillArchiveFetcher for StaticArchiveFetcher {
		fn fetch(&self, _source: &SkillSource) -> Result<DownloadedArchive, SkillRegistryError> {
			Ok(self.archive.clone())
		}
	}

	#[test]
	fn format_skill_list_and_detail_after_install() {
		let registry = test_registry();
		registry
			.install_from_url(
				"https://github.com/anthropics/skills/tree/main/skills/claude-api",
				"test-suite",
			)
			.expect("install should succeed");

		let list = format_skill_list(&registry).expect("skill list should render");
		let list_json: Value = serde_json::from_str(&list).expect("list should be valid json");
		assert_eq!(list_json.as_array().expect("list should be array").len(), 1);
		assert_eq!(list_json[0]["descriptor"]["name"], "claude-api");

		let detail =
			format_skill_detail(&registry, "claude api").expect("skill detail should render");
		let detail_json: Value =
			serde_json::from_str(&detail).expect("detail should be valid json");
		assert_eq!(detail_json["record"]["descriptor"]["name"], "claude-api");
		assert!(
			detail_json["prompt_context"]
				.as_str()
				.expect("prompt context should be string")
				.contains("### skill: claude-api")
		);
	}

	#[test]
	fn format_install_skill_report_returns_json_message() {
		let registry = test_registry();
		let output = format_install_skill_report(
			&registry,
			"https://github.com/anthropics/skills/tree/main/skills/claude-api",
			"cli-test",
		)
		.expect("install report should render");
		let json: Value = serde_json::from_str(&output).expect("output should be valid json");
		assert_eq!(json["skill_name"], "claude-api");
		assert!(
			json["message"]
				.as_str()
				.expect("message should be string")
				.contains("Reference `claude-api`")
		);
	}

	#[test]
	fn annotate_cli_response_marks_live_fallback_as_effective_deterministic() {
		let response = ResponseEnvelope {
			request_id: RequestId("req-fallback".to_string()),
			status: ResponseStatus::Succeeded,
			message: "placeholder response body".to_string(),
			artifacts: Vec::new(),
		};

		let annotated = annotate_cli_response_with_runtime_mode(
			response,
			&RuntimeModeReport::live_react_fallback_to_deterministic(
				"openrouter plugin disabled by startup policy",
			),
		);

		assert!(
			annotated
				.message
				.contains("[runtime requested=live-react effective=deterministic]")
		);
		assert!(
			annotated
				.message
				.contains("fallback_reason=openrouter plugin disabled by startup policy")
		);
	}

	fn test_registry() -> SkillRegistry {
		let root = tempfile::tempdir().expect("temp root should exist");
		SkillRegistry::file_backed(root.keep()).with_fetcher(Arc::new(StaticArchiveFetcher {
			archive: DownloadedArchive {
				archive_url: "https://example.com/archive.zip".to_string(),
				bytes: test_skill_archive_bytes(),
				resolved_reference: Some("main".to_string()),
			},
		}))
	}

	fn test_skill_archive_bytes() -> Vec<u8> {
		let mut cursor = Cursor::new(Vec::new());
		{
			let mut writer = zip::ZipWriter::new(&mut cursor);
			let options = zip::write::SimpleFileOptions::default();
			writer
				.add_directory("skills-main/skills/claude-api/", options)
				.expect("dir should be added");
			writer
				.add_directory("skills-main/skills/claude-api/shared/", options)
				.expect("shared dir should be added");
			writer
				.start_file("skills-main/skills/claude-api/SKILL.md", options)
				.expect("skill file should start");
			writer
				.write_all(
					br#"---
name: claude-api
description: Build apps with the Claude API.
---

# Claude API Skill

Use this skill when the user explicitly asks for Claude API integration help.
"#,
				)
				.expect("skill markdown should write");
			writer
				.start_file("skills-main/skills/claude-api/shared/models.md", options)
				.expect("support file should start");
			writer
				.write_all(b"Use claude-opus-4-6 unless the user asks otherwise.")
				.expect("support file should write");
			writer.finish().expect("zip should finish");
		}
		cursor.into_inner()
	}
}
