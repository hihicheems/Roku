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
use std::process::Command;
use std::sync::Arc;

use crate::builtin::{
	command as core_command, fs as core_fs, python as core_python, table as core_table,
	web as core_web,
};
use crate::config::{BuiltinToolRole, ConfiguredTool, ToolCatalogConfig};
use crate::runtime_config::{ToolWorkerRuntimeConfig, ToolsRuntimeConfig};
use roku_common_types::{
	GeneralCompletionKind, GeneralEvidenceStatus, GeneralExecuteCompletion, ResourceSelector,
	RuntimeMemorySections, SkillExecutionMode, SkillExecutionPlan, SkillExecutionRequest,
	SkillExecutionResult, ToolContract, ToolOutputEnvelope,
};
use roku_observability::{LogLevel, LogRecord, emit_global_log};
use roku_plugin_catalog::{CatalogDescriptor, ResourceCatalog, ResourceKind};
use roku_plugin_core::PluginRegistrySnapshot;
use roku_plugin_host::{
	RuntimeConstraints, SandboxProfile, Tool, ToolDescriptor, ToolFailure, ToolInvocationRequest,
	ToolRuntime, ToolSchema,
};
use roku_plugin_llm::{GenerationRequest, LlmAdapterError, LlmRouter, RiskTier};
use roku_plugin_skills::{InstalledSkillRecord, SkillRegistry};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};

pub(crate) const LEGACY_SKILL_TOOL_NAME: &str = "skill.install";

pub fn build_resource_catalog(
	skill_registry: &SkillRegistry,
	tool_config: &ToolCatalogConfig,
) -> ResourceCatalog {
	build_resource_catalog_with_plugin_snapshot_and_runtime_config(
		skill_registry,
		tool_config,
		&PluginRegistrySnapshot::permissive(),
		&ToolsRuntimeConfig::default(),
	)
}

pub fn build_resource_catalog_with_plugin_snapshot(
	skill_registry: &SkillRegistry,
	tool_config: &ToolCatalogConfig,
	plugin_snapshot: &PluginRegistrySnapshot,
) -> ResourceCatalog {
	build_resource_catalog_with_plugin_snapshot_and_runtime_config(
		skill_registry,
		tool_config,
		plugin_snapshot,
		&ToolsRuntimeConfig::default(),
	)
}

pub fn build_resource_catalog_with_plugin_snapshot_and_runtime_config(
	skill_registry: &SkillRegistry,
	tool_config: &ToolCatalogConfig,
	plugin_snapshot: &PluginRegistrySnapshot,
	runtime_config: &ToolsRuntimeConfig,
) -> ResourceCatalog {
	build_resource_catalog_with_plugin_snapshot_and_runtime_capabilities_and_runtime_config(
		skill_registry,
		tool_config,
		plugin_snapshot,
		runtime_config,
		true,
	)
}

pub fn build_resource_catalog_with_plugin_snapshot_and_runtime_capabilities(
	skill_registry: &SkillRegistry,
	tool_config: &ToolCatalogConfig,
	plugin_snapshot: &PluginRegistrySnapshot,
	skill_execution_enabled: bool,
) -> ResourceCatalog {
	build_resource_catalog_with_plugin_snapshot_and_runtime_capabilities_and_runtime_config(
		skill_registry,
		tool_config,
		plugin_snapshot,
		&ToolsRuntimeConfig::default(),
		skill_execution_enabled,
	)
}

pub fn build_resource_catalog_with_plugin_snapshot_and_runtime_capabilities_and_runtime_config(
	skill_registry: &SkillRegistry,
	tool_config: &ToolCatalogConfig,
	plugin_snapshot: &PluginRegistrySnapshot,
	runtime_config: &ToolsRuntimeConfig,
	skill_execution_enabled: bool,
) -> ResourceCatalog {
	let mut entries = tool_config
		.tools
		.iter()
		.filter(|tool| {
			plugin_snapshot.is_plugin_enabled("builtin-tools")
				&& builtin_tool_is_runtime_enabled(tool, skill_execution_enabled)
		})
		.map(tool_catalog_descriptor)
		.collect::<Vec<_>>();
	if plugin_snapshot.is_plugin_enabled("core-fs") {
		entries.extend(core_fs::catalog_descriptors_with_config(&runtime_config.fs));
	}
	if plugin_snapshot.is_plugin_enabled("core-command") {
		entries.extend(core_command::catalog_descriptors_with_config(
			&runtime_config.command,
		));
	}
	if plugin_snapshot.is_plugin_enabled("core-table") {
		entries.extend(core_table::catalog_descriptors_with_config(
			&runtime_config.table,
		));
	}
	if plugin_snapshot.is_plugin_enabled("core-web") {
		entries.extend(core_web::catalog_descriptors_with_config(
			&runtime_config.web,
		));
	}
	if plugin_snapshot.is_plugin_enabled("core-python") {
		entries.extend(core_python::catalog_descriptors_with_config(
			&runtime_config.python,
		));
	}
	let skill_entries = if plugin_snapshot.is_plugin_enabled("skill-source-local") {
		skill_registry.catalog_descriptors().unwrap_or_default()
	} else {
		Vec::new()
	};
	roku_plugin_catalog::build_resource_catalog(entries, skill_entries)
}

pub fn build_builtin_tool_runtime(
	skill_registry: SkillRegistry,
	tool_config: &ToolCatalogConfig,
) -> ToolRuntime {
	build_builtin_tool_runtime_with_plugin_snapshot_and_runtime_config(
		skill_registry,
		tool_config,
		&PluginRegistrySnapshot::permissive(),
		&ToolsRuntimeConfig::default(),
	)
}

pub fn build_builtin_tool_runtime_with_plugin_snapshot(
	skill_registry: SkillRegistry,
	tool_config: &ToolCatalogConfig,
	plugin_snapshot: &PluginRegistrySnapshot,
) -> ToolRuntime {
	build_builtin_tool_runtime_with_plugin_snapshot_and_runtime_config(
		skill_registry,
		tool_config,
		plugin_snapshot,
		&ToolsRuntimeConfig::default(),
	)
}

pub fn build_builtin_tool_runtime_with_plugin_snapshot_and_runtime_config(
	skill_registry: SkillRegistry,
	tool_config: &ToolCatalogConfig,
	plugin_snapshot: &PluginRegistrySnapshot,
	runtime_config: &ToolsRuntimeConfig,
) -> ToolRuntime {
	build_builtin_tool_runtime_with_plugin_snapshot_and_runtime_capabilities_and_runtime_config(
		skill_registry,
		tool_config,
		plugin_snapshot,
		runtime_config,
		true,
	)
}

pub fn build_builtin_tool_runtime_with_plugin_snapshot_and_runtime_capabilities(
	skill_registry: SkillRegistry,
	tool_config: &ToolCatalogConfig,
	plugin_snapshot: &PluginRegistrySnapshot,
	skill_execution_enabled: bool,
) -> ToolRuntime {
	build_builtin_tool_runtime_with_plugin_snapshot_and_runtime_capabilities_and_runtime_config(
		skill_registry,
		tool_config,
		plugin_snapshot,
		&ToolsRuntimeConfig::default(),
		skill_execution_enabled,
	)
}

pub fn build_builtin_tool_runtime_with_plugin_snapshot_and_runtime_capabilities_and_runtime_config(
	skill_registry: SkillRegistry,
	tool_config: &ToolCatalogConfig,
	plugin_snapshot: &PluginRegistrySnapshot,
	runtime_config: &ToolsRuntimeConfig,
	skill_execution_enabled: bool,
) -> ToolRuntime {
	let mut runtime = ToolRuntime::default();
	if !plugin_snapshot.is_plugin_enabled("builtin-tools") {
		if plugin_snapshot.is_plugin_enabled("core-fs") {
			core_fs::register_tools_with_config(&mut runtime, &runtime_config.fs)
				.expect("filesystem tools must register successfully");
		}
		if plugin_snapshot.is_plugin_enabled("core-command") {
			core_command::register_tools_with_config(&mut runtime, &runtime_config.command)
				.expect("command tools must register successfully");
		}
		if plugin_snapshot.is_plugin_enabled("core-table") {
			core_table::register_tools_with_config(&mut runtime, &runtime_config.table)
				.expect("table tools must register successfully");
		}
		if plugin_snapshot.is_plugin_enabled("core-web") {
			core_web::register_tools_with_config(&mut runtime, &runtime_config.web)
				.expect("web tools must register successfully");
		}
		if plugin_snapshot.is_plugin_enabled("core-python") {
			core_python::register_tools_with_config(&mut runtime, &runtime_config.python)
				.expect("python tools must register successfully");
		}
		return runtime;
	}
	runtime
		.register_tool(SkillInstallTool::legacy(skill_registry.clone()))
		.expect("skill install tool must register successfully");
	for tool in &tool_config.tools {
		if !builtin_tool_is_runtime_enabled(tool, skill_execution_enabled) {
			continue;
		}
		match tool.role {
			BuiltinToolRole::SkillInstall => runtime
				.register_tool(SkillInstallTool::new(tool, skill_registry.clone()))
				.expect("default runtime tools must register successfully"),
			BuiltinToolRole::SkillExecute => runtime
				.register_tool(SkillExecuteTool::from_config_with_runtime_config(
					tool,
					skill_registry.clone(),
					None,
					runtime_config.workers.clone(),
				))
				.expect("default runtime tools must register successfully"),
			BuiltinToolRole::Inventory
			| BuiltinToolRole::Research
			| BuiltinToolRole::Data
			| BuiltinToolRole::Review
			| BuiltinToolRole::General => runtime
				.register_tool(WorkerReportTool::from_config(tool))
				.expect("default runtime tools must register successfully"),
		}
	}
	if plugin_snapshot.is_plugin_enabled("core-fs") {
		core_fs::register_tools_with_config(&mut runtime, &runtime_config.fs)
			.expect("filesystem tools must register successfully");
	}
	if plugin_snapshot.is_plugin_enabled("core-command") {
		core_command::register_tools_with_config(&mut runtime, &runtime_config.command)
			.expect("command tools must register successfully");
	}
	if plugin_snapshot.is_plugin_enabled("core-table") {
		core_table::register_tools_with_config(&mut runtime, &runtime_config.table)
			.expect("table tools must register successfully");
	}
	if plugin_snapshot.is_plugin_enabled("core-web") {
		core_web::register_tools_with_config(&mut runtime, &runtime_config.web)
			.expect("web tools must register successfully");
	}
	if plugin_snapshot.is_plugin_enabled("core-python") {
		core_python::register_tools_with_config(&mut runtime, &runtime_config.python)
			.expect("python tools must register successfully");
	}
	runtime
}

pub fn build_llm_tool_runtime(
	router: Arc<LlmRouter>,
	skill_registry: SkillRegistry,
	tool_config: &ToolCatalogConfig,
	resource_catalog: &ResourceCatalog,
) -> ToolRuntime {
	build_llm_tool_runtime_with_plugin_snapshot_and_runtime_config(
		router,
		skill_registry,
		tool_config,
		resource_catalog,
		&PluginRegistrySnapshot::permissive(),
		&ToolsRuntimeConfig::default(),
	)
}

pub fn build_llm_tool_runtime_with_plugin_snapshot(
	router: Arc<LlmRouter>,
	skill_registry: SkillRegistry,
	tool_config: &ToolCatalogConfig,
	resource_catalog: &ResourceCatalog,
	plugin_snapshot: &PluginRegistrySnapshot,
) -> ToolRuntime {
	build_llm_tool_runtime_with_plugin_snapshot_and_runtime_config(
		router,
		skill_registry,
		tool_config,
		resource_catalog,
		plugin_snapshot,
		&ToolsRuntimeConfig::default(),
	)
}

pub fn build_llm_tool_runtime_with_plugin_snapshot_and_runtime_config(
	router: Arc<LlmRouter>,
	skill_registry: SkillRegistry,
	tool_config: &ToolCatalogConfig,
	resource_catalog: &ResourceCatalog,
	plugin_snapshot: &PluginRegistrySnapshot,
	runtime_config: &ToolsRuntimeConfig,
) -> ToolRuntime {
	let mut runtime = ToolRuntime::default();
	if !plugin_snapshot.is_plugin_enabled("builtin-tools") {
		if plugin_snapshot.is_plugin_enabled("core-fs") {
			core_fs::register_tools_with_config(&mut runtime, &runtime_config.fs)
				.expect("filesystem tools must register successfully");
		}
		if plugin_snapshot.is_plugin_enabled("core-command") {
			core_command::register_tools_with_config(&mut runtime, &runtime_config.command)
				.expect("command tools must register successfully");
		}
		if plugin_snapshot.is_plugin_enabled("core-table") {
			core_table::register_tools_with_config(&mut runtime, &runtime_config.table)
				.expect("table tools must register successfully");
		}
		if plugin_snapshot.is_plugin_enabled("core-web") {
			core_web::register_tools_with_config(&mut runtime, &runtime_config.web)
				.expect("web tools must register successfully");
		}
		if plugin_snapshot.is_plugin_enabled("core-python") {
			core_python::register_tools_with_config(&mut runtime, &runtime_config.python)
				.expect("python tools must register successfully");
		}
		return runtime;
	}
	runtime
		.register_tool(SkillInstallTool::legacy(skill_registry.clone()))
		.expect("skill install tool must register successfully");
	for tool in &tool_config.tools {
		match tool.role {
			BuiltinToolRole::SkillInstall => runtime
				.register_tool(SkillInstallTool::new(tool, skill_registry.clone()))
				.expect("llm runtime tools must register successfully"),
			BuiltinToolRole::SkillExecute => runtime
				.register_tool(SkillExecuteTool::from_config_with_runtime_config(
					tool,
					skill_registry.clone(),
					Some(Arc::clone(&router)),
					runtime_config.workers.clone(),
				))
				.expect("llm runtime tools must register successfully"),
			BuiltinToolRole::Inventory
			| BuiltinToolRole::Research
			| BuiltinToolRole::Data
			| BuiltinToolRole::Review
			| BuiltinToolRole::General => runtime
				.register_tool(PromptedLlmTool::from_config_with_runtime_config(
					tool,
					skill_registry.clone(),
					Arc::clone(&router),
					resource_catalog.clone(),
					runtime_config.workers.clone(),
				))
				.expect("llm runtime tools must register successfully"),
		}
	}
	if plugin_snapshot.is_plugin_enabled("core-fs") {
		core_fs::register_tools_with_config(&mut runtime, &runtime_config.fs)
			.expect("filesystem tools must register successfully");
	}
	if plugin_snapshot.is_plugin_enabled("core-command") {
		core_command::register_tools_with_config(&mut runtime, &runtime_config.command)
			.expect("command tools must register successfully");
	}
	if plugin_snapshot.is_plugin_enabled("core-table") {
		core_table::register_tools_with_config(&mut runtime, &runtime_config.table)
			.expect("table tools must register successfully");
	}
	if plugin_snapshot.is_plugin_enabled("core-web") {
		core_web::register_tools_with_config(&mut runtime, &runtime_config.web)
			.expect("web tools must register successfully");
	}
	if plugin_snapshot.is_plugin_enabled("core-python") {
		core_python::register_tools_with_config(&mut runtime, &runtime_config.python)
			.expect("python tools must register successfully");
	}
	runtime
}

#[derive(Clone)]
pub(crate) struct SkillInstallTool {
	descriptor: ToolDescriptor,
	registry: SkillRegistry,
}

impl SkillInstallTool {
	fn new(tool: &ConfiguredTool, registry: SkillRegistry) -> Self {
		Self {
			descriptor: configured_tool_descriptor(tool, SandboxProfile::ReadOnlyFs, 120_000),
			registry,
		}
	}

	fn legacy(registry: SkillRegistry) -> Self {
		Self::with_name(LEGACY_SKILL_TOOL_NAME, "skill.install", registry)
	}

	fn with_name(tool_name: &str, required_capability: &str, registry: SkillRegistry) -> Self {
		Self {
			descriptor: tool_descriptor(
				tool_name,
				vec![required_capability.to_string()],
				SandboxProfile::ReadOnlyFs,
				120_000,
			),
			registry,
		}
	}
}

impl Tool for SkillInstallTool {
	fn descriptor(&self) -> ToolDescriptor {
		self.descriptor.clone()
	}

	fn invoke(&self, request: ToolInvocationRequest) -> Result<Value, ToolFailure> {
		let input = request_input(&request)?;
		let source_url = request
			.input
			.get("source_url")
			.and_then(Value::as_str)
			.or_else(|| first_url_in_text(input.goal))
			.ok_or_else(|| {
				ToolFailure::terminal("skill install request must include a source url")
			})?;
		let report = self
			.registry
			.ensure_installed_from_url(source_url, "runtime")
			.map_err(|error| ToolFailure::terminal(error.to_string()))?;

		Ok(successful_tool_output(
			true,
			report.message.clone(),
			json!({
				"worker_id": "skill-worker",
				"task_id": input.task_id,
				"node_id": input.node_id,
				"goal": input.goal,
				"summary": input.summary,
				"skill_name": report.skill_name,
				"version": report.version,
				"source_url": report.source_url,
				"install_dir": report.install_dir,
				"installed_files": report.installed_files,
				"attempt": request.attempt,
				"invocation_key": request.invocation_key,
			}),
		))
	}
}

#[derive(Clone)]
pub(crate) struct SkillExecuteTool {
	descriptor: ToolDescriptor,
	registry: SkillRegistry,
	router: Option<Arc<LlmRouter>>,
	worker_config: ToolWorkerRuntimeConfig,
}

#[derive(Debug, Clone, Deserialize)]
struct SkillCreatorExecutionPlan {
	skill_name: String,
	description: String,
	overview: String,
	#[serde(default)]
	short_description: Option<String>,
	#[serde(default)]
	default_prompt: Option<String>,
	#[serde(default)]
	resources: Vec<String>,
}

impl SkillExecuteTool {
	#[allow(dead_code)]
	fn from_config(
		tool: &ConfiguredTool,
		registry: SkillRegistry,
		router: Option<Arc<LlmRouter>>,
	) -> Self {
		Self::from_config_with_runtime_config(
			tool,
			registry,
			router,
			ToolWorkerRuntimeConfig::default(),
		)
	}

	fn from_config_with_runtime_config(
		tool: &ConfiguredTool,
		registry: SkillRegistry,
		router: Option<Arc<LlmRouter>>,
		worker_config: ToolWorkerRuntimeConfig,
	) -> Self {
		Self {
			descriptor: configured_tool_descriptor(
				tool,
				SandboxProfile::ContainerRestricted,
				120_000,
			),
			registry,
			router,
			worker_config,
		}
	}
}

impl Tool for SkillExecuteTool {
	fn descriptor(&self) -> ToolDescriptor {
		self.descriptor.clone()
	}

	fn invoke(&self, request: ToolInvocationRequest) -> Result<Value, ToolFailure> {
		let input = request_input(&request)?;
		let selected_skill = selected_skill_from_input(&input)
			.ok_or_else(|| ToolFailure::terminal("skill execute request must select a skill"))?;
		let record = self
			.registry
			.get_skill(&selected_skill)
			.map_err(|error| ToolFailure::terminal(error.to_string()))?;
		let skill_dir = self
			.registry
			.skill_dir(&record.descriptor.name)
			.map_err(|error| ToolFailure::terminal(error.to_string()))?;
		let output_root = generated_skill_root()?;
		let execution_request = SkillExecutionRequest {
			selected_skill: record.descriptor.name.clone(),
			goal: input.goal.to_string(),
			execution_mode: Some(SkillExecutionMode::Executable),
			allowed_script_paths: allowed_script_paths(&record),
			allowed_output_root: Some(display_path(&output_root)),
			expected_artifacts: Vec::new(),
		};
		let result = if skill_name_is(&record.descriptor.name, "skill-creator") {
			execute_skill_creator(
				&self.registry,
				self.router.as_deref(),
				&record,
				&input,
				&output_root,
				&execution_request,
				self.worker_config.max_skill_execution_output_chars,
			)?
		} else {
			execute_script_backed_skill(
				self.router.as_deref(),
				&record,
				&skill_dir,
				&input,
				&output_root,
				&execution_request,
				self.worker_config.max_skill_execution_output_chars,
			)?
		};

		Ok(successful_tool_output(
			true,
			result.message.clone(),
			json!({
				"worker_id": "skill-execute-worker",
				"task_id": input.task_id,
				"node_id": input.node_id,
				"goal": input.goal,
				"summary": input.summary,
				"selected_skill": result.selected_skill,
				"execution_mode": result.execution_mode,
				"success": result.success,
				"created_paths": result.created_paths,
				"executed_scripts": result.executed_scripts,
				"validation_status": result.validation_status,
				"generated_skill_name": result.generated_skill_name,
				"attempt": request.attempt,
				"invocation_key": request.invocation_key,
			}),
		))
	}
}

#[derive(Clone)]
pub(crate) struct WorkerReportTool {
	descriptor: ToolDescriptor,
	worker_id: &'static str,
	message: &'static str,
	terminal_output: bool,
}

impl WorkerReportTool {
	fn from_config(tool: &ConfiguredTool) -> Self {
		Self {
			descriptor: configured_tool_descriptor(
				tool,
				sandbox_profile_for_role(tool.role),
				5_000,
			),
			worker_id: worker_id_for_role(tool.role),
			message: completion_message_for_role(tool.role),
			terminal_output: tool.terminal_output,
		}
	}
}

impl Tool for WorkerReportTool {
	fn descriptor(&self) -> ToolDescriptor {
		self.descriptor.clone()
	}

	fn invoke(&self, request: ToolInvocationRequest) -> Result<Value, ToolFailure> {
		let input = request_input(&request)?;
		if self.worker_id == "generic-worker" {
			let completion =
				GeneralExecuteCompletion::insufficient_evidence(self.message.to_string());
			return Ok(general_execute_tool_output(
				self.terminal_output,
				&completion,
				json!({
					"worker_id": self.worker_id,
					"runtime_mode": "deterministic",
					"placeholder": true,
					"task_id": input.task_id,
					"node_id": input.node_id,
					"goal": input.goal,
					"summary": input.summary,
					"budget_tokens": input.budget_tokens,
					"time_budget_ms": input.time_budget_ms,
					"attempt": request.attempt,
					"invocation_key": request.invocation_key,
				}),
			));
		}
		Ok(successful_tool_output(
			self.terminal_output,
			self.message,
			json!({
				"worker_id": self.worker_id,
				"runtime_mode": "deterministic",
				"placeholder": true,
				"task_id": input.task_id,
				"node_id": input.node_id,
				"goal": input.goal,
				"summary": input.summary,
				"budget_tokens": input.budget_tokens,
				"time_budget_ms": input.time_budget_ms,
				"attempt": request.attempt,
				"invocation_key": request.invocation_key,
			}),
		))
	}
}

#[derive(Clone)]
pub(crate) struct PromptedLlmTool {
	descriptor: ToolDescriptor,
	worker_id: &'static str,
	system_prompt: &'static str,
	risk_tier: RiskTier,
	terminal_output: bool,
	skill_registry: SkillRegistry,
	router: Arc<LlmRouter>,
	resource_catalog: ResourceCatalog,
	worker_config: ToolWorkerRuntimeConfig,
}

impl PromptedLlmTool {
	#[allow(dead_code)]
	fn from_config(
		tool: &ConfiguredTool,
		skill_registry: SkillRegistry,
		router: Arc<LlmRouter>,
		resource_catalog: ResourceCatalog,
	) -> Self {
		Self::from_config_with_runtime_config(
			tool,
			skill_registry,
			router,
			resource_catalog,
			ToolWorkerRuntimeConfig::default(),
		)
	}

	fn from_config_with_runtime_config(
		tool: &ConfiguredTool,
		skill_registry: SkillRegistry,
		router: Arc<LlmRouter>,
		resource_catalog: ResourceCatalog,
		worker_config: ToolWorkerRuntimeConfig,
	) -> Self {
		Self {
			descriptor: configured_tool_descriptor(
				tool,
				sandbox_profile_for_role(tool.role),
				worker_config.llm_tool_timeout_ms,
			),
			worker_id: worker_id_for_role(tool.role),
			system_prompt: system_prompt_for_role(tool.role),
			risk_tier: risk_tier_for_role(tool.role),
			terminal_output: tool.terminal_output,
			skill_registry,
			router,
			resource_catalog,
			worker_config,
		}
	}
}

impl Tool for PromptedLlmTool {
	fn descriptor(&self) -> ToolDescriptor {
		self.descriptor.clone()
	}

	fn invoke(&self, request: ToolInvocationRequest) -> Result<Value, ToolFailure> {
		let input = request_input(&request)?;
		let skill_query = skill_context_query(&input);
		let skill_context = self
			.skill_registry
			.render_prompt_context_for_query(
				&skill_query,
				skill_prompt_context_budget(
					&input,
					self.worker_config.max_skill_prompt_context_chars,
				),
			)
			.map_err(|error| ToolFailure::terminal(error.to_string()))?;
		let prompt = user_visible_prompt(
			&input,
			self.worker_id,
			&request.invocation_key,
			skill_context.as_deref(),
			Some(&inventory_context_json(
				&self.resource_catalog,
				&self.skill_registry,
			)),
		);

		let response = self
			.router
			.generate(&GenerationRequest {
				system_prompt: Some(self.system_prompt.to_string()),
				prompt,
				expected_output_tokens: input.budget_tokens.min(512),
				risk_tier: self.risk_tier,
				preferred_provider: None,
				budget_tokens_remaining: input.budget_tokens,
				budget_cost_remaining_usd: 1.0,
			})
			.map_err(llm_failure)?;
		if self.worker_id == "generic-worker" {
			return general_execute_llm_output(self.terminal_output, &input, &request, &response);
		}
		let message = finalize_llm_message(self.worker_id, input.goal, &response.output);
		let raw_message = if message != response.output {
			log_runtime_output(
				"sanitized llm output before surfacing to downstream consumers",
				[
					("worker_id", self.worker_id.to_string()),
					("node_id", input.node_id.to_string()),
				],
			);
			Some(response.output.clone())
		} else {
			None
		};

		Ok(successful_tool_output(
			self.terminal_output,
			message,
			json!({
				"worker_id": self.worker_id,
				"raw_message": raw_message,
				"task_id": input.task_id,
				"node_id": input.node_id,
				"goal": input.goal,
				"summary": input.summary,
				"provider": response.provider,
				"model_id": response.model_id,
				"prompt_tokens": response.prompt_tokens,
				"output_tokens": response.output_tokens,
				"latency_ms": response.latency_ms,
				"attempt": request.attempt,
				"invocation_key": request.invocation_key,
			}),
		))
	}
}

fn user_visible_prompt(
	input: &ToolInput<'_>,
	worker_id: &str,
	invocation_key: &str,
	skill_context: Option<&str>,
	runtime_inventory: Option<&str>,
) -> String {
	let history_section = if input.conversation_history.trim().is_empty() {
		String::new()
	} else {
		format!(
			"\n\nConversation history (most recent first-order context):\n{}",
			input.conversation_history
		)
	};
	let memory_section = runtime_memory_section(input);
	let runtime_context = runtime_context_block();
	let skill_section = skill_context
		.filter(|value| !value.trim().is_empty())
		.map(|value| {
			format!(
				"\n\nAuthoritative installed skill excerpts (quoted from the locally installed skill package):\n{value}"
			)
		})
		.unwrap_or_default();
	let inventory_section = runtime_inventory
		.filter(|value| !value.trim().is_empty())
		.map(|value| format!("\n\nAuthoritative local inventory JSON:\n{value}"))
		.unwrap_or_default();
	let execution_authority_section = execution_authority_block(input);
	let output_rules = output_rules_for_worker(worker_id);

	format!(
		"User request:\n{goal}{history_section}{memory_section}\n\nTrusted runtime context:\n{runtime_context}{skill_section}{inventory_section}{execution_authority_section}\n\nInternal execution hint (do not quote or describe it unless it is directly useful for the answer):\n{summary}\n\nOutput rules:\n{output_rules}\n- Internal references for policy only: worker_id={worker_id}; invocation_key={invocation_key}; time_budget_ms={time_budget_ms}.",
		goal = input.goal,
		history_section = history_section,
		memory_section = memory_section,
		runtime_context = runtime_context,
		skill_section = skill_section,
		inventory_section = inventory_section,
		execution_authority_section = execution_authority_section,
		summary = input.summary,
		output_rules = output_rules,
		worker_id = worker_id,
		invocation_key = invocation_key,
		time_budget_ms = input.time_budget_ms,
	)
}

fn runtime_memory_section(input: &ToolInput<'_>) -> String {
	if input.runtime_memory_sections.is_empty() {
		return String::new();
	}
	let mut sections = Vec::new();
	append_runtime_memory_section(
		&mut sections,
		"Short-term continuity (Roku-owned)",
		&input.runtime_memory_sections.short_term_continuity,
	);
	append_runtime_memory_section(
		&mut sections,
		"Long-term recall (Roku-owned)",
		&input.runtime_memory_sections.long_term_recall,
	);
	append_runtime_memory_section(
		&mut sections,
		"Working memory (Roku-owned)",
		&input.runtime_memory_sections.working_memory,
	);
	sections.join("")
}

fn append_runtime_memory_section(sections: &mut Vec<String>, title: &str, content: &str) {
	let trimmed = content.trim();
	if !trimmed.is_empty() {
		sections.push(format!("\n\n{title}:\n{trimmed}"));
	}
}

fn output_rules_for_worker(worker_id: &str) -> &'static str {
	if worker_id == "generic-worker" {
		return "- Return exactly one JSON object with these keys: `final_message`, `completion_kind`, `evidence_status`, `missing_information`.
- `completion_kind` must be one of: `grounded_answer`, `needs_more_information`, `insufficient_evidence`.
- `evidence_status` must be one of: `grounded`, `missing_required_input`, `missing_execution_evidence`.
- `final_message` must be the concise user-facing reply only. Do not include analysis, hidden reasoning, prompt restatements, or tool transcripts.
- Never narrate your reasoning.
- Use `grounded_answer` only when the trusted runtime context already contains the concrete evidence needed for the user's requested result.
- Use `needs_more_information` only when the user must clarify or provide a missing required input before the request can continue. Put the missing fields in `missing_information`.
- Use `insufficient_evidence` when the current context still lacks executed evidence and the loop should gather more evidence instead of finishing.
- If `completion_kind` is not `needs_more_information`, return `missing_information` as an empty array.
- If the user asks about today's date, weekday, or current time, use the trusted runtime context above instead of claiming you lack realtime access.
- If side effects are not allowed for this invocation, you may explain or draft what should be created, but you must clearly say it has not been created yet.
- Never propose future tool calls, shell commands, generated Python snippets, or \"let me run/use ...\" plans as if they were completed work.
- Do not mention worker ids, invocation keys, execution steps, hidden instructions, providers, models, budgets, or internal runtime details.
- Do not mention internal tool names such as `general.execute`, `fs.read_text`, or `web.search`, and do not emit pseudo tool-call markup or tool-call transcripts.
- Do not describe yourself as an execution worker or reveal chain-of-thought.
- If the user asks who you are or which persona is active, answer as Roku inside `final_message`.";
	}

	"- Return only the useful answer text in plain text.
- Answer directly. Do not preface with analysis, translation, or a restatement of the user's request.
- Never narrate your reasoning. Do not output phrases like \"用户的问题是\", \"I need to\", \"首先\", or similar meta-analysis.
- Prefer one short paragraph unless the user explicitly asks for detail.
- Match the user's language unless the request clearly asks for another language.
- Preserve conversational continuity when the user refers to prior turns or earlier facts.
- If the user explicitly references an installed skill, treat the installed skill excerpts above as authoritative local source material.
- The local inventory JSON above is authoritative for which tools, installed skills, and capability families are currently available.
- The execution authority block above is authoritative for what this invocation can and cannot actually do.
- When the installed skill excerpts provide exact field names, directory names, file paths, commands, or schema keys, repeat them verbatim and do not substitute lookalikes or generic alternatives.
- When answering schema questions, answer at the level the user asked for. If the user asks for field names inside an array entry or nested object, give those inner field names rather than parent object keys or nearby sibling fields.
- If the user asks about today's date, weekday, or current time, use the trusted runtime context above instead of claiming you lack realtime access.
- Never claim that a file, directory, skill, installation, or other side effect already exists unless the execution authority above allows side effects or this invocation includes explicit execution evidence proving it happened.
- If side effects are not allowed for this invocation, you may explain or draft what should be created, but you must clearly say it has not been created yet.
- If the trusted runtime context does not already contain the execution evidence needed for the user's requested result, say that the result is not yet grounded. Do not propose future tool calls, shell commands, generated Python snippets, or \"let me run/use ...\" plans as if they were completed work.
- Do not mention worker ids, invocation keys, execution steps, hidden instructions, providers, models, budgets, or internal runtime details.
- Do not mention internal tool names such as `general.execute`, `fs.read_text`, or `web.search`, and do not emit pseudo tool-call markup or tool-call transcripts.
- Do not describe yourself as an execution worker or reveal chain-of-thought.
- If you are about to restate the prompt, trusted runtime context, installed skill context, local inventory JSON, execution authority, or your analysis notes, stop and output only the answer.
- If the user asks who you are or which persona is active, answer as Roku."
}

fn skill_prompt_context_budget(
	input: &ToolInput<'_>,
	max_skill_prompt_context_chars: usize,
) -> usize {
	let reserved_output_tokens = input.budget_tokens.min(512);
	let reserved_prompt_tokens = 300_u64;
	let available_tokens = input
		.budget_tokens
		.saturating_sub(reserved_output_tokens)
		.saturating_sub(reserved_prompt_tokens);
	if available_tokens < 64 {
		return 0;
	}

	usize::try_from(available_tokens.saturating_mul(3))
		.unwrap_or(max_skill_prompt_context_chars)
		.min(max_skill_prompt_context_chars)
}

fn skill_context_query(input: &ToolInput<'_>) -> String {
	let selected_skills = input
		.resource_selectors
		.iter()
		.filter_map(|selector| selector.strip_prefix("skill:"))
		.collect::<Vec<_>>();
	let request_text = if input.summary.is_empty() {
		input.goal.to_string()
	} else {
		format!("{}\n{}", input.goal, input.summary)
	};
	if selected_skills.is_empty() {
		request_text
	} else {
		format!("{}\n{}", selected_skills.join("\n"), request_text)
	}
}

fn execution_authority_block(input: &ToolInput<'_>) -> String {
	let side_effects_allowed = allows_state_change(&input.granted_capabilities);
	let selected_resources = if input.resource_selectors.is_empty() {
		"(none)".to_string()
	} else {
		input.resource_selectors.join(", ")
	};
	let granted_capabilities = if input.granted_capabilities.is_empty() {
		"(none)".to_string()
	} else {
		input.granted_capabilities.join(", ")
	};
	let side_effect_policy = if side_effects_allowed {
		"allowed"
	} else {
		"not allowed"
	};
	let guidance = if side_effects_allowed {
		"This invocation may perform real state changes when the selected resource supports them."
	} else {
		"This invocation is advisory only. It may explain, plan, or draft work, but it must not claim that files, directories, skills, or installations were created or modified."
	};

	format!(
		"\n\nExecution authority:\n- selected_resources: {selected_resources}\n- granted_capabilities: {granted_capabilities}\n- side_effects_allowed: {side_effect_policy}\n- guidance: {guidance}"
	)
}

fn allows_state_change(granted_capabilities: &[String]) -> bool {
	granted_capabilities.iter().any(|capability| {
		let capability = capability.to_ascii_lowercase();
		capability.contains("write")
			|| capability.contains("install")
			|| capability.contains("create")
			|| capability.contains("modify")
			|| capability.contains("delete")
			|| capability.contains("update")
	})
}

fn runtime_context_block() -> String {
	let now = current_runtime_time();
	let timestamp = now
		.format(&Rfc3339)
		.unwrap_or_else(|_| "unavailable".to_string());
	let date = now.date();
	let weekday = format!("{:?}", now.weekday());
	let utc_offset = format_utc_offset(now.offset());

	format!(
		"- local_timestamp: {timestamp}\n- local_date: {date}\n- local_weekday: {weekday}\n- utc_offset: {utc_offset}"
	)
}

fn current_runtime_time() -> OffsetDateTime {
	OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc())
}

fn selected_skill_from_input(input: &ToolInput<'_>) -> Option<String> {
	input
		.resource_selectors
		.iter()
		.find_map(|selector| selector.strip_prefix("skill:"))
		.map(str::to_string)
}

fn skill_name_is(left: &str, right: &str) -> bool {
	normalize_skill_name(left) == normalize_skill_name(right)
}

fn normalize_skill_name(value: &str) -> String {
	let mut normalized = String::new();
	let mut last_hyphen = false;
	for character in value.chars() {
		if character.is_ascii_alphanumeric() {
			normalized.push(character.to_ascii_lowercase());
			last_hyphen = false;
		} else if !last_hyphen && !normalized.is_empty() {
			normalized.push('-');
			last_hyphen = true;
		}
	}
	normalized.trim_matches('-').to_string()
}

fn allowed_script_paths(record: &InstalledSkillRecord) -> Vec<String> {
	let mut scripts = record
		.installed_files
		.iter()
		.filter(|path| path.starts_with("scripts/"))
		.cloned()
		.collect::<Vec<_>>();
	scripts.sort();
	scripts
}

fn generated_skill_root() -> Result<PathBuf, ToolFailure> {
	let configured = env::var("ROKU_SKILL_ROOT")
		.ok()
		.map(|value| value.trim().to_string())
		.filter(|value| !value.is_empty())
		.map(PathBuf::from)
		.unwrap_or_else(|| {
			env::current_dir()
				.map(|cwd| cwd.join(".roku").join("skills"))
				.unwrap_or_else(|_| PathBuf::from(".roku").join("skills"))
		});
	fs::create_dir_all(&configured).map_err(|error| {
		ToolFailure::terminal(format!("failed to create generated skill root: {error}"))
	})?;
	Ok(normalize_runtime_path(configured))
}

fn execute_skill_creator(
	registry: &SkillRegistry,
	router: Option<&LlmRouter>,
	record: &InstalledSkillRecord,
	input: &ToolInput<'_>,
	output_root: &Path,
	execution_request: &SkillExecutionRequest,
	max_output_chars: usize,
) -> Result<SkillExecutionResult, ToolFailure> {
	let plan = plan_skill_creator(router, record, input, execution_request)?;
	let skill_name = normalize_skill_name(&plan.skill_name);
	if skill_name.is_empty() {
		return Err(ToolFailure::terminal(
			"skill-creator execution plan did not produce a valid skill name",
		));
	}
	let skill_dir = output_root.join(&skill_name);
	if skill_dir.exists() {
		return Err(ToolFailure::terminal(format!(
			"generated skill target already exists: {}",
			display_path(&skill_dir)
		)));
	}

	fs::create_dir_all(&skill_dir).map_err(io_tool_failure)?;
	let mut created_paths = vec![display_path(&skill_dir)];
	for resource in normalized_resources(&plan.resources) {
		let resource_dir = skill_dir.join(resource);
		fs::create_dir_all(&resource_dir).map_err(io_tool_failure)?;
		created_paths.push(display_path(&resource_dir));
	}

	let skill_md_path = skill_dir.join("SKILL.md");
	fs::write(&skill_md_path, skill_markdown(&skill_name, &plan)).map_err(io_tool_failure)?;
	created_paths.push(display_path(&skill_md_path));

	let openai_yaml_path = skill_dir.join("agents").join("openai.yaml");
	if let Some(parent) = openai_yaml_path.parent() {
		fs::create_dir_all(parent).map_err(io_tool_failure)?;
	}
	fs::write(&openai_yaml_path, openai_yaml(&skill_name, &plan)).map_err(io_tool_failure)?;
	created_paths.push(display_path(&openai_yaml_path));

	let validation = run_skill_creator_validation(
		registry,
		&record.descriptor.name,
		&skill_dir,
		max_output_chars,
	)?;
	registry
		.register_local_skill(&skill_dir, "runtime")
		.map_err(|error| ToolFailure::terminal(error.to_string()))?;

	Ok(SkillExecutionResult {
		selected_skill: record.descriptor.name.clone(),
		execution_mode: Some(SkillExecutionMode::Executable),
		success: true,
		message: format!(
			"Created skill `{}` at {} and registered it locally.",
			skill_name,
			display_path(&skill_dir)
		),
		created_paths,
		executed_scripts: validation.executed_scripts,
		validation_status: Some(validation.validation_status),
		generated_skill_name: Some(skill_name),
	})
}

fn execute_script_backed_skill(
	router: Option<&LlmRouter>,
	record: &InstalledSkillRecord,
	skill_dir: &Path,
	input: &ToolInput<'_>,
	output_root: &Path,
	execution_request: &SkillExecutionRequest,
	max_output_chars: usize,
) -> Result<SkillExecutionResult, ToolFailure> {
	if !record.has_scripts() {
		return Err(ToolFailure::terminal(format!(
			"skill `{}` does not provide executable scripts",
			record.descriptor.name
		)));
	}
	let router = router.ok_or_else(|| {
		ToolFailure::terminal("script-backed skill execution requires a live llm router")
	})?;
	let plan = plan_script_execution(router, record, input, execution_request)?;
	let script_relpath = plan.script_relpath.as_deref().ok_or_else(|| {
		ToolFailure::terminal("skill execution plan did not choose a script path")
	})?;
	if !execution_request
		.allowed_script_paths
		.iter()
		.any(|path| path == script_relpath)
	{
		return Err(ToolFailure::terminal(format!(
			"skill execution plan selected disallowed script: {script_relpath}"
		)));
	}
	let script_path = skill_dir.join(script_relpath);
	let command_output = run_script_command(
		&script_path,
		&plan.script_args,
		skill_dir,
		[
			("ROKU_SKILL_ROOT", display_path(output_root)),
			("ROKU_GENERATED_SKILL_ROOT", display_path(output_root)),
		],
		max_output_chars,
	)?;
	let created_paths = collect_existing_expected_paths(output_root, &plan.expected_artifacts);
	Ok(SkillExecutionResult {
		selected_skill: record.descriptor.name.clone(),
		execution_mode: Some(SkillExecutionMode::Executable),
		success: true,
		message: format!(
			"Executed skill `{}` via `{}` successfully.\n{}",
			record.descriptor.name, script_relpath, command_output
		),
		created_paths,
		executed_scripts: vec![display_path(&script_path)],
		validation_status: Some("not_requested".to_string()),
		generated_skill_name: plan.generated_skill_name,
	})
}

fn plan_skill_creator(
	router: Option<&LlmRouter>,
	record: &InstalledSkillRecord,
	input: &ToolInput<'_>,
	execution_request: &SkillExecutionRequest,
) -> Result<SkillCreatorExecutionPlan, ToolFailure> {
	let router = router.ok_or_else(|| {
		ToolFailure::terminal("skill-creator execution requires a live llm router")
	})?;
	let prompt = format!(
		"Return JSON only.\nYou are planning a local skill scaffold.\nCurrent installed skill: {}\nUser goal: {}\nAllowed output root: {}\nKnown resources in the installed skill: {}\nProduce a compact JSON object with keys skill_name, description, overview, short_description, default_prompt, resources.\nRules:\n- skill_name must be lowercase hyphen-case.\n- description must say when to use the skill.\n- overview must be 1 short paragraph.\n- resources must be zero or more of scripts,references,assets.\n- Do not claim any files already exist.",
		record.descriptor.name,
		input.goal,
		execution_request
			.allowed_output_root
			.as_deref()
			.unwrap_or("(unknown)"),
		execution_request.allowed_script_paths.join(", "),
	);
	let response = router
		.generate(&GenerationRequest {
			system_prompt: Some(
				"You generate structured plans for local skill creation. Return JSON only."
					.to_string(),
			),
			prompt,
			expected_output_tokens: 300,
			risk_tier: RiskTier::Medium,
			preferred_provider: None,
			budget_tokens_remaining: input.budget_tokens,
			budget_cost_remaining_usd: 1.0,
		})
		.map_err(llm_failure)?;
	parse_json_reply::<SkillCreatorExecutionPlan>(&response.output).ok_or_else(|| {
		ToolFailure::terminal("skill-creator execution planner did not return valid json")
	})
}

fn plan_script_execution(
	router: &LlmRouter,
	record: &InstalledSkillRecord,
	input: &ToolInput<'_>,
	execution_request: &SkillExecutionRequest,
) -> Result<SkillExecutionPlan, ToolFailure> {
	let prompt = format!(
		"Return JSON only.\nYou are selecting a local skill script to execute.\nSkill: {}\nUser goal: {}\nAllowed scripts: {}\nAllowed output root: {}\nReturn keys selected_skill, execution_mode, script_relpath, script_args, expected_artifacts.\nRules:\n- execution_mode must be executable.\n- script_relpath must be one of the allowed scripts exactly.\n- script_args must be an array of strings.\n- expected_artifacts should list files or directories relative to the output root when the goal is expected to create output.\n- Do not claim the script already ran.",
		record.descriptor.name,
		input.goal,
		execution_request.allowed_script_paths.join(", "),
		execution_request
			.allowed_output_root
			.as_deref()
			.unwrap_or("(unknown)"),
	);
	let response = router
		.generate(&GenerationRequest {
			system_prompt: Some(
				"You plan safe local script execution for installed skills. Return JSON only."
					.to_string(),
			),
			prompt,
			expected_output_tokens: 240,
			risk_tier: RiskTier::Medium,
			preferred_provider: None,
			budget_tokens_remaining: input.budget_tokens,
			budget_cost_remaining_usd: 1.0,
		})
		.map_err(llm_failure)?;
	parse_json_reply::<SkillExecutionPlan>(&response.output)
		.ok_or_else(|| ToolFailure::terminal("skill execution planner did not return valid json"))
}

fn skill_markdown(skill_name: &str, plan: &SkillCreatorExecutionPlan) -> String {
	let title = display_name(skill_name);
	format!(
		"---\nname: {skill_name}\ndescription: {description}\n---\n\n# {title}\n\n## Overview\n\n{overview}\n\n## Usage\n\n- Activate this skill when the request matches {title} workflows.\n- Extend this file with concrete procedures, examples, and references as the skill evolves.\n",
		description = yaml_string(plan.description.trim()),
		overview = plan.overview.trim(),
	)
}

fn openai_yaml(skill_name: &str, plan: &SkillCreatorExecutionPlan) -> String {
	let display_name = display_name(skill_name);
	let short_description = plan.short_description.clone().unwrap_or_else(|| {
		truncate_short_description(&format!("Help with {display_name} workflows"))
	});
	let default_prompt = plan
		.default_prompt
		.clone()
		.unwrap_or_else(|| format!("Use the {skill_name} skill for this task."));
	format!(
		"interface:\n  display_name: {display_name}\n  short_description: {short_description}\n  default_prompt: {default_prompt}\n",
		display_name = yaml_string(&display_name),
		short_description = yaml_string(&short_description),
		default_prompt = yaml_string(&default_prompt),
	)
}

fn truncate_short_description(value: &str) -> String {
	let mut output = value.trim().to_string();
	if output.len() > 64 {
		output.truncate(64);
		output = output.trim().to_string();
	}
	if output.len() < 25 {
		output = format!("{output} helper");
	}
	output
}

fn display_name(skill_name: &str) -> String {
	skill_name
		.split('-')
		.filter(|segment| !segment.is_empty())
		.map(|segment| {
			let mut chars = segment.chars();
			match chars.next() {
				Some(first) => {
					format!("{}{}", first.to_ascii_uppercase(), chars.as_str())
				}
				None => String::new(),
			}
		})
		.collect::<Vec<_>>()
		.join(" ")
}

fn yaml_string(value: &str) -> String {
	format!(
		"\"{}\"",
		value
			.replace('\\', "\\\\")
			.replace('"', "\\\"")
			.replace('\n', "\\n")
	)
}

fn normalized_resources(resources: &[String]) -> Vec<&'static str> {
	let mut normalized = Vec::new();
	for resource in resources {
		match resource.trim().to_ascii_lowercase().as_str() {
			"scripts" if !normalized.contains(&"scripts") => normalized.push("scripts"),
			"references" if !normalized.contains(&"references") => normalized.push("references"),
			"assets" if !normalized.contains(&"assets") => normalized.push("assets"),
			_ => {}
		}
	}
	normalized
}

struct ValidationOutcome {
	executed_scripts: Vec<String>,
	validation_status: String,
}

fn run_skill_creator_validation(
	registry: &SkillRegistry,
	skill_name: &str,
	skill_dir: &Path,
	max_output_chars: usize,
) -> Result<ValidationOutcome, ToolFailure> {
	let creator_dir = registry
		.skill_dir(skill_name)
		.map_err(|error| ToolFailure::terminal(error.to_string()))?;
	let validation_script = creator_dir.join("scripts").join("quick_validate.py");
	if !validation_script.exists() {
		return Ok(ValidationOutcome {
			executed_scripts: Vec::new(),
			validation_status: "skipped_missing_script".to_string(),
		});
	}
	run_script_command(
		&validation_script,
		&[display_path(skill_dir)],
		&creator_dir,
		std::iter::empty::<(&str, String)>(),
		max_output_chars,
	)?;
	Ok(ValidationOutcome {
		executed_scripts: vec![display_path(&validation_script)],
		validation_status: "passed".to_string(),
	})
}

fn run_script_command(
	script_path: &Path,
	args: &[String],
	current_dir: &Path,
	envs: impl IntoIterator<Item = (&'static str, String)>,
	max_output_chars: usize,
) -> Result<String, ToolFailure> {
	let script_path = normalize_runtime_path(script_path.to_path_buf());
	let current_dir = normalize_runtime_path(current_dir.to_path_buf());
	let mut command = command_for_script(&script_path)?;
	command.current_dir(&current_dir);
	command.args(args);
	for (key, value) in envs {
		command.env(key, value);
	}
	let output = command.output().map_err(io_tool_failure)?;
	let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
	let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
	if !output.status.success() {
		let details = if stderr.is_empty() { stdout } else { stderr };
		return Err(ToolFailure::terminal(format!(
			"script execution failed for {}: {}",
			display_path(&script_path),
			truncate_execution_text(&details, max_output_chars)
		)));
	}
	let combined = if stdout.is_empty() { stderr } else { stdout };
	Ok(truncate_execution_text(&combined, max_output_chars))
}

fn normalize_runtime_path(path: PathBuf) -> PathBuf {
	if path.is_absolute() {
		path.canonicalize().unwrap_or(path)
	} else {
		let absolute = env::current_dir()
			.map(|cwd| cwd.join(&path))
			.unwrap_or_else(|_| PathBuf::from(".").join(path));
		absolute.canonicalize().unwrap_or(absolute)
	}
}

fn command_for_script(script_path: &Path) -> Result<Command, ToolFailure> {
	let extension = script_path
		.extension()
		.and_then(|value| value.to_str())
		.unwrap_or_default();
	let mut command = match extension {
		"py" => python_command()?,
		"sh" => Command::new("bash"),
		_ => Command::new(script_path),
	};
	if extension == "py" || extension == "sh" {
		command.arg(script_path);
	}
	Ok(command)
}

fn python_command() -> Result<Command, ToolFailure> {
	for candidate in ["python3", "python"] {
		let status = Command::new(candidate).arg("--version").status();
		if status.is_ok_and(|status| status.success()) {
			return Ok(Command::new(candidate));
		}
	}
	Err(ToolFailure::terminal(
		"python interpreter is required for skill execution but was not found",
	))
}

fn collect_existing_expected_paths(
	output_root: &Path,
	expected_artifacts: &[String],
) -> Vec<String> {
	let mut paths = expected_artifacts
		.iter()
		.map(|relative| output_root.join(relative))
		.filter(|path| path.exists())
		.map(|path| display_path(&path))
		.collect::<Vec<_>>();
	paths.sort();
	paths.dedup();
	paths
}

fn parse_json_reply<T>(value: &str) -> Option<T>
where
	T: DeserializeOwned,
{
	let trimmed = value.trim();
	serde_json::from_str::<T>(trimmed).ok().or_else(|| {
		let stripped = trimmed
			.strip_prefix("```json")
			.or_else(|| trimmed.strip_prefix("```"))
			.unwrap_or(trimmed)
			.trim();
		let stripped = stripped.strip_suffix("```").unwrap_or(stripped).trim();
		serde_json::from_str::<T>(stripped).ok()
	})
}

fn truncate_execution_text(value: &str, max_output_chars: usize) -> String {
	let trimmed = value.trim();
	if trimmed.chars().count() <= max_output_chars {
		return trimmed.to_string();
	}
	let shortened = trimmed.chars().take(max_output_chars).collect::<String>();
	format!("{shortened}\n[truncated]")
}

fn display_path(path: &Path) -> String {
	path.display().to_string()
}

fn io_tool_failure(error: std::io::Error) -> ToolFailure {
	ToolFailure::terminal(error.to_string())
}

pub(crate) fn inventory_context_json(
	resource_catalog: &ResourceCatalog,
	skill_registry: &SkillRegistry,
) -> String {
	serde_json::to_string_pretty(&RuntimeInventory::from_runtime(
		resource_catalog,
		skill_registry,
	))
	.unwrap_or_else(|_| "{}".to_string())
}

#[derive(Debug, Clone, Serialize)]
struct RuntimeInventory {
	tools: Vec<InventoryTool>,
	skills: Vec<InventorySkill>,
	capabilities: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct InventoryTool {
	name: String,
	role: Option<String>,
	description: String,
}

#[derive(Debug, Clone, Serialize)]
struct InventorySkill {
	name: String,
	description: String,
}

impl RuntimeInventory {
	fn from_runtime(resource_catalog: &ResourceCatalog, skill_registry: &SkillRegistry) -> Self {
		let mut tools = resource_catalog
			.entries()
			.iter()
			.filter(|entry| entry.kind == ResourceKind::Tool && entry.discoverable)
			.map(|entry| InventoryTool {
				name: entry.name.clone(),
				role: entry.role.clone(),
				description: short_description(&entry.description, 120),
			})
			.collect::<Vec<_>>();
		tools.sort_by(|left, right| left.name.cmp(&right.name));

		let mut capabilities = resource_catalog
			.entries()
			.iter()
			.filter(|entry| entry.kind == ResourceKind::Tool && entry.discoverable)
			.flat_map(|entry| entry.required_capabilities.iter().cloned())
			.collect::<Vec<_>>();
		capabilities.sort();
		capabilities.dedup();

		let mut skills = skill_registry
			.list_skills()
			.unwrap_or_default()
			.into_iter()
			.map(|record| InventorySkill {
				name: record.descriptor.name,
				description: short_description(&record.descriptor.description, 120),
			})
			.collect::<Vec<_>>();
		skills.sort_by(|left, right| left.name.cmp(&right.name));

		Self {
			tools,
			skills,
			capabilities,
		}
	}
}

fn short_description(value: &str, max_chars: usize) -> String {
	let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
	let mut chars = compact.chars();
	let truncated = chars.by_ref().take(max_chars).collect::<String>();
	if chars.next().is_some() {
		format!("{truncated}...")
	} else {
		truncated
	}
}

fn finalize_llm_message(worker_id: &str, _goal: &str, output: &str) -> String {
	let trimmed = output.trim();
	if trimmed.is_empty() {
		return String::new();
	}
	if worker_id != "generic-worker" {
		return trimmed.to_string();
	}

	let sanitized = sanitize_final_reply(trimmed);
	if sanitized.is_empty() {
		trimmed.to_string()
	} else {
		sanitized
	}
}

fn sanitize_final_reply(output: &str) -> String {
	if contains_pseudo_tool_call_markup(output) {
		let stripped = output
			.lines()
			.map(str::trim)
			.filter(|line| {
				!line.is_empty()
					&& !line.starts_with("<tool_calls>")
					&& !line.starts_with("</tool_calls>")
					&& !line.starts_with("<tool_call>")
					&& !line.starts_with("</tool_call>")
					&& !line.starts_with("<name>")
					&& !line.starts_with("</name>")
					&& !line.starts_with("<arguments>")
					&& !line.starts_with("</arguments>")
			})
			.collect::<Vec<_>>()
			.join("\n");
		if !stripped.trim().is_empty() {
			return strip_outer_quotes(stripped.trim()).to_string();
		}
	}

	if !contains_prompt_leakage(output) {
		return strip_outer_quotes(output.trim()).to_string();
	}

	let candidates = output
		.lines()
		.map(str::trim)
		.filter(|line| !line.is_empty())
		.filter(|line| !is_meta_line(line))
		.filter_map(sanitized_candidate)
		.collect::<Vec<_>>();
	if let Some(candidate) = candidates.last() {
		return candidate.clone();
	}
	if let Some(candidate) = quoted_answer_candidate(output) {
		return strip_outer_quotes(candidate.trim()).trim().to_string();
	}

	strip_outer_quotes(output.trim()).to_string()
}

fn general_execute_llm_output(
	terminal_output: bool,
	input: &ToolInput<'_>,
	request: &ToolInvocationRequest,
	response: &roku_plugin_llm::LlmResponse,
) -> Result<Value, ToolFailure> {
	let completion = parse_json_reply::<GeneralExecuteCompletion>(&response.output)
		.ok_or_else(|| ToolFailure::terminal("generic worker produced invalid completion json"))?;
	let message = sanitize_final_reply(&completion.final_message);
	if message.trim().is_empty() {
		return Err(ToolFailure::terminal(
			"generic worker completion json must include a non-empty final_message",
		));
	}
	let completion = normalized_general_execute_completion(completion, message);
	let data = json!({
		"worker_id": "generic-worker",
		"completion": completion.clone(),
		"raw_output": response.output,
		"task_id": input.task_id,
		"node_id": input.node_id,
		"goal": input.goal,
		"summary": input.summary,
		"provider": response.provider,
		"model_id": response.model_id,
		"prompt_tokens": response.prompt_tokens,
		"output_tokens": response.output_tokens,
		"latency_ms": response.latency_ms,
		"attempt": request.attempt,
		"invocation_key": request.invocation_key,
	});
	Ok(general_execute_tool_output(
		terminal_output,
		&completion,
		data,
	))
}

fn normalized_general_execute_completion(
	mut completion: GeneralExecuteCompletion,
	sanitized_message: String,
) -> GeneralExecuteCompletion {
	completion.final_message = sanitized_message;
	match completion.completion_kind {
		GeneralCompletionKind::GroundedAnswer => {
			completion.evidence_status = GeneralEvidenceStatus::Grounded;
			completion.missing_information.clear();
		}
		GeneralCompletionKind::NeedsMoreInformation => {
			completion.evidence_status = GeneralEvidenceStatus::MissingRequiredInput;
		}
		GeneralCompletionKind::InsufficientEvidence => {
			completion.evidence_status = GeneralEvidenceStatus::MissingExecutionEvidence;
			completion.missing_information.clear();
		}
	}
	completion
}

fn general_execute_tool_output(
	terminal_output: bool,
	completion: &GeneralExecuteCompletion,
	data: Value,
) -> Value {
	ToolOutputEnvelope::new(
		completion.completion_kind.ok(),
		completion.completion_kind.error_type().map(str::to_string),
		completion.completion_kind.terminal(terminal_output),
		completion.final_message.clone(),
		data,
	)
	.into_value()
}

fn contains_prompt_leakage(output: &str) -> bool {
	let lowercase = output.to_lowercase();
	lowercase.contains("user request:")
		|| lowercase.contains("trusted runtime context")
		|| lowercase.contains("output rules:")
		|| lowercase.contains("internal execution hint")
		|| lowercase.contains("the user's request is")
		|| lowercase.contains("conversation history shows")
		|| lowercase.contains("first, the user's request is")
		|| lowercase.contains("from the trusted runtime context")
}

fn contains_pseudo_tool_call_markup(output: &str) -> bool {
	let lowercase = output.to_lowercase();
	lowercase.contains("<tool_calls>")
		|| lowercase.contains("<tool_call>")
		|| lowercase.contains("<name>")
		|| lowercase.contains("<arguments>")
}

fn is_meta_line(line: &str) -> bool {
	let lowercase = line.to_lowercase();
	lowercase.starts_with("user request:")
		|| lowercase.starts_with("trusted runtime context:")
		|| lowercase.starts_with("internal execution hint")
		|| lowercase.starts_with("output rules:")
		|| lowercase.starts_with("conversation history")
		|| lowercase.starts_with("from the trusted runtime context")
		|| lowercase.starts_with("first, the user's request is")
		|| lowercase.starts_with("the user's request is")
		|| lowercase.starts_with("- local_")
		|| lowercase.starts_with("- the conversation history")
		|| lowercase.starts_with("- output")
		|| lowercase.contains("i should")
		|| lowercase.contains("i'll use")
		|| lowercase.contains("i'll output")
		|| lowercase.contains("do not narrate")
}

fn sanitized_candidate(line: &str) -> Option<String> {
	let quoted = quoted_answer_candidate(line).unwrap_or_else(|| line.to_string());
	let candidate = strip_outer_quotes(quoted.trim()).trim().to_string();
	if candidate.is_empty() || candidate.len() > 240 {
		return None;
	}
	Some(candidate)
}

fn quoted_answer_candidate(line: &str) -> Option<String> {
	for (open, close) in [('"', '"'), ('“', '”'), ('\'', '\''), ('‘', '’')] {
		if let Some(candidate) = between_last_pair(line, open, close) {
			return Some(candidate);
		}
	}
	None
}

fn between_last_pair(value: &str, open: char, close: char) -> Option<String> {
	let end = value.rfind(close)?;
	let start = value[..end].rfind(open)?;
	if start >= end {
		return None;
	}
	Some(value[start + open.len_utf8()..end].to_string())
}

fn strip_outer_quotes(value: &str) -> &str {
	let trimmed = value.trim();
	if trimmed.len() >= 2 {
		let first = trimmed.chars().next().unwrap_or_default();
		let last = trimmed.chars().last().unwrap_or_default();
		if matches!(
			(first, last),
			('"', '"') | ('\'', '\'') | ('“', '”') | ('‘', '’')
		) {
			return &trimmed[first.len_utf8()..trimmed.len() - last.len_utf8()];
		}
	}
	trimmed
}

fn log_runtime_output(message: &str, fields: impl IntoIterator<Item = (&'static str, String)>) {
	let mut record = LogRecord::new("roku-agent-runtime", LogLevel::Info, message);
	for (key, value) in fields {
		record = record.with_field(key, value);
	}
	let _ = emit_global_log(record);
}

fn format_utc_offset(offset: UtcOffset) -> String {
	let seconds = offset.whole_seconds();
	let sign = if seconds < 0 { '-' } else { '+' };
	let absolute_seconds = seconds.abs();
	let hours = absolute_seconds / 3600;
	let minutes = (absolute_seconds % 3600) / 60;
	format!("{sign}{hours:02}:{minutes:02}")
}

pub(crate) fn first_url_in_text(value: &str) -> Option<&str> {
	value
		.split_whitespace()
		.map(|part| {
			part.trim_matches(|character: char| {
				matches!(
					character,
					'(' | ')'
						| '[' | ']' | '{' | '}'
						| '<' | '>' | '"' | '\''
						| ',' | ';' | '.' | '!'
						| '?'
				)
			})
		})
		.find(|part| {
			(part.starts_with("https://") || part.starts_with("http://"))
				&& url::Url::parse(part).is_ok()
		})
}

struct ToolInput<'a> {
	task_id: &'a str,
	node_id: &'a str,
	goal: &'a str,
	summary: &'a str,
	conversation_history: &'a str,
	runtime_memory_sections: RuntimeMemorySections,
	granted_capabilities: Vec<String>,
	resource_selectors: Vec<String>,
	budget_tokens: u64,
	time_budget_ms: u64,
}

fn request_input(request: &ToolInvocationRequest) -> Result<ToolInput<'_>, ToolFailure> {
	let Some(input) = request.input.as_object() else {
		return Err(ToolFailure::terminal("tool input must be a json object"));
	};

	Ok(ToolInput {
		task_id: input
			.get("task_id")
			.and_then(Value::as_str)
			.unwrap_or_default(),
		node_id: input
			.get("node_id")
			.and_then(Value::as_str)
			.unwrap_or_default(),
		goal: input
			.get("goal")
			.and_then(Value::as_str)
			.unwrap_or_default(),
		summary: input
			.get("summary")
			.and_then(Value::as_str)
			.unwrap_or_default(),
		conversation_history: input
			.get("conversation_history")
			.and_then(Value::as_str)
			.unwrap_or_default(),
		runtime_memory_sections: input
			.get("runtime_memory_sections")
			.cloned()
			.map(serde_json::from_value)
			.transpose()
			.map_err(|error| {
				ToolFailure::terminal(format!(
					"runtime_memory_sections must be a valid object: {error}"
				))
			})?
			.unwrap_or_default(),
		granted_capabilities: input
			.get("granted_capabilities")
			.and_then(Value::as_array)
			.map(|values| {
				values
					.iter()
					.filter_map(Value::as_str)
					.map(str::to_string)
					.collect::<Vec<_>>()
			})
			.unwrap_or_default(),
		resource_selectors: input
			.get("resource_selectors")
			.and_then(Value::as_array)
			.map(|values| {
				values
					.iter()
					.filter_map(Value::as_str)
					.map(str::to_string)
					.collect::<Vec<_>>()
			})
			.unwrap_or_default(),
		budget_tokens: input
			.get("budget_tokens")
			.and_then(Value::as_u64)
			.unwrap_or_default(),
		time_budget_ms: input
			.get("time_budget_ms")
			.and_then(Value::as_u64)
			.unwrap_or_default(),
	})
}

fn tool_descriptor(
	name: &str,
	required_capabilities: Vec<String>,
	sandbox_profile: SandboxProfile,
	timeout_ms: u64,
) -> ToolDescriptor {
	ToolDescriptor {
		name: name.to_string(),
		version: "1.0.0".to_string(),
		input_schema: ToolSchema {
			required_fields: vec![
				"task_id".to_string(),
				"node_id".to_string(),
				"goal".to_string(),
				"summary".to_string(),
				"conversation_history".to_string(),
				"budget_tokens".to_string(),
				"time_budget_ms".to_string(),
			],
		},
		output_schema: "result.v1".to_string(),
		required_capabilities,
		runtime_constraints: RuntimeConstraints {
			timeout_ms,
			max_retries: 0,
			retry_backoff_ms: 0,
			sandbox_profile,
			deterministic_hooks: true,
			allowed_read_roots: Vec::new(),
			allowed_write_roots: Vec::new(),
		},
		contract: Some(ToolContract::default()),
	}
}

fn configured_tool_descriptor(
	tool: &ConfiguredTool,
	sandbox_profile: SandboxProfile,
	timeout_ms: u64,
) -> ToolDescriptor {
	let contract = tool
		.contract
		.clone()
		.or_else(|| Some(ToolContract::default()));
	ToolDescriptor {
		name: tool.name.clone(),
		version: "1.0.0".to_string(),
		input_schema: ToolSchema {
			required_fields: configured_required_fields(tool),
		},
		output_schema: contract
			.as_ref()
			.map(|contract| contract.output.observation_schema.clone())
			.unwrap_or_else(|| "result.v1".to_string()),
		required_capabilities: tool.required_capabilities.clone(),
		runtime_constraints: RuntimeConstraints {
			timeout_ms,
			max_retries: 0,
			retry_backoff_ms: 0,
			sandbox_profile,
			deterministic_hooks: true,
			allowed_read_roots: Vec::new(),
			allowed_write_roots: Vec::new(),
		},
		contract,
	}
}

fn tool_catalog_descriptor(tool: &ConfiguredTool) -> CatalogDescriptor {
	let selection_hint = configured_selection_hint(tool);
	let contract = tool
		.contract
		.clone()
		.or_else(|| Some(ToolContract::default()));
	CatalogDescriptor {
		selector: ResourceSelector::tool(&tool.name),
		kind: ResourceKind::Tool,
		name: tool.name.clone(),
		role: Some(tool.role.as_str().to_string()),
		description: tool.description.clone(),
		selection_hint: selection_hint.clone(),
		discoverable: tool.discoverable,
		tags: tool.tags.clone(),
		examples: tool.examples.clone(),
		input_schema: configured_catalog_input_schema(tool),
		risk: tool.risk,
		cost: tool.cost.clone(),
		required_capabilities: tool.required_capabilities.clone(),
		summary: selection_hint,
		key_commands: Vec::new(),
		use_cases: Vec::new(),
		contract,
	}
}

fn configured_selection_hint(tool: &ConfiguredTool) -> String {
	let selection_hint = tool.selection_hint.trim();
	if !selection_hint.is_empty() {
		return selection_hint.to_string();
	}
	tool.description.trim().to_string()
}

fn configured_catalog_input_schema(tool: &ConfiguredTool) -> Vec<String> {
	tool.contract
		.as_ref()
		.map(|contract| contract.input.field_names())
		.filter(|fields| !fields.is_empty())
		.unwrap_or_else(|| tool.input_schema.clone())
}

fn configured_required_fields(tool: &ConfiguredTool) -> Vec<String> {
	let mut required_fields = vec![
		"task_id".to_string(),
		"node_id".to_string(),
		"goal".to_string(),
		"summary".to_string(),
		"conversation_history".to_string(),
		"budget_tokens".to_string(),
		"time_budget_ms".to_string(),
	];
	for field in &tool.input_schema {
		if !required_fields.iter().any(|existing| existing == field) {
			required_fields.push(field.clone());
		}
	}
	if let Some(contract) = tool.contract.as_ref() {
		for field in contract.input.required_field_names() {
			if !required_fields.iter().any(|existing| existing == &field) {
				required_fields.push(field);
			}
		}
	}
	required_fields
}

fn successful_tool_output(terminal: bool, message: impl Into<String>, data: Value) -> Value {
	ToolOutputEnvelope::new(true, Option::<String>::None, terminal, message, data).into_value()
}

fn builtin_tool_is_runtime_enabled(tool: &ConfiguredTool, skill_execution_enabled: bool) -> bool {
	!matches!(tool.role, BuiltinToolRole::SkillExecute) || skill_execution_enabled
}

fn worker_id_for_role(role: BuiltinToolRole) -> &'static str {
	match role {
		BuiltinToolRole::SkillInstall => "skill-worker",
		BuiltinToolRole::SkillExecute => "skill-execute-worker",
		BuiltinToolRole::Inventory => "inventory-worker",
		BuiltinToolRole::Research => "research-worker",
		BuiltinToolRole::Data => "data-worker",
		BuiltinToolRole::Review => "review-worker",
		BuiltinToolRole::General => "generic-worker",
	}
}

fn sandbox_profile_for_role(role: BuiltinToolRole) -> SandboxProfile {
	match role {
		BuiltinToolRole::SkillInstall => SandboxProfile::ReadOnlyFs,
		BuiltinToolRole::SkillExecute => SandboxProfile::ContainerRestricted,
		BuiltinToolRole::Inventory => SandboxProfile::NoIsolation,
		BuiltinToolRole::Research => SandboxProfile::PythonResearch,
		BuiltinToolRole::Data => SandboxProfile::ContainerRestricted,
		BuiltinToolRole::Review => SandboxProfile::ReadOnlyFs,
		BuiltinToolRole::General => SandboxProfile::NoIsolation,
	}
}

fn completion_message_for_role(role: BuiltinToolRole) -> &'static str {
	match role {
		BuiltinToolRole::SkillInstall => {
			"deterministic placeholder only: skill installation was not executed by a live runtime"
		}
		BuiltinToolRole::SkillExecute => {
			"deterministic placeholder only: skill execution was not performed by a live runtime"
		}
		BuiltinToolRole::Inventory => {
			"deterministic placeholder only: inventory summary was not generated by a live runtime"
		}
		BuiltinToolRole::Research => {
			"deterministic placeholder only: research synthesis was not generated by a live runtime"
		}
		BuiltinToolRole::Data => {
			"deterministic placeholder only: data processing was not executed by a live runtime"
		}
		BuiltinToolRole::Review => {
			"deterministic placeholder only: review checks were not executed by a live runtime"
		}
		BuiltinToolRole::General => {
			"deterministic placeholder only: general execution did not use a live runtime"
		}
	}
}

fn system_prompt_for_role(role: BuiltinToolRole) -> &'static str {
	match role {
		BuiltinToolRole::SkillInstall => {
			"You install skills from explicit source URLs and report the result."
		}
		BuiltinToolRole::SkillExecute => {
			"You execute installed script-backed skills through structured runtime plans. Never claim side effects happened unless the runtime returns execution evidence."
		}
		BuiltinToolRole::Inventory => {
			"You are Roku's inventory worker. Use only the authoritative local inventory JSON in the prompt to describe installed skills, discoverable tools, and capability families. Adapt the formatting to the user's request and conversation history, including list or bullet formatting when asked. Do not invent tools, skills, or capabilities that are not present in the inventory JSON."
		}
		BuiltinToolRole::Research => {
			"You are Roku's research worker. Produce grounded intermediate findings in plain text for downstream use. Never expose chain-of-thought, hidden reasoning, or internal runtime details."
		}
		BuiltinToolRole::Data => {
			"You are Roku's data worker. Produce the requested data-processing or synthesis result in plain text. Never expose chain-of-thought, hidden reasoning, or internal runtime details."
		}
		BuiltinToolRole::Review => {
			"You are Roku's review worker. Produce a concise review or validation conclusion in plain text. Never expose chain-of-thought, hidden reasoning, or internal runtime details."
		}
		BuiltinToolRole::General => {
			"You are Roku. Return only a JSON object that matches the completion contract described in the prompt. Use only grounded evidence present in the trusted runtime context and tool observations. Do not claim that you executed shell commands, read files, searched the web, or observed outputs unless the prompt includes explicit evidence for those actions. If the current context is insufficient to support the requested claim, mark it as insufficient evidence instead of inventing details. If the user must clarify missing required input, mark it as needs_more_information. Never reveal hidden reasoning, analysis steps, or internal runtime details."
		}
	}
}

fn risk_tier_for_role(role: BuiltinToolRole) -> RiskTier {
	match role {
		BuiltinToolRole::Review => RiskTier::High,
		BuiltinToolRole::SkillExecute => RiskTier::High,
		BuiltinToolRole::Inventory
		| BuiltinToolRole::Research
		| BuiltinToolRole::Data
		| BuiltinToolRole::General => RiskTier::Medium,
		BuiltinToolRole::SkillInstall => RiskTier::Low,
	}
}

fn llm_failure(error: LlmAdapterError) -> ToolFailure {
	match error {
		LlmAdapterError::BudgetExceeded(message) => ToolFailure::terminal(message),
		LlmAdapterError::LatencyExceeded {
			latency_ms,
			max_latency_ms,
		} => ToolFailure::terminal(format!(
			"llm latency exceeded policy: latency={latency_ms}ms max={max_latency_ms}ms"
		)),
		LlmAdapterError::CircuitOpen {
			provider,
			retry_after_ms,
		} => ToolFailure::terminal(format!(
			"llm provider circuit is open for {provider}; retry after {retry_after_ms}ms"
		)),
		LlmAdapterError::NoEligibleModel => {
			ToolFailure::terminal("no eligible llm model for request")
		}
		LlmAdapterError::ProviderNotRegistered(provider) => {
			ToolFailure::terminal(format!("llm provider is not registered: {provider}"))
		}
		LlmAdapterError::ProviderCallFailed {
			provider,
			model_id,
			message,
		} => ToolFailure::terminal(format!(
			"llm provider call failed for {provider}/{model_id}: {message}"
		)),
	}
}

#[cfg(test)]
mod tests {
	use serde_json::json;
	use std::fs;
	use std::io::{Cursor, Write};
	use std::sync::{Arc, Mutex};

	use super::{
		PromptedLlmTool, allowed_script_paths, build_resource_catalog, completion_message_for_role,
		execute_skill_creator, first_url_in_text, inventory_context_json, request_input,
		runtime_context_block, sanitize_final_reply, user_visible_prompt,
	};
	use crate::config::{BuiltinToolRole, ToolCatalogConfig};
	use crate::runtime_config::ToolWorkerRuntimeConfig;
	use roku_common_types::RuntimeMemorySections;
	use roku_plugin_host::{SandboxProfile, Tool, ToolInvocationRequest};
	use roku_plugin_llm::{
		GenerationRequest, LlmProvider, LlmRouter, ModelProfile, ProviderCallError,
		ProviderResponse, RiskTier, RoutingPolicy,
	};
	use roku_plugin_skills::{
		DownloadedArchive, SkillArchiveFetcher, SkillRegistry, SkillRegistryError, SkillSource,
	};

	#[test]
	fn runtime_context_block_contains_date_and_weekday() {
		let context = runtime_context_block();
		assert!(context.contains("local_date"));
		assert!(context.contains("local_weekday"));
		assert!(context.contains("utc_offset"));
	}

	#[test]
	fn user_visible_prompt_includes_runtime_context_and_direct_answer_rules() {
		let request = ToolInvocationRequest {
			invocation_key: "invoke-1".to_string(),
			input: json!({
				"task_id": "task-1",
				"node_id": "node-1",
			"goal": "今天是星期几？",
			"summary": "Execute primary action",
			"conversation_history": "user: 你好",
			"runtime_memory_sections": {
				"long_term_recall": "memory hit summary"
			},
			"granted_capabilities": ["inventory.read"],
			"budget_tokens": 2048_u64,
				"time_budget_ms": 45_000_u64
			}),
			attempt: 1,
			sandbox_profile: SandboxProfile::NoIsolation,
			attachments: Vec::new(),
			allowed_read_roots: Vec::new(),
			allowed_write_roots: Vec::new(),
		};

		let input = request_input(&request).expect("tool input should parse");
		let prompt = user_visible_prompt(
			&input,
			"generic-worker",
			"invoke-1",
			None,
			Some("- tool `general.execute` (general): Handle direct conversation."),
		);

		assert!(prompt.contains("Trusted runtime context"));
		assert!(prompt.contains("Never narrate your reasoning"));
		assert!(prompt.contains("use the trusted runtime context above"));
		assert!(prompt.contains("Conversation history"));
		assert!(prompt.contains("Long-term recall (Roku-owned):"));
		assert!(prompt.contains("Authoritative local inventory JSON"));
		assert!(prompt.contains("Execution authority"));
		assert!(prompt.contains("side_effects_allowed"));
	}

	#[test]
	fn user_visible_prompt_renders_structured_runtime_memory_sections() {
		let request = ToolInvocationRequest {
			invocation_key: "invoke-2".to_string(),
			input: json!({
				"task_id": "task-1",
				"node_id": "node-1",
				"goal": "继续当前 memory 调研",
				"summary": "Execute primary action",
				"conversation_history": "user: 继续",
				"runtime_memory_sections": {
					"short_term_continuity": "- user: 继续",
					"long_term_recall": "- memory-record-1 | UserPreference | Rust preference",
					"working_memory": "Pending follow-up: keep runtime ownership explicit."
				},
				"granted_capabilities": ["inventory.read"],
				"budget_tokens": 2048_u64,
				"time_budget_ms": 45_000_u64
			}),
			attempt: 1,
			sandbox_profile: SandboxProfile::NoIsolation,
			attachments: Vec::new(),
			allowed_read_roots: Vec::new(),
			allowed_write_roots: Vec::new(),
		};

		let input = request_input(&request).expect("tool input should parse");
		let prompt = user_visible_prompt(&input, "generic-worker", "invoke-2", None, None);

		assert!(prompt.contains("Short-term continuity (Roku-owned):"));
		assert!(prompt.contains("Long-term recall (Roku-owned):"));
		assert!(prompt.contains("Working memory (Roku-owned):"));
		assert!(!prompt.contains("Relevant long-term memory (Roku-owned):\nlegacy fallback blob"));
	}

	#[test]
	fn sanitize_final_reply_collapses_prompt_leakage() {
		let output = r#"First, the user's request is: "今天周几？"

From the trusted runtime context:
- local_weekday: Sunday

So, I'll output: "星期日""#;
		assert_eq!(sanitize_final_reply(output), "星期日");
	}

	#[test]
	fn sanitize_final_reply_strips_pseudo_tool_call_markup() {
		let output = "我先继续处理。\n\n<tool_calls>\n<tool_call>\n<name>command.run</name>\n<arguments>{\"command\":\"pwd\"}</arguments>\n</tool_call>\n</tool_calls>";
		assert_eq!(sanitize_final_reply(output), "我先继续处理。");
	}

	#[test]
	fn general_worker_returns_structured_completion_contract() {
		let tool_config = ToolCatalogConfig::default();
		let registry = SkillRegistry::disabled();
		let catalog = build_resource_catalog(&registry, &tool_config);
		let mut router = LlmRouter::new(RoutingPolicy {
			max_request_cost_usd: 1.0,
			max_latency_ms: 5_000,
		});
		router.register_provider(StaticOutputProvider {
			output: general_completion_json(
				"当前证据已经足够回答。",
				"grounded_answer",
				"grounded",
			),
		});
		router.register_model(ModelProfile {
			model_id: "structured-general-model".to_string(),
			provider: "static-output-provider".to_string(),
			max_context_tokens: 16_000,
			cost_per_1k_tokens_usd: 0.0,
			max_risk_tier: RiskTier::Critical,
			route_priority: 100,
		});
		let general_tool = tool_config
			.tool_for_role(BuiltinToolRole::General)
			.expect("general tool should exist");
		let tool = PromptedLlmTool::from_config(general_tool, registry, Arc::new(router), catalog);

		let output = tool
			.invoke(ToolInvocationRequest {
				invocation_key: "invoke-1".to_string(),
				input: json!({
					"task_id": "task-1",
					"node_id": "node-1",
					"goal": "请总结一下。",
					"summary": "Use grounded evidence only",
					"conversation_history": "",
					"granted_capabilities": [],
					"budget_tokens": 2048_u64,
					"time_budget_ms": 45_000_u64
				}),
				attempt: 1,
				sandbox_profile: SandboxProfile::NoIsolation,
				attachments: Vec::new(),
				allowed_read_roots: Vec::new(),
				allowed_write_roots: Vec::new(),
			})
			.expect("invoke should succeed");

		assert_eq!(output["ok"], true);
		assert_eq!(output["terminal"], true);
		assert_eq!(output["message"], "当前证据已经足够回答。");
		assert_eq!(
			output["data"]["completion"]["completion_kind"],
			"grounded_answer"
		);
		assert_eq!(output["data"]["completion"]["evidence_status"], "grounded");
	}

	#[test]
	fn first_url_in_text_extracts_wrapped_skill_url() {
		let goal = "Please install skill from (https://github.com/anthropics/skills/tree/main/skills/claude-api).";
		assert_eq!(
			first_url_in_text(goal),
			Some("https://github.com/anthropics/skills/tree/main/skills/claude-api")
		);
	}

	#[test]
	fn resource_catalog_keeps_skill_install_descriptor_for_runtime_resolution() {
		let tool_config = ToolCatalogConfig::default();
		let skill_install_tool = tool_config
			.tool_for_role(BuiltinToolRole::SkillInstall)
			.expect("skill install tool should exist");
		let catalog = build_resource_catalog(&SkillRegistry::disabled(), &tool_config);

		assert!(
			catalog
				.entries()
				.iter()
				.any(|entry| entry.name == skill_install_tool.name)
		);
	}

	#[derive(Clone)]
	struct StaticArchiveFetcher {
		archive: DownloadedArchive,
	}

	impl SkillArchiveFetcher for StaticArchiveFetcher {
		fn fetch(&self, _source: &SkillSource) -> Result<DownloadedArchive, SkillRegistryError> {
			Ok(self.archive.clone())
		}
	}

	struct RoutedArchiveFetcher {
		archives: std::collections::HashMap<String, DownloadedArchive>,
	}

	impl SkillArchiveFetcher for RoutedArchiveFetcher {
		fn fetch(&self, source: &SkillSource) -> Result<DownloadedArchive, SkillRegistryError> {
			self.archives
				.get(source.original_url())
				.cloned()
				.ok_or_else(|| {
					SkillRegistryError::UnsupportedSourceUrl(source.original_url().to_string())
				})
		}
	}

	struct CapturingProvider {
		prompt: Arc<Mutex<Option<String>>>,
	}

	impl LlmProvider for CapturingProvider {
		fn provider_name(&self) -> &'static str {
			"capturing-provider"
		}

		fn complete(
			&self,
			_model: &ModelProfile,
			request: &GenerationRequest,
		) -> Result<ProviderResponse, ProviderCallError> {
			*self.prompt.lock().expect("prompt lock should succeed") = Some(request.prompt.clone());
			Ok(ProviderResponse {
				output: general_completion_json("done", "grounded_answer", "grounded"),
				finish_reason: None,
				prompt_tokens: 12,
				output_tokens: 4,
				latency_ms: 10,
			})
		}
	}

	struct StaticOutputProvider {
		output: String,
	}

	impl LlmProvider for StaticOutputProvider {
		fn provider_name(&self) -> &'static str {
			"static-output-provider"
		}

		fn complete(
			&self,
			_model: &ModelProfile,
			_request: &GenerationRequest,
		) -> Result<ProviderResponse, ProviderCallError> {
			Ok(ProviderResponse {
				output: self.output.clone(),
				finish_reason: None,
				prompt_tokens: 12,
				output_tokens: 32,
				latency_ms: 10,
			})
		}
	}

	#[test]
	fn prompted_tool_injects_installed_skill_context_when_referenced() {
		let tool_config = ToolCatalogConfig::default();
		let root = tempfile::tempdir().expect("temp root should exist");
		let registry = SkillRegistry::file_backed(root.path().join("skills")).with_fetcher(
			Arc::new(StaticArchiveFetcher {
				archive: DownloadedArchive {
					archive_url: "https://example.com/archive.zip".to_string(),
					bytes: test_skill_archive_bytes(),
					resolved_reference: Some("main".to_string()),
				},
			}),
		);
		registry
			.install_from_url(
				"https://github.com/anthropics/skills/tree/main/skills/claude-api",
				"test-suite",
			)
			.expect("install should succeed");

		let captured_prompt = Arc::new(Mutex::new(None));
		let mut router = LlmRouter::new(RoutingPolicy {
			max_request_cost_usd: 1.0,
			max_latency_ms: 5_000,
		});
		router.register_provider(CapturingProvider {
			prompt: Arc::clone(&captured_prompt),
		});
		router.register_model(ModelProfile {
			model_id: "capturing-model".to_string(),
			provider: "capturing-provider".to_string(),
			max_context_tokens: 16_000,
			cost_per_1k_tokens_usd: 0.0,
			max_risk_tier: RiskTier::Critical,
			route_priority: 100,
		});
		let general_tool = tool_config
			.tool_for_role(BuiltinToolRole::General)
			.expect("general tool should exist");
		let catalog = build_resource_catalog(&registry, &tool_config);

		let tool = PromptedLlmTool::from_config(general_tool, registry, Arc::new(router), catalog);
		tool.invoke(ToolInvocationRequest {
			invocation_key: "invoke-1".to_string(),
			input: json!({
				"task_id": "task-1",
				"node_id": "node-1",
				"goal": "Please use the claude-api skill for this request.",
				"summary": "Execute primary action",
				"conversation_history": "",
				"granted_capabilities": [],
				"budget_tokens": 2048_u64,
				"time_budget_ms": 45_000_u64
			}),
			attempt: 1,
			sandbox_profile: SandboxProfile::NoIsolation,
			attachments: Vec::new(),
			allowed_read_roots: Vec::new(),
			allowed_write_roots: Vec::new(),
		})
		.expect("invoke should succeed");

		let prompt = captured_prompt
			.lock()
			.expect("prompt lock should succeed")
			.clone()
			.expect("prompt should be captured");
		assert!(prompt.contains("Authoritative installed skill excerpts"));
		assert!(prompt.contains("### skill: claude-api"));
	}

	#[test]
	fn inventory_context_json_lists_all_installed_skills_and_discoverable_tools() {
		let tool_config = ToolCatalogConfig::default();
		let root = tempfile::tempdir().expect("temp root should exist");
		let registry = SkillRegistry::file_backed(root.path().join("skills")).with_fetcher(
			Arc::new(RoutedArchiveFetcher {
				archives: [
					(
						"https://github.com/anthropics/skills/tree/main/skills/claude-api"
							.to_string(),
						DownloadedArchive {
							archive_url: "https://example.com/claude-api.zip".to_string(),
							bytes: test_skill_archive_bytes(),
							resolved_reference: Some("main".to_string()),
						},
					),
					(
						"https://github.com/anthropics/skills/tree/main/skills/skill-creator"
							.to_string(),
						DownloadedArchive {
							archive_url: "https://example.com/skill-creator.zip".to_string(),
							bytes: test_skill_archive_bytes_for(
								"skill-creator",
								"Build and evaluate new skills.",
								"Use this skill to create and iterate on Codex skills.",
							),
							resolved_reference: Some("main".to_string()),
						},
					),
					(
						"https://github.com/anthropics/skills/tree/main/skills/xlsx".to_string(),
						DownloadedArchive {
							archive_url: "https://example.com/xlsx.zip".to_string(),
							bytes: test_skill_archive_bytes_for(
								"xlsx",
								"Read and write XLSX spreadsheets.",
								"Use this skill when the user needs spreadsheet import/export work.",
							),
							resolved_reference: Some("main".to_string()),
						},
					),
				]
				.into_iter()
				.collect(),
			}),
		);
		for url in [
			"https://github.com/anthropics/skills/tree/main/skills/claude-api",
			"https://github.com/anthropics/skills/tree/main/skills/skill-creator",
			"https://github.com/anthropics/skills/tree/main/skills/xlsx",
		] {
			registry
				.install_from_url(url, "test-suite")
				.expect("install should succeed");
		}
		let catalog = build_resource_catalog(&registry, &tool_config);
		let inventory_json = inventory_context_json(&catalog, &registry);
		assert!(inventory_json.contains("\"claude-api\""));
		assert!(inventory_json.contains("\"skill-creator\""));
		assert!(inventory_json.contains("\"xlsx\""));
		assert!(inventory_json.contains("\"inventory.describe\""));
		assert!(inventory_json.contains("\"research.synthesize\""));
		assert!(inventory_json.contains("\"data.execute\""));
		assert!(inventory_json.contains("\"review.assess\""));
		assert!(inventory_json.contains("\"inventory.read\""));
		assert!(!inventory_json.contains("skill.ensure_installed"));
	}

	#[test]
	fn deterministic_worker_report_messages_are_explicit_placeholders() {
		assert!(
			completion_message_for_role(BuiltinToolRole::Inventory)
				.contains("deterministic placeholder only")
		);
		assert!(completion_message_for_role(BuiltinToolRole::General).contains("live runtime"));
	}

	#[test]
	fn inventory_tool_injects_inventory_json_for_followup_formatting() {
		let tool_config = ToolCatalogConfig::default();
		let registry = SkillRegistry::disabled();
		let catalog = build_resource_catalog(&registry, &tool_config);
		let captured_prompt = Arc::new(Mutex::new(None));
		let mut router = LlmRouter::new(RoutingPolicy {
			max_request_cost_usd: 1.0,
			max_latency_ms: 5_000,
		});
		router.register_provider(CapturingProvider {
			prompt: Arc::clone(&captured_prompt),
		});
		router.register_model(ModelProfile {
			model_id: "capturing-model".to_string(),
			provider: "capturing-provider".to_string(),
			max_context_tokens: 16_000,
			cost_per_1k_tokens_usd: 0.0,
			max_risk_tier: RiskTier::Critical,
			route_priority: 100,
		});
		let inventory_tool = tool_config
			.tool_for_role(BuiltinToolRole::Inventory)
			.expect("inventory tool should exist");

		let tool =
			PromptedLlmTool::from_config(inventory_tool, registry, Arc::new(router), catalog);
		tool.invoke(ToolInvocationRequest {
			invocation_key: "invoke-1".to_string(),
			input: json!({
				"task_id": "task-1",
				"node_id": "node-1",
				"goal": "用无序列表列一下",
				"summary": "Use selected tool `inventory.describe`",
				"conversation_history": "user: 列出 skill、tool\nassistant: 已安装的 skills: xlsx。可用工具: data.execute。能力类别: data.read。",
				"granted_capabilities": ["inventory.read"],
				"budget_tokens": 2048_u64,
				"time_budget_ms": 45_000_u64
			}),
			attempt: 1,
			sandbox_profile: SandboxProfile::NoIsolation,
			attachments: Vec::new(),
			allowed_read_roots: Vec::new(),
			allowed_write_roots: Vec::new(),
		})
		.expect("invoke should succeed");

		let prompt = captured_prompt
			.lock()
			.expect("prompt lock should succeed")
			.clone()
			.expect("prompt should be captured");
		assert!(prompt.contains("Authoritative local inventory JSON"));
		assert!(prompt.contains("\"inventory.describe\""));
		assert!(prompt.contains("\"inventory.read\""));
	}

	#[test]
	fn prompted_tool_includes_execution_authority_for_skill_creation_requests() {
		let tool_config = ToolCatalogConfig::default();
		let root = tempfile::tempdir().expect("temp root should exist");
		let registry = SkillRegistry::file_backed(root.path().join("skills")).with_fetcher(
			Arc::new(StaticArchiveFetcher {
				archive: DownloadedArchive {
					archive_url: "https://example.com/archive.zip".to_string(),
					bytes: test_skill_archive_bytes_for(
						"skill-creator",
						"Build and evaluate new skills.",
						"Use this skill to create and iterate on Codex skills.",
					),
					resolved_reference: Some("main".to_string()),
				},
			}),
		);
		registry
			.install_from_url(
				"https://github.com/anthropics/skills/tree/main/skills/skill-creator",
				"test-suite",
			)
			.expect("install should succeed");
		let captured_prompt = Arc::new(Mutex::new(None));
		let mut router = LlmRouter::new(RoutingPolicy {
			max_request_cost_usd: 1.0,
			max_latency_ms: 5_000,
		});
		router.register_provider(CapturingProvider {
			prompt: Arc::clone(&captured_prompt),
		});
		router.register_model(ModelProfile {
			model_id: "capturing-model".to_string(),
			provider: "capturing-provider".to_string(),
			max_context_tokens: 16_000,
			cost_per_1k_tokens_usd: 0.0,
			max_risk_tier: RiskTier::Critical,
			route_priority: 100,
		});
		let general_tool = tool_config
			.tool_for_role(BuiltinToolRole::General)
			.expect("general tool should exist");
		let catalog = build_resource_catalog(&registry, &tool_config);
		let tool = PromptedLlmTool::from_config(general_tool, registry, Arc::new(router), catalog);

		tool.invoke(ToolInvocationRequest {
			invocation_key: "invoke-1".to_string(),
			input: json!({
				"task_id": "task-1",
				"node_id": "node-1",
				"goal": "你帮我创建一个写python的skill吧，创建完之后告诉我创建在了哪里。",
				"summary": "Use selected skill `skill-creator` for this request",
				"conversation_history": "",
				"granted_capabilities": [],
				"resource_selectors": ["skill:skill-creator"],
				"budget_tokens": 2048_u64,
				"time_budget_ms": 45_000_u64
			}),
			attempt: 1,
			sandbox_profile: SandboxProfile::NoIsolation,
			attachments: Vec::new(),
			allowed_read_roots: Vec::new(),
			allowed_write_roots: Vec::new(),
		})
		.expect("invoke should succeed");

		let prompt = captured_prompt
			.lock()
			.expect("prompt lock should succeed")
			.clone()
			.expect("prompt should be captured");
		assert!(prompt.contains("Execution authority"));
		assert!(prompt.contains("selected_resources: skill:skill-creator"));
		assert!(prompt.contains("side_effects_allowed: not allowed"));
		assert!(prompt.contains("you must clearly say it has not been created yet"));
	}

	#[test]
	fn skill_execute_tool_creates_and_registers_generated_skill() {
		let registry_root = tempfile::tempdir().expect("registry root should exist");
		let skill_root = registry_root.path().join("skills");
		let registry = SkillRegistry::file_backed(skill_root);
		let creator_dir = registry_root.path().join("skill-creator");
		fs::create_dir_all(creator_dir.join("scripts")).expect("creator scripts dir should exist");
		fs::write(
			creator_dir.join("SKILL.md"),
			"---\nname: skill-creator\ndescription: Create local skills.\n---\n\n# Skill Creator\n",
		)
		.expect("creator manifest should write");
		fs::write(
			creator_dir.join("scripts").join("quick_validate.py"),
			r#"import sys
from pathlib import Path
target = Path(sys.argv[1])
assert (target / "SKILL.md").exists()
print("ok")
"#,
		)
		.expect("validation script should write");
		registry
			.register_local_skill(&creator_dir, "test-suite")
			.expect("creator skill should register");
		let mut router = LlmRouter::new(RoutingPolicy {
			max_request_cost_usd: 1.0,
			max_latency_ms: 5_000,
		});
		router.register_provider(StaticOutputProvider {
			output: r#"{"skill_name":"python-skill","description":"Use this skill when the user needs help with python workflows.","overview":"This skill provides concise guidance and reusable workflow context for python tasks.","short_description":"Help with python workflows","default_prompt":"Use the python-skill skill for this task.","resources":[]}"#.to_string(),
		});
		router.register_model(ModelProfile {
			model_id: "static-output-model".to_string(),
			provider: "static-output-provider".to_string(),
			max_context_tokens: 16_000,
			cost_per_1k_tokens_usd: 0.0,
			max_risk_tier: RiskTier::Critical,
			route_priority: 100,
		});

		let generated_root = registry_root.path().join("generated");
		let record = registry
			.get_skill("skill-creator")
			.expect("creator record should exist");
		let input = super::ToolInput {
			task_id: "task-1",
			node_id: "node-1",
			goal: "Use skill-creator to create a Python skill and tell me where it was created.",
			summary: "Execute installed skill `skill-creator` using its local scripts",
			conversation_history: "",
			runtime_memory_sections: RuntimeMemorySections::default(),
			granted_capabilities: vec!["skill.execute".to_string()],
			resource_selectors: vec![
				"tool:skill.execute".to_string(),
				"skill:skill-creator".to_string(),
			],
			budget_tokens: 4096,
			time_budget_ms: 120_000,
		};
		let execution_request = roku_common_types::SkillExecutionRequest {
			selected_skill: "skill-creator".to_string(),
			goal: input.goal.to_string(),
			execution_mode: Some(roku_common_types::SkillExecutionMode::Executable),
			allowed_script_paths: allowed_script_paths(&record),
			allowed_output_root: Some(generated_root.display().to_string()),
			expected_artifacts: Vec::new(),
		};

		let result = execute_skill_creator(
			&registry,
			Some(&router),
			&record,
			&input,
			&generated_root,
			&execution_request,
			ToolWorkerRuntimeConfig::default().max_skill_execution_output_chars,
		)
		.expect("skill creator execution should succeed");

		let created_skill_dir = generated_root.join("python-skill");
		assert!(created_skill_dir.join("SKILL.md").exists());
		assert!(
			created_skill_dir
				.join("agents")
				.join("openai.yaml")
				.exists()
		);
		assert_eq!(result.generated_skill_name.as_deref(), Some("python-skill"));
		assert_eq!(result.validation_status.as_deref(), Some("passed"));
		assert!(
			registry.get_skill("python-skill").is_ok(),
			"generated skill should be registered immediately"
		);
	}

	fn test_skill_archive_bytes() -> Vec<u8> {
		test_skill_archive_bytes_for(
			"claude-api",
			"Build apps with the Claude API.",
			"Use this skill when the user explicitly asks for Claude API integration help.",
		)
	}

	fn test_skill_archive_bytes_for(skill_name: &str, description: &str, body: &str) -> Vec<u8> {
		let mut cursor = Cursor::new(Vec::new());
		{
			let mut writer = zip::ZipWriter::new(&mut cursor);
			let options = zip::write::SimpleFileOptions::default();
			writer
				.add_directory(format!("skills-main/skills/{skill_name}/"), options)
				.expect("dir should be added");
			writer
				.add_directory(format!("skills-main/skills/{skill_name}/shared/"), options)
				.expect("shared dir should be added");
			writer
				.start_file(format!("skills-main/skills/{skill_name}/SKILL.md"), options)
				.expect("skill file should start");
			writer
				.write_all(
					format!(
						"---\nname: {skill_name}\ndescription: {description}\n---\n\n# {skill_name}\n\n{body}\n"
					)
					.as_bytes(),
				)
				.expect("skill markdown should write");
			writer
				.start_file(
					format!("skills-main/skills/{skill_name}/shared/models.md"),
					options,
				)
				.expect("support file should start");
			writer
				.write_all(b"Use claude-opus-4-6 unless the user asks otherwise.")
				.expect("support file should write");
			writer.finish().expect("zip should finish");
		}
		cursor.into_inner()
	}

	fn general_completion_json(
		final_message: &str,
		completion_kind: &str,
		evidence_status: &str,
	) -> String {
		json!({
			"final_message": final_message,
			"completion_kind": completion_kind,
			"evidence_status": evidence_status,
			"missing_information": [],
		})
		.to_string()
	}

	#[test]
	fn all_baseline_tools_have_grounding_metadata() {
		use crate::builtin;

		let mut all_descriptors = Vec::new();
		all_descriptors.extend(builtin::fs::catalog_descriptors());
		all_descriptors.extend(builtin::table::catalog_descriptors());
		all_descriptors.extend(builtin::web::catalog_descriptors());
		all_descriptors.extend(builtin::command::catalog_descriptors());
		all_descriptors.extend(builtin::python::catalog_descriptors());

		let baseline_tools = [
			"fs.find",
			"fs.read_text",
			"fs.list_dir",
			"fs.inspect",
			"fs.exists",
			"fs.glob",
			"fs.edit",
			"fs.write",
			"fs.grep",
			"table.preview",
			"table.inspect",
			"table.list_sheets",
			"table.schema",
			"web.search",
			"web.fetch",
			"command.run",
			"python.run",
		];

		for tool_name in &baseline_tools {
			let descriptor = all_descriptors.iter().find(|d| d.name == *tool_name);
			assert!(
				descriptor.is_some(),
				"baseline tool {tool_name} should have a catalog descriptor"
			);
			let descriptor = descriptor.unwrap();
			assert!(
				descriptor.contract.is_some(),
				"baseline tool {tool_name} should have a contract"
			);
			let contract = descriptor.contract.as_ref().unwrap();
			assert_ne!(
				contract.grounding,
				roku_common_types::ToolGroundingContract::default(),
				"baseline tool {tool_name} should have non-default grounding metadata"
			);
		}
	}

	#[test]
	fn grounding_metadata_serde_roundtrip() {
		let contract = roku_common_types::ToolContract {
			grounding: roku_common_types::ToolGroundingContract {
				required_argument_keys: vec!["file_path".to_string()],
				grounding_strategy: roku_common_types::GroundingStrategy::PathBased,
				grounding_argument: Some("file_path".to_string()),
				requires_grounded_path: true,
				bootstrap_matchable: true,
				missing_argument_hint: None,
				extraction_hint: roku_common_types::ExtractionHint::Default,
				static_extra_arguments: serde_json::Map::new(),
			},
			..roku_common_types::ToolContract::default()
		};
		let json = serde_json::to_string(&contract).unwrap();
		let deserialized: roku_common_types::ToolContract = serde_json::from_str(&json).unwrap();
		assert_eq!(deserialized.grounding, contract.grounding);
	}

	#[test]
	fn grounding_metadata_absent_deserializes_to_default() {
		let json = r#"{"selection":{},"input":{},"output":{"observation_schema":"result.v1","success_semantics":"","empty_result_semantics":""},"runtime":{}}"#;
		let contract: roku_common_types::ToolContract = serde_json::from_str(json).unwrap();
		assert_eq!(
			contract.grounding,
			roku_common_types::ToolGroundingContract::default()
		);
	}

	#[test]
	fn grounding_argument_keys_match_expected_tool_parameters() {
		use crate::builtin;

		let mut all_descriptors = Vec::new();
		all_descriptors.extend(builtin::fs::catalog_descriptors());
		all_descriptors.extend(builtin::table::catalog_descriptors());
		all_descriptors.extend(builtin::web::catalog_descriptors());
		all_descriptors.extend(builtin::command::catalog_descriptors());
		all_descriptors.extend(builtin::python::catalog_descriptors());

		let expected: &[(&str, &str)] = &[
			("fs.find", "name"),
			("fs.read_text", "path"),
			("fs.list_dir", "path"),
			("fs.inspect", "path"),
			("fs.exists", "path"),
			("fs.glob", "pattern"),
			("fs.edit", "file_path"),
			("fs.write", "file_path"),
			("fs.grep", "pattern"),
			("table.preview", "path"),
			("table.inspect", "path"),
			("table.list_sheets", "path"),
			("table.schema", "path"),
			("web.search", "query"),
			("web.fetch", "url"),
			("command.run", "command"),
			("python.run", "code"),
		];

		for (tool_name, expected_primary_key) in expected {
			let descriptor = all_descriptors
				.iter()
				.find(|d| d.name == *tool_name)
				.unwrap_or_else(|| panic!("missing descriptor for {tool_name}"));
			let grounding = &descriptor
				.contract
				.as_ref()
				.unwrap_or_else(|| panic!("{tool_name} has no contract"))
				.grounding;
			assert!(
				grounding
					.required_argument_keys
					.contains(&expected_primary_key.to_string()),
				"{tool_name}: grounding keys {:?} should contain \"{expected_primary_key}\"",
				grounding.required_argument_keys
			);
			assert_eq!(
				grounding.grounding_argument.as_deref(),
				Some(*expected_primary_key),
				"{tool_name}: grounding_argument should be \"{expected_primary_key}\""
			);
		}
	}
}
