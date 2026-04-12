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

//! Runtime bootstrap and command-facing execution adapters.
//!
//! This module translates CLI/env inputs into concrete runtime-service instances and request
//! envelopes. It owns process-local bootstrap concerns such as plugin discovery, config loading,
//! entry/runtime bundle resolution, and mode selection. It does not decide
//! agent behavior inside a run once the request has entered the runtime loop.
//!
//! For memory specifically, this module now delegates adapter selection to the
//! Roku-owned entry registry and keeps only composition-root duties such as
//! config loading and service startup.

use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

pub use roku_agent_runtime::RunMode;
use roku_agent_runtime::{GenericAgentRuntime, PluginRegistrySnapshot, ToolCatalogConfig};
use roku_agent_runtime::{RuntimeModeReport, RuntimeService};
use roku_api_gateway::{Gateway, RawRequest};
use roku_common_types::{ApprovalDecision, ApprovalId, ResponseEnvelope, RuntimeError, TaskId};
use roku_common_types::{InMemoryAuditSink, LogLevel, LogRecord, Metrics, emit_global_log};
use roku_memory::{
	ConservativeMemoryLifecyclePolicy, DisabledMemoryLifecyclePolicy, LongTermMemoryBackend,
	MemoryBackendHealth, MemoryDeleteSelector, MemoryError, MemoryLifecyclePolicy, MemoryQuery,
	MemoryWriteRequest,
};
use roku_plugin_host::{
	PluginDisableReason, PluginDiscoveryConfig, PluginPolicyConfig, PluginStartupConfig,
	build_plugin_registry_snapshot, default_bundled_plugin_descriptors,
};
use roku_plugin_llm::{
	AnthropicBootstrapError, AnthropicRuntimeConfig, LlmProviderKind, LlmRouter,
	OpenAiBootstrapError, OpenAiRuntimeConfig, OpenRouterBootstrapError, OpenRouterRuntimeConfig,
	anthropic_api_key_from_env, build_anthropic_router_with_metrics,
	build_openai_router_with_metrics, build_openrouter_router_with_metrics,
	openai_api_key_from_env,
};
use roku_plugin_mcp::{McpConfig, McpConnection, McpTool, mcp_tools_to_catalog_descriptors};
use roku_plugin_skills::{SkillRegistry, SkillsRuntimeConfig};
use serde_json::json;

use crate::CommandError;
use crate::entry_registry::{resolve_entry_runtime_bundle, resolve_memory_subsystem};
use crate::memory_runtime_config::MemoryRuntimeConfig;
use crate::pending_loop_substrate::MemoryPendingLoopSnapshotStore;
use crate::runtime_config::{
	RuntimeConfigs, load_runtime_configs, prepare_runtime_generated_artifacts,
};
use crate::storage::LocalStorageLayout;

/// Canonical request options shared by CLI entrypoints before a runtime request is normalized.
///
/// This stays slightly above `RequestEnvelope`: CLI-only compatibility hints and env overrides can
/// be represented here without leaking command-surface concerns into the gateway contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExecutionRequestOptions {
	pub session_id: String,
	pub goal: String,
	pub generated_skill_root: Option<std::path::PathBuf>,
}

/// Runs a single deterministic in-process request with default CLI session options.
///
/// This is the thinnest command-facing entrypoint and is used by tests and the default `once`
/// command path.
pub async fn run_once(goal: &str) -> Result<ResponseEnvelope, RuntimeError> {
	run_with_mode_and_options(
		ExecutionRequestOptions {
			session_id: "session-1".to_string(),
			goal: goal.to_string(),
			generated_skill_root: None,
		},
		RunMode::Normal,
	)
	.await
}

/// Runs a single deterministic request while allowing the caller to choose the runtime mode.
///
/// The mode only affects service execution semantics after bootstrap; request normalization and
/// env-derived overrides remain the same as `run_once`.
pub async fn run_with_mode(goal: &str, mode: RunMode) -> Result<ResponseEnvelope, RuntimeError> {
	run_with_mode_and_options(
		ExecutionRequestOptions {
			session_id: "session-1".to_string(),
			goal: goal.to_string(),
			generated_skill_root: None,
		},
		mode,
	)
	.await
}

/// Applies CLI-specific overrides, builds the deterministic service, and executes one request.
pub(crate) async fn run_with_mode_and_options(
	options: ExecutionRequestOptions,
	mode: RunMode,
) -> Result<ResponseEnvelope, RuntimeError> {
	let _env_override_guard = apply_request_env_overrides(&options);
	let gateway = Gateway;
	// build_deterministic_runtime_service_from_env creates a reqwest::blocking::Client (via
	// SkillRegistry::file_backed) which internally constructs and drops a tokio current-thread
	// runtime. That drop panics when it occurs inside an async context. block_in_place provides
	// a blocking-allowed scope so both the construction and the internal runtime drop complete
	// without hitting that check.
	let service = tokio::task::block_in_place(|| {
		build_deterministic_runtime_service_from_env()
			.map_err(|error| RuntimeError::new(error.to_string()))
	})?;
	let request = build_request(&gateway, options, next_cli_request_sequence());
	execute_with_service_and_mode(service, request, mode, None).await
}

/// Runs one live request using env-backed plugin/runtime bootstrap.
///
/// Unlike `run_once`, this path will attempt to boot the live OpenRouter-backed runtime and only
/// falls back according to runtime bootstrap policy.
pub async fn run_live_once_from_env(goal: &str) -> Result<ResponseEnvelope, CommandError> {
	run_live_once_with_options_from_env(ExecutionRequestOptions {
		session_id: "session-1".to_string(),
		goal: goal.to_string(),
		generated_skill_root: None,
	})
	.await
}

pub(crate) async fn run_live_once_with_options_from_env(
	options: ExecutionRequestOptions,
) -> Result<ResponseEnvelope, CommandError> {
	run_live_once_with_options_from_env_and_sender(options, None).await
}

pub(crate) async fn run_live_once_with_options_from_env_and_sender(
	options: ExecutionRequestOptions,
	event_sender: Option<&roku_agent_runtime::LoopEventSender>,
) -> Result<ResponseEnvelope, CommandError> {
	let _env_override_guard = apply_request_env_overrides(&options);
	let gateway = Gateway;
	// build_live_runtime_service_from_env creates a reqwest::blocking::Client (via
	// SkillRegistry::file_backed) which internally constructs and drops a tokio current-thread
	// runtime. Use block_in_place so both the construction and the internal runtime drop complete
	// in a blocking-allowed scope rather than inside the async executor.
	let service = tokio::task::block_in_place(build_live_runtime_service_from_env)?;
	let catalog = Arc::new(service.resource_catalog().clone());
	let service = service.with_approval_gate(cli_approval_gate(catalog));
	let request = build_request(&gateway, options, next_cli_request_sequence());
	execute_with_service_and_mode(service, request, RunMode::Normal, event_sender)
		.await
		.map_err(CommandError::Runtime)
}

/// Builds the live runtime service using the process-local layout, plugin inventory, and configs.
///
/// This is the shared bootstrap entrypoint for CLI live-once, Telegram, and the HTTP gateway so
/// those surfaces observe the same plugin policy and runtime-mode fallback behavior.
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

pub(crate) fn show_artifacts_from_env(_task_id: &str) -> Result<String, CommandError> {
	Ok("[]".to_string())
}

pub(crate) fn show_artifact_content_from_env(
	task_id: &str,
	artifact_id: &str,
) -> Result<String, CommandError> {
	Err(CommandError::Usage(format!(
		"artifact content not found for task={} artifact={artifact_id}",
		task_id
	)))
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
	Err(CommandError::Usage(format!(
		"experiment not found for task: {task_id}"
	)))
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

pub(crate) fn prepare_memory_artifacts_from_env() -> Result<String, CommandError> {
	let layout = LocalStorageLayout::from_env();
	layout.ensure_dirs().map_err(CommandError::Io)?;
	let configs = load_runtime_configs(&layout)?;
	let generated = prepare_runtime_generated_artifacts(&configs)?;
	let lifecycle = default_memory_lifecycle_report(&configs.memory);
	serde_json::to_string_pretty(&json!({
		"enabled": configs.memory.enabled,
		"backend": configs.memory.backend.as_str(),
		"lifecycle": {
			"recall": {
				"requested": lifecycle.recall.requested,
				"effective": lifecycle.recall.effective,
				"blocked_reason": lifecycle.recall.blocked_reason,
			},
			"write_back": {
				"requested": lifecycle.write_back.requested,
				"effective": lifecycle.write_back.effective,
				"blocked_reason": lifecycle.write_back.blocked_reason,
			},
		},
		"backends": {
			"openviking": configs.memory.backends.openviking.summary_json(),
			"sqlite": configs.memory.backends.sqlite.summary_json(),
		},
		"generated_openviking_config_path": generated
			.as_ref()
			.map(|path| path.display().to_string()),
	}))
	.map_err(|error| CommandError::OutputEncoding(error.to_string()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MemoryLifecycleToggleReport {
	requested: bool,
	effective: bool,
	blocked_reason: Option<&'static str>,
}

impl MemoryLifecycleToggleReport {
	const fn disabled() -> Self {
		Self {
			requested: false,
			effective: false,
			blocked_reason: None,
		}
	}

	const fn enabled() -> Self {
		Self {
			requested: true,
			effective: true,
			blocked_reason: None,
		}
	}

	const fn blocked(reason: &'static str) -> Self {
		Self {
			requested: true,
			effective: false,
			blocked_reason: Some(reason),
		}
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MemoryLifecycleReport {
	recall: MemoryLifecycleToggleReport,
	write_back: MemoryLifecycleToggleReport,
}

fn default_memory_lifecycle_report(memory_config: &MemoryRuntimeConfig) -> MemoryLifecycleReport {
	let recall = if memory_config.enabled && memory_config.recall.enabled {
		MemoryLifecycleToggleReport::enabled()
	} else {
		MemoryLifecycleToggleReport::disabled()
	};
	let write_back = if !memory_config.enabled || !memory_config.write.enabled {
		MemoryLifecycleToggleReport::disabled()
	} else if !memory_config.recall.enabled {
		MemoryLifecycleToggleReport::blocked(
			"runtime.memory.recall.enabled is false, so the default lifecycle policy disables write-back",
		)
	} else {
		MemoryLifecycleToggleReport::enabled()
	};

	MemoryLifecycleReport { recall, write_back }
}

pub(crate) fn show_memory_health_from_env() -> Result<String, CommandError> {
	let (_, backend) = build_enabled_memory_backend_from_env()?;
	let health = backend.health().map_err(map_memory_error)?;
	encode_memory_health(health)
}

pub(crate) fn search_memory_from_env(query: MemoryQuery) -> Result<String, CommandError> {
	let (_, backend) = build_enabled_memory_backend_from_env()?;
	let hits = backend.search(&query).map_err(map_memory_error)?;
	serde_json::to_string_pretty(&hits)
		.map_err(|error| CommandError::OutputEncoding(error.to_string()))
}

pub(crate) fn write_memory_from_env(request: MemoryWriteRequest) -> Result<String, CommandError> {
	let (_, backend) = build_enabled_memory_backend_from_env()?;
	let ack = backend.write(&request).map_err(map_memory_error)?;
	serde_json::to_string_pretty(&json!({
		"accepted": ack.accepted,
		"record_id": ack.record_id,
	}))
	.map_err(|error| CommandError::OutputEncoding(error.to_string()))
}

pub(crate) fn delete_memory_from_env(record_id: &str) -> Result<String, CommandError> {
	let (_, backend) = build_enabled_memory_backend_from_env()?;
	backend
		.delete(&MemoryDeleteSelector {
			record_id: record_id.to_string(),
		})
		.map_err(map_memory_error)?;
	serde_json::to_string_pretty(&json!({
		"deleted": true,
		"record_id": record_id,
	}))
	.map_err(|error| CommandError::OutputEncoding(error.to_string()))
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

fn encode_memory_health(health: MemoryBackendHealth) -> Result<String, CommandError> {
	serde_json::to_string_pretty(&json!({
		"backend": health.backend,
		"status": match health.status {
			roku_memory::MemoryBackendStatus::Healthy => "healthy",
			roku_memory::MemoryBackendStatus::Degraded => "degraded",
			roku_memory::MemoryBackendStatus::Unavailable => "unavailable",
		},
		"detail": health.detail,
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
	let bundle = resolve_entry_runtime_bundle(&bootstrap.runtime_configs.memory, &layout)?;

	wire_memory_subsystem(
		RuntimeService::new_with_bundles_and_runtime_and_metrics(
			bundle.control_plane,
			Arc::new(InMemoryAuditSink::default()),
			runtime,
			Arc::new(Metrics::default()),
		),
		bundle.memory,
		&bootstrap.runtime_configs.memory,
	)
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
	let _ = prepare_runtime_generated_artifacts(&runtime_configs)?;
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
		log_local_backend("skill-registry", &layout.skill_root);
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
	let (layout, bootstrap) = build_plugin_bootstrap_from_env()?;
	let runtime =
		GenericAgentRuntime::with_skill_registry_tool_config_and_plugin_snapshot_and_runtime_config(
			bootstrap.skill_registry,
			bootstrap.tool_config,
			bootstrap.plugin_snapshot,
			bootstrap.runtime_configs.tools,
			bootstrap.runtime_configs.agent,
		);
	log_runtime_bootstrap_mode(&RuntimeModeReport::deterministic());
	let bundle = resolve_entry_runtime_bundle(&bootstrap.runtime_configs.memory, &layout)?;
	wire_memory_subsystem(
		RuntimeService::new_with_bundles_and_runtime_and_metrics(
			bundle.control_plane,
			Arc::new(InMemoryAuditSink::default()),
			runtime,
			Arc::new(Metrics::default()),
		)
		.with_runtime_mode_report(RuntimeModeReport::deterministic()),
		bundle.memory,
		&bootstrap.runtime_configs.memory,
	)
}

pub(crate) fn build_live_runtime_service_from_layout_and_bootstrap(
	layout: &LocalStorageLayout,
	bootstrap: PluginBootstrap,
) -> Result<RuntimeService, CommandError> {
	let metrics = Arc::new(Metrics::default());
	let (runtime, runtime_mode) = build_live_runtime(layout, bootstrap.clone(), metrics.clone())?;
	let bundle = resolve_entry_runtime_bundle(&bootstrap.runtime_configs.memory, layout)?;

	wire_memory_subsystem(
		RuntimeService::new_with_bundles_and_runtime_and_metrics(
			bundle.control_plane,
			Arc::new(InMemoryAuditSink::default()),
			runtime,
			metrics,
		)
		.with_runtime_mode_report(runtime_mode),
		bundle.memory,
		&bootstrap.runtime_configs.memory,
	)
}

fn wire_memory_subsystem(
	service: RuntimeService,
	subsystem: roku_memory::ResolvedMemorySubsystem,
	memory_config: &MemoryRuntimeConfig,
) -> Result<RuntimeService, CommandError> {
	let roku_memory::ResolvedMemorySubsystem {
		long_term,
		pending_loop,
		..
	} = subsystem;
	let lifecycle = default_memory_lifecycle_report(memory_config);
	let policy: Arc<dyn MemoryLifecyclePolicy> = if lifecycle.recall.effective {
		Arc::new(ConservativeMemoryLifecyclePolicy {
			recall_limit: memory_config.recall.top_k.max(1),
			automatic_write_back: lifecycle.write_back.effective,
		})
	} else {
		Arc::new(DisabledMemoryLifecyclePolicy)
	};

	let pending_loop_backend = wire_sqlite_pending_loop(memory_config, pending_loop);

	Ok(service
		.with_pending_loop_snapshot_store(Arc::new(MemoryPendingLoopSnapshotStore::new(
			pending_loop_backend,
		)))
		.with_long_term_memory_backend(long_term)
		.with_memory_lifecycle_policy(policy))
}

/// Always use SQLite for pending-loop persistence regardless of the configured memory backend.
///
/// Wraps the SQLite adapter with a read-through to the prior subsystem backend so that
/// pending-loop snapshots stored by a previous backend (e.g. OpenViking) are migrated on
/// first access rather than silently lost.
fn wire_sqlite_pending_loop(
	memory_config: &MemoryRuntimeConfig,
	prior_backend: Box<dyn roku_memory::PendingLoopSnapshotBackend>,
) -> Box<dyn roku_memory::PendingLoopSnapshotBackend> {
	let store_config = roku_plugin_memory_sqlite::SqliteMemoryStoreConfig::new(
		memory_config.backends.sqlite.path.clone(),
	);
	match roku_plugin_memory_sqlite::SqlitePendingLoopSnapshotAdapter::connect(store_config) {
		Ok(adapter) => Box::new(MigratingPendingLoopBackend {
			primary: Box::new(adapter),
			prior: prior_backend,
		}),
		Err(error) => {
			let _ = emit_global_log(
				LogRecord::new(
					"roku-cmd",
					LogLevel::Warn,
					format!(
						"failed to open dedicated SQLite pending-loop store, \
						 falling back to subsystem backend: {error}"
					),
				)
				.with_field(
					"sqlite_path",
					memory_config.backends.sqlite.path.display().to_string(),
				),
			);
			prior_backend
		}
	}
}

/// Read-through wrapper that migrates pending-loop snapshots from a prior backend
/// (e.g. OpenViking, or the old SQLite session_preferences path) into the primary
/// SQLite dedicated-table backend on first access.
struct MigratingPendingLoopBackend {
	primary: Box<dyn roku_memory::PendingLoopSnapshotBackend>,
	prior: Box<dyn roku_memory::PendingLoopSnapshotBackend>,
}

impl roku_memory::PendingLoopSnapshotBackend for MigratingPendingLoopBackend {
	fn load_pending_loop_snapshot(
		&self,
		session_id: &str,
	) -> Result<Option<roku_memory::PendingLoopSnapshot>, roku_memory::PendingLoopSnapshotError> {
		// Try primary (SQLite dedicated table) first.
		if let Some(snapshot) = self.primary.load_pending_loop_snapshot(session_id)? {
			return Ok(Some(snapshot));
		}
		// Read-through to prior backend for pre-migration data.
		let prior_snapshot = self.prior.load_pending_loop_snapshot(session_id)?;
		if let Some(ref snapshot) = prior_snapshot {
			// Promote to primary and clear from prior.
			let _ = self
				.primary
				.save_pending_loop_snapshot(session_id, Some(snapshot.clone()));
			let _ = self.prior.clear_pending_loop_snapshot(session_id);
		}
		Ok(prior_snapshot)
	}

	fn save_pending_loop_snapshot(
		&self,
		session_id: &str,
		snapshot: Option<roku_memory::PendingLoopSnapshot>,
	) -> Result<(), roku_memory::PendingLoopSnapshotError> {
		self.primary
			.save_pending_loop_snapshot(session_id, snapshot)
	}
}

fn build_enabled_memory_backend_from_env()
-> Result<(RuntimeConfigs, Arc<dyn LongTermMemoryBackend>), CommandError> {
	let layout = LocalStorageLayout::from_env();
	layout.ensure_dirs().map_err(CommandError::Io)?;
	let configs = load_runtime_configs(&layout)?;
	let _ = prepare_runtime_generated_artifacts(&configs)?;
	if !configs.memory.enabled {
		return Err(CommandError::Usage(
			"runtime.memory.enabled is false; enable memory before using memory commands"
				.to_string(),
		));
	}
	let backend = resolve_memory_subsystem(&configs.memory)?.long_term;
	Ok((configs, backend))
}

fn map_memory_error(error: MemoryError) -> CommandError {
	CommandError::MemoryBackend(error.to_string())
}

/// Load MCP server configuration from `config/mcp.toml` relative to the layout's config dir.
///
/// Returns an empty config if the file is absent or unparseable (fail-open).
fn load_mcp_config(layout: &LocalStorageLayout) -> McpConfig {
	// Derive config dir from tool_config_path (e.g. "config/tools.toml" -> "config")
	let config_dir = layout
		.tool_config_path
		.parent()
		.map(|p| p.to_path_buf())
		.unwrap_or_else(|| std::path::PathBuf::from("config"));
	let mcp_path = config_dir.join("mcp.toml");

	match std::fs::read_to_string(&mcp_path) {
		Ok(content) => toml::from_str(&content).unwrap_or_else(|e| {
			let _ = emit_global_log(LogRecord::new(
				"roku-cmd",
				LogLevel::Warn,
				format!("invalid MCP config at {}: {}", mcp_path.display(), e),
			));
			McpConfig::default()
		}),
		Err(_) => McpConfig::default(),
	}
}

struct McpBootstrapResult {
	catalog_entries: Vec<roku_plugin_tools::CatalogDescriptor>,
	tools: Vec<Box<dyn roku_plugin_host::Tool>>,
	/// Keepalive for the tokio runtime that hosts rmcp serve loop tasks.
	runtime: Option<Arc<tokio::runtime::Runtime>>,
}

/// Connect to all configured MCP servers (blocking, fail-open).
///
/// The returned `runtime` must be kept alive for the process lifetime — rmcp
/// serve loop tasks live on it. Dropping it kills all MCP connections.
fn connect_mcp_servers_blocking(config: &McpConfig) -> McpBootstrapResult {
	if config.servers.is_empty() {
		return McpBootstrapResult {
			catalog_entries: Vec::new(),
			tools: Vec::new(),
			runtime: None,
		};
	}

	// Create a runtime that will live for the entire process. The rmcp serve
	// loop tasks are spawned on this runtime during connection — dropping it
	// would kill those tasks and break all MCP tool calls.
	let rt = match tokio::runtime::Builder::new_current_thread()
		.enable_all()
		.build()
	{
		Ok(rt) => Arc::new(rt),
		Err(e) => {
			let _ = emit_global_log(LogRecord::new(
				"roku-cmd",
				LogLevel::Warn,
				format!("failed to create MCP bootstrap runtime: {}", e),
			));
			return McpBootstrapResult {
				catalog_entries: Vec::new(),
				tools: Vec::new(),
				runtime: None,
			};
		}
	};

	let rt_for_thread = Arc::clone(&rt);
	let (entries, tools) = std::thread::scope(|s| {
		s.spawn(|| {
			rt_for_thread.block_on(async {
				let mut all_catalog_entries: Vec<roku_plugin_tools::CatalogDescriptor> = Vec::new();
				let mut all_tools: Vec<Box<dyn roku_plugin_host::Tool>> = Vec::new();

				for server_config in &config.servers {
					match McpConnection::connect(server_config).await {
						Ok(connection) => {
							let connection = Arc::new(connection);
							match connection.list_tools().await {
								Ok(tools) => {
									let server_name = connection.server_name();
									let _ = emit_global_log(LogRecord::new(
										"roku-cmd",
										LogLevel::Info,
										format!(
											"MCP server '{}': discovered {} tools",
											server_name,
											tools.len()
										),
									));
									let catalog_entries =
										mcp_tools_to_catalog_descriptors(server_name, &tools);
									all_catalog_entries.extend(catalog_entries);
									for tool in tools {
										all_tools.push(Box::new(McpTool::new(
											Arc::clone(&connection),
											tool,
										)));
									}
								}
								Err(e) => {
									let _ = emit_global_log(LogRecord::new(
										"roku-cmd",
										LogLevel::Warn,
										format!(
											"MCP server '{}': failed to list tools: {}",
											server_config.name, e
										),
									));
								}
							}
						}
						Err(e) => {
							let _ = emit_global_log(LogRecord::new(
								"roku-cmd",
								LogLevel::Warn,
								format!(
									"MCP server '{}': connection failed: {}",
									server_config.name, e
								),
							));
						}
					}
				}

				(all_catalog_entries, all_tools)
			})
		})
		.join()
		.unwrap_or_else(|_| (Vec::new(), Vec::new()))
	});

	McpBootstrapResult {
		runtime: if entries.is_empty() { None } else { Some(rt) },
		catalog_entries: entries,
		tools,
	}
}

fn build_live_runtime(
	layout: &LocalStorageLayout,
	mut bootstrap: PluginBootstrap,
	metrics: Arc<Metrics>,
) -> Result<(GenericAgentRuntime, RuntimeModeReport), CommandError> {
	// The `openrouter` plugin identifier still gates the whole live LLM
	// runtime; the rename to a provider-neutral identifier is out of scope
	// for this task.
	if !bootstrap.plugin_snapshot.is_plugin_enabled("openrouter") {
		let runtime_mode = RuntimeModeReport::live_react_fallback_to_deterministic(
			"live llm runtime disabled by startup policy",
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

	// Only consult auth.json for provider selection when runtime.toml does not
	// explicitly set `provider`. An explicit toml setting always wins.
	let provider_kind = if bootstrap.runtime_configs.llm_provider_explicit {
		bootstrap.runtime_configs.llm_provider
	} else {
		resolve_provider_from_auth_store(bootstrap.runtime_configs.llm_provider)
	};
	log_selected_llm_provider(provider_kind);

	let (route_router, execution_router) = match build_live_llm_routers(
		provider_kind,
		&bootstrap.runtime_configs.openrouter,
		&bootstrap.runtime_configs.anthropic,
		&bootstrap.runtime_configs.openai,
		&metrics,
	) {
		Ok(routers) => routers,
		Err(LiveLlmBootstrapFailure { fallback_reason }) => {
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

	let runtime_mode = RuntimeModeReport::live_react();
	log_runtime_bootstrap_mode(&runtime_mode);

	let mcp_config = load_mcp_config(layout);
	let mcp = connect_mcp_servers_blocking(&mcp_config);

	Ok((
		GenericAgentRuntime::with_routers_and_mcp(
			route_router,
			execution_router,
			bootstrap.skill_registry,
			bootstrap.tool_config,
			bootstrap.plugin_snapshot,
			bootstrap.runtime_configs.tools,
			bootstrap.runtime_configs.agent,
			mcp.catalog_entries,
			mcp.tools,
			mcp.runtime,
		),
		runtime_mode,
	))
}

/// Reason a live LLM provider failed to bootstrap, pre-formatted for the
/// plugin snapshot's `AdmissionRejected` detail.
struct LiveLlmBootstrapFailure {
	fallback_reason: String,
}

/// Build the (route_router, execution_router) pair for the selected LLM
/// provider.
///
/// The selected provider is the only one whose credentials are hard
/// requirements; the rest may be unset without affecting startup. The two
/// routers are separate instances so the route and execution paths have
/// independent circuit-breaker state.
fn build_live_llm_routers(
	kind: LlmProviderKind,
	openrouter: &OpenRouterRuntimeConfig,
	anthropic: &AnthropicRuntimeConfig,
	openai: &OpenAiRuntimeConfig,
	metrics: &Arc<Metrics>,
) -> Result<(LlmRouter, LlmRouter), LiveLlmBootstrapFailure> {
	match kind {
		LlmProviderKind::Openrouter => {
			let api_key =
				resolve_api_key_for_provider("openrouter", || openrouter_api_key_from_env().ok())
					.ok_or_else(|| {
					openrouter_bootstrap_failure(
						roku_plugin_llm::OpenRouterBootstrapError::MissingEnv("OPENROUTER_API_KEY"),
					)
				})?;
			let config = openrouter.clone().with_api_key(api_key);
			let route_router =
				build_openrouter_router_with_metrics(config.clone(), Arc::clone(metrics))
					.map_err(openrouter_bootstrap_failure)?;
			let execution_router =
				build_openrouter_router_with_metrics(config, Arc::clone(metrics))
					.map_err(openrouter_bootstrap_failure)?;
			Ok((route_router, execution_router))
		}
		LlmProviderKind::Anthropic => {
			let api_key =
				resolve_api_key_for_provider("anthropic", || anthropic_api_key_from_env().ok())
					.ok_or_else(|| {
						anthropic_bootstrap_failure(
							roku_plugin_llm::AnthropicBootstrapError::MissingEnv(
								"ROKU_ANTHROPIC_API_KEY",
							),
						)
					})?;
			let config = anthropic.clone().with_api_key(api_key);
			let route_router = build_anthropic_router_with_metrics(
				config.clone(),
				anthropic.clone(),
				Arc::clone(metrics),
			)
			.map_err(anthropic_bootstrap_failure)?;
			let execution_router =
				build_anthropic_router_with_metrics(config, anthropic.clone(), Arc::clone(metrics))
					.map_err(anthropic_bootstrap_failure)?;
			Ok((route_router, execution_router))
		}
		LlmProviderKind::Openai => {
			let api_key = resolve_api_key_for_provider("openai", || openai_api_key_from_env().ok())
				.ok_or_else(|| {
					openai_bootstrap_failure(roku_plugin_llm::OpenAiBootstrapError::MissingEnv(
						"ROKU_OPENAI_API_KEY",
					))
				})?;
			let config = openai.clone().with_api_key(api_key);
			let route_router = build_openai_router_with_metrics(
				config.clone(),
				openai.clone(),
				Arc::clone(metrics),
			)
			.map_err(openai_bootstrap_failure)?;
			let execution_router =
				build_openai_router_with_metrics(config, openai.clone(), Arc::clone(metrics))
					.map_err(openai_bootstrap_failure)?;
			Ok((route_router, execution_router))
		}
	}
}

/// Resolve an API key for a provider using the priority chain:
/// 1. Environment variable (via `env_fn`)
/// 2. auth.json credential store
fn resolve_api_key_for_provider(
	provider: &str,
	env_fn: impl FnOnce() -> Option<String>,
) -> Option<String> {
	// Priority 1: environment variable.
	if let Some(key) = env_fn() {
		return Some(key);
	}
	// Priority 2: auth.json credential.
	let store = crate::auth::AuthStore::from_env();
	let auth_file = store.load().ok().flatten()?;
	let entry = auth_file.credentials.get(provider)?;
	match entry {
		crate::auth::CredentialEntry::ApiKey { api_key } => Some(api_key.clone()),
		crate::auth::CredentialEntry::OAuth { access_token, .. } => Some(access_token.clone()),
	}
}

/// Resolve the active provider from auth.json, falling back to the config default.
pub(crate) fn resolve_provider_from_auth_store(config_default: LlmProviderKind) -> LlmProviderKind {
	let store = crate::auth::AuthStore::from_env();
	let auth_file = match store.load() {
		Ok(Some(f)) => f,
		_ => return config_default,
	};
	match auth_file.active_provider.as_deref() {
		Some("openrouter") => LlmProviderKind::Openrouter,
		Some("anthropic") => LlmProviderKind::Anthropic,
		Some("openai") => LlmProviderKind::Openai,
		_ => config_default,
	}
}

fn openrouter_bootstrap_failure(error: OpenRouterBootstrapError) -> LiveLlmBootstrapFailure {
	LiveLlmBootstrapFailure {
		fallback_reason: format!("openrouter bootstrap failed: {error}"),
	}
}

fn anthropic_bootstrap_failure(error: AnthropicBootstrapError) -> LiveLlmBootstrapFailure {
	LiveLlmBootstrapFailure {
		fallback_reason: format!("anthropic bootstrap failed: {error}"),
	}
}

fn openai_bootstrap_failure(error: OpenAiBootstrapError) -> LiveLlmBootstrapFailure {
	LiveLlmBootstrapFailure {
		fallback_reason: format!("openai bootstrap failed: {error}"),
	}
}

fn log_selected_llm_provider(kind: LlmProviderKind) {
	let _ = emit_global_log(
		LogRecord::new("roku-cmd", LogLevel::Info, "selected live llm provider")
			.with_field("provider", kind.as_str().to_string()),
	);
}

fn log_local_backend(component: &str, path: &Path) {
	let _ = emit_global_log(
		LogRecord::new(
			"roku-cmd",
			LogLevel::Info,
			format!("using local file-backed {component}"),
		)
		.with_field("path", path.display().to_string()),
	);
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

async fn execute_with_service_and_mode(
	service: RuntimeService,
	request: roku_common_types::RequestEnvelope,
	mode: RunMode,
	event_sender: Option<&roku_agent_runtime::LoopEventSender>,
) -> Result<ResponseEnvelope, RuntimeError> {
	let runtime_mode = service.runtime_mode_report();
	let response = service
		.execute_with_mode(request, mode, event_sender)
		.await?;
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

fn build_request(
	gateway: &Gateway,
	options: ExecutionRequestOptions,
	seq: u64,
) -> roku_common_types::RequestEnvelope {
	gateway.normalize(
		RawRequest {
			session_id: options.session_id,
			goal: options.goal,
		},
		seq,
	)
}

/// Build an interactive CLI approval gate that prompts via stderr/stdin.
///
/// Only for interactive mode. Pipe mode must use [`pipe_approval_gate`] instead
/// because stdin is consumed by the message stream.
pub(crate) fn cli_approval_gate(
	catalog: Arc<roku_plugin_tools::ResourceCatalog>,
) -> Arc<dyn roku_agent_runtime::ToolApprovalGate> {
	Arc::new(roku_agent_runtime::RiskBasedGate::new(
		|tool_name: &str, arguments: &serde_json::Value| {
			use std::io::Write as _;
			let args_display = serde_json::to_string_pretty(arguments).unwrap_or_default();
			eprintln!("\n[approval] Tool: {tool_name}");
			if !args_display.is_empty() && args_display != "null" {
				let summary: String = args_display.chars().take(200).collect();
				eprintln!("[approval] Arguments: {summary}");
			}
			eprint!("[approval] Allow? [y/N] ");
			std::io::stderr().flush().ok();
			let mut input = String::new();
			if std::io::stdin().read_line(&mut input).is_ok()
				&& input.trim().eq_ignore_ascii_case("y")
			{
				roku_agent_runtime::ToolApprovalDecision::Approve
			} else {
				roku_agent_runtime::ToolApprovalDecision::Deny(
					"User denied the operation.".to_string(),
				)
			}
		},
		catalog,
	))
}

/// Build an approval gate for pipe mode where stdin is unavailable.
///
/// Safe/read-only tools are auto-approved. Write-risk and unknown tools are
/// denied with an actionable message — the caller cannot be prompted for
/// confirmation in non-interactive mode.
pub(crate) fn pipe_approval_gate(
	catalog: Arc<roku_plugin_tools::ResourceCatalog>,
) -> Arc<dyn roku_agent_runtime::ToolApprovalGate> {
	Arc::new(roku_agent_runtime::RiskBasedGate::new(
		|tool_name: &str, _arguments: &serde_json::Value| {
			roku_agent_runtime::ToolApprovalDecision::Deny(format!(
				"Tool `{tool_name}` requires approval but pipe mode has no interactive stdin. \
				 Use interactive mode (`roku chat`) to approve write operations."
			))
		},
		catalog,
	))
}

/// Load the OAuth client_id for the OpenAI PKCE flow.
///
/// Resolution order:
/// 1. `OPENAI_OAUTH_CLIENT_ID` environment variable
/// 2. `oauth_client_id` field in `config/runtime.toml` under `[runtime.llm]`
pub(crate) fn load_oauth_client_id() -> Option<String> {
	if let Ok(val) = std::env::var("OPENAI_OAUTH_CLIENT_ID")
		&& !val.is_empty()
	{
		return Some(val);
	}
	use crate::storage::LocalStorageLayout;
	let layout = LocalStorageLayout::from_env();
	let configs = crate::runtime_config::load_runtime_configs(&layout).ok()?;
	configs.oauth_client_id
}

pub(crate) fn next_cli_request_sequence() -> u64 {
	SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.map(|duration| duration.as_millis().min(u64::MAX as u128) as u64)
		.unwrap_or(1)
}

pub(crate) struct RequestEnvOverrideGuard {
	_guards: Vec<EnvOverrideGuard>,
}

struct EnvOverrideGuard {
	key: &'static str,
	original: Option<OsString>,
}

impl EnvOverrideGuard {
	fn set_path(key: &'static str, value: &Path) -> Self {
		let original = std::env::var_os(key);
		unsafe {
			std::env::set_var(key, value);
		}
		Self { key, original }
	}
}

impl Drop for EnvOverrideGuard {
	fn drop(&mut self) {
		if let Some(value) = &self.original {
			unsafe {
				std::env::set_var(self.key, value);
			}
		} else {
			unsafe {
				std::env::remove_var(self.key);
			}
		}
	}
}

pub(crate) fn apply_request_env_overrides(
	options: &ExecutionRequestOptions,
) -> RequestEnvOverrideGuard {
	let mut guards = Vec::new();
	if let Some(path) = &options.generated_skill_root {
		guards.push(EnvOverrideGuard::set_path("ROKU_SKILL_ROOT", path));
		guards.push(EnvOverrideGuard::set_path(
			"ROKU_GENERATED_SKILL_ROOT",
			path,
		));
	}
	RequestEnvOverrideGuard { _guards: guards }
}

#[cfg(test)]
mod tests {
	use std::fs;
	use std::io::{Cursor, Write};
	use std::sync::Arc;

	use roku_common_types::{RequestEnvelope, RequestId, ResponseEnvelope, ResponseStatus};
	use roku_memory::{
		InMemoryLongTermMemoryBackend, MemoryRecallConfig, MemoryWriteConfig,
		NoopPendingLoopSnapshotBackend, NoopSessionManagementBackend, NoopSessionStateBackend,
		NoopShortTermContinuityBackend, PendingLoopSnapshot, ResolvedMemorySubsystem,
	};
	use roku_plugin_skills::{
		DownloadedArchive, SkillArchiveFetcher, SkillRegistryError, SkillSource,
	};
	use serde_json::Value;

	use super::*;
	use crate::test_support::{ENV_MUTEX, pending_inventory_resume_success_loop_state};

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

	#[test]
	fn request_env_overrides_restore_skill_roots_after_drop() {
		let _env_lock = ENV_MUTEX.lock().expect("env mutex should lock");
		let original_skill_root = std::env::var_os("ROKU_SKILL_ROOT");
		let original_generated_root = std::env::var_os("ROKU_GENERATED_SKILL_ROOT");
		let tempdir = tempfile::tempdir().expect("temp skill root should exist");
		let generated_root = tempdir.path().join("skills");

		{
			let _guard = apply_request_env_overrides(&ExecutionRequestOptions {
				session_id: "session-1".to_string(),
				goal: "test".to_string(),
				generated_skill_root: Some(generated_root.clone()),
			});
			assert_eq!(
				std::env::var_os("ROKU_SKILL_ROOT"),
				Some(generated_root.clone().into())
			);
			assert_eq!(
				std::env::var_os("ROKU_GENERATED_SKILL_ROOT"),
				Some(generated_root.clone().into())
			);
		}

		assert_eq!(std::env::var_os("ROKU_SKILL_ROOT"), original_skill_root);
		assert_eq!(
			std::env::var_os("ROKU_GENERATED_SKILL_ROOT"),
			original_generated_root
		);
	}

	#[test]
	fn prepare_memory_artifacts_reports_write_back_as_effectively_enabled() {
		let _env_lock = ENV_MUTEX.lock().expect("env mutex should lock");
		let tempdir = tempfile::tempdir().expect("temp root should exist");
		let config_dir = tempdir.path().join("config");
		fs::create_dir_all(&config_dir).expect("config dir should exist");
		let runtime_toml = config_dir.join("runtime.toml");
		fs::write(
			&runtime_toml,
			r#"
[runtime.memory]
enabled = true

[runtime.memory.recall]
enabled = true

[runtime.memory.write]
enabled = true
"#,
		)
		.expect("runtime config should be written");

		let _home_guard = EnvOverrideGuard::set_path("ROKU_HOME", tempdir.path());
		let _config_guard = EnvOverrideGuard::set_path("ROKU_RUNTIME_CONFIG_PATH", &runtime_toml);

		let output = prepare_memory_artifacts_from_env()
			.expect("prepare-config output should render effective lifecycle state");
		let output_json: Value =
			serde_json::from_str(&output).expect("prepare-config output should be valid json");

		assert_eq!(output_json["lifecycle"]["recall"]["requested"], true);
		assert_eq!(output_json["lifecycle"]["recall"]["effective"], true);
		assert_eq!(
			output_json["lifecycle"]["recall"]["blocked_reason"],
			Value::Null
		);
		assert_eq!(output_json["lifecycle"]["write_back"]["requested"], true);
		assert_eq!(output_json["lifecycle"]["write_back"]["effective"], true);
		assert_eq!(
			output_json["lifecycle"]["write_back"]["blocked_reason"],
			Value::Null
		);
	}

	#[tokio::test(flavor = "multi_thread")]
	async fn config_enabled_recall_and_write_enable_effective_write_back_behavior() {
		let backend = Arc::new(InMemoryLongTermMemoryBackend::default());
		let service = service_with_memory_config(
			MemoryRuntimeConfig {
				core: roku_memory::MemoryRuntimeConfig {
					enabled: true,
					recall: MemoryRecallConfig {
						enabled: true,
						top_k: 5,
					},
					write: MemoryWriteConfig {
						enabled: true,
						max_batch_size: 4,
					},
					..roku_memory::MemoryRuntimeConfig::default()
				},
				..MemoryRuntimeConfig::default()
			},
			backend.clone(),
		);

		let response = service
			.execute(memory_write_request(
				"What skills and tools do you have right now?",
			))
			.await
			.expect("configured runtime request should succeed");

		assert_eq!(response.status, ResponseStatus::Succeeded);
		assert_eq!(backend.recorded_writes().len(), 1);
	}

	#[tokio::test(flavor = "multi_thread")]
	async fn config_disabled_recall_keeps_write_back_effectively_off() {
		let backend = Arc::new(InMemoryLongTermMemoryBackend::default());
		let service = service_with_memory_config(
			MemoryRuntimeConfig {
				core: roku_memory::MemoryRuntimeConfig {
					enabled: true,
					recall: MemoryRecallConfig {
						enabled: false,
						top_k: 5,
					},
					write: MemoryWriteConfig {
						enabled: true,
						max_batch_size: 4,
					},
					..roku_memory::MemoryRuntimeConfig::default()
				},
				..MemoryRuntimeConfig::default()
			},
			backend.clone(),
		);

		let response = service
			.execute(memory_write_request(
				"What skills and tools do you have right now?",
			))
			.await
			.expect("configured runtime request should succeed");

		assert_eq!(response.status, ResponseStatus::Succeeded);
		assert!(backend.recorded_writes().is_empty());
	}

	#[tokio::test(flavor = "multi_thread")]
	#[allow(clippy::await_holding_lock)]
	async fn once_flow_resumes_pending_loop_snapshots_from_shared_memory_substrate() {
		let _env_lock = ENV_MUTEX.lock().expect("env mutex should lock");
		let tempdir = tempfile::tempdir().expect("temp root should exist");
		let config_dir = tempdir.path().join("config");
		fs::create_dir_all(&config_dir).expect("config dir should exist");
		let runtime_toml = config_dir.join("runtime.toml");
		let sqlite_path = tempdir.path().join("state").join("pending-loop.db");
		fs::write(
			&runtime_toml,
			format!(
				r#"
[runtime.memory]
enabled = true
backend = "sqlite"

[runtime.memory.recall]
enabled = true

[runtime.memory.write]
enabled = false

[runtime.memory.backends.sqlite]
path = "{}"
"#,
				sqlite_path.display()
			),
		)
		.expect("runtime config should be written");

		let _home_guard = EnvOverrideGuard::set_path("ROKU_HOME", tempdir.path());
		let _config_guard = EnvOverrideGuard::set_path("ROKU_RUNTIME_CONFIG_PATH", &runtime_toml);
		let layout = LocalStorageLayout::from_env();
		layout.ensure_dirs().expect("layout dirs should exist");
		let configs = load_runtime_configs(&layout).expect("runtime configs should load");
		let (pending_loop, selected_topic) = pending_inventory_resume_success_loop_state();
		let session_id = pending_loop.session_id.clone();
		let subsystem =
			resolve_memory_subsystem(&configs.memory).expect("memory subsystem should resolve");
		subsystem
			.pending_loop
			.save_pending_loop_snapshot(
				&session_id,
				Some(PendingLoopSnapshot {
					run_id: pending_loop.run_id.clone(),
					loop_state_json: serde_json::to_string(&pending_loop)
						.expect("pending loop should encode"),
				}),
			)
			.expect("pending loop snapshot should persist");
		assert!(
			subsystem
				.pending_loop
				.load_pending_loop_snapshot(&session_id)
				.expect("pending loop snapshot should reload")
				.is_some()
		);
		drop(subsystem);

		let response = run_with_mode_and_options(
			ExecutionRequestOptions {
				session_id: session_id.clone(),
				goal: selected_topic,
				generated_skill_root: None,
			},
			RunMode::Normal,
		)
		.await
		.expect("once flow should resume the shared pending loop snapshot");

		assert_eq!(response.status, ResponseStatus::Succeeded);
		assert!(
			response
				.message
				.contains("[runtime requested=deterministic effective=deterministic]")
		);

		let reloaded =
			resolve_memory_subsystem(&configs.memory).expect("memory subsystem should reload");
		assert!(
			reloaded
				.pending_loop
				.load_pending_loop_snapshot(&session_id)
				.expect("pending loop snapshot should be readable")
				.is_none(),
			"once flow should consume the persisted pending loop snapshot"
		);
	}

	fn service_with_memory_config(
		memory_config: MemoryRuntimeConfig,
		backend: Arc<InMemoryLongTermMemoryBackend>,
	) -> RuntimeService {
		let subsystem = ResolvedMemorySubsystem::with_parts(
			backend,
			Box::new(NoopShortTermContinuityBackend),
			Box::new(NoopSessionStateBackend),
			Box::new(NoopPendingLoopSnapshotBackend),
			Box::new(NoopSessionManagementBackend),
		);
		wire_memory_subsystem(RuntimeService::default(), subsystem, &memory_config)
			.expect("memory config should wire into runtime service")
	}

	fn memory_write_request(goal: &str) -> RequestEnvelope {
		RequestEnvelope {
			request_id: RequestId("req-memory-write".to_string()),
			session_id: "session-1".to_string(),
			goal: goal.to_string(),
			planning_mode_hint: None,
			conversation_history: Vec::new(),
		}
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
