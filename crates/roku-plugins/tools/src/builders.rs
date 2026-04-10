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
use roku_common_types::{CatalogDescriptor, ResourceCatalog, ResourceKind};
use roku_common_types::{
	ResourceSelector, SkillExecutionMode, SkillExecutionPlan, SkillExecutionRequest,
	SkillExecutionResult, ToolContract, ToolOutputEnvelope,
};
use roku_plugin_host::PluginRegistrySnapshot;
use roku_plugin_host::{
	RuntimeConstraints, SandboxProfile, Tool, ToolDescriptor, ToolFailure, ToolInvocationRequest,
	ToolRuntime, ToolSchema,
};
use roku_plugin_llm::{GenerationRequest, LlmRouter, RiskTier};
use roku_plugin_skills::{InstalledSkillRecord, SkillRegistry};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{Value, json};

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
	roku_common_types::build_resource_catalog(entries, skill_entries)
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
			_ => {}
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
	_resource_catalog: &ResourceCatalog,
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
			_ => {}
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
		.generate_blocking(&GenerationRequest {
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
			tools: None,
		})
		.map_err(|e| ToolFailure::terminal(format!("LLM error: {e}")))?;
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
		.generate_blocking(&GenerationRequest {
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
			tools: None,
		})
		.map_err(|e| ToolFailure::terminal(format!("LLM error: {e}")))?;
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
	resource_selectors: Vec<String>,
	budget_tokens: u64,
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

#[cfg(test)]
mod tests {
	use std::fs;

	use super::{allowed_script_paths, build_resource_catalog, execute_skill_creator, first_url_in_text};
	use crate::config::{BuiltinToolRole, ToolCatalogConfig};
	use crate::runtime_config::ToolWorkerRuntimeConfig;
	use async_trait::async_trait;
	use roku_plugin_llm::{
		GenerationRequest, LlmProvider, LlmRouter, ModelProfile, ProviderCallError,
		ProviderResponse, RiskTier, RoutingPolicy,
	};
	use roku_plugin_skills::SkillRegistry;

	// general_worker_returns_structured_completion_contract removed:
	// general.execute is no longer registered.

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

	struct StaticOutputProvider {
		output: String,
	}

	#[async_trait]
	impl LlmProvider for StaticOutputProvider {
		fn provider_name(&self) -> &'static str {
			"static-output-provider"
		}

		async fn complete(
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
				tool_calls: None,
			})
		}
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
			resource_selectors: vec![
				"tool:skill.execute".to_string(),
				"skill:skill-creator".to_string(),
			],
			budget_tokens: 4096,
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
