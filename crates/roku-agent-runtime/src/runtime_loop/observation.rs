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

use roku_common_types::ToolOutputEnvelope;
use roku_plugin_host::{ToolExecutionResult, ToolRuntimeError};
use roku_plugin_tools::{ResourceCatalog, TAG_CATEGORY_FILESYSTEM, is_builtin_tool_name};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Grounded fact returned from a tool invocation or translated tool runtime error.
///
/// ## Why this exists
/// The runtime must preserve a strict boundary between tool truth and runtime interpretation.
/// `ToolObservation` stores the tool-facing facts that later loop logic may interpret.
///
/// ## Fields
/// - `ok`: Whether the tool invocation succeeded according to the tool contract.
/// - `tool_name`: Name of the tool that produced this observation.
/// - `error_type`: Runtime-normalized error type for failed observations, if any.
/// - `terminal`: Whether the tool contract explicitly says the loop should stop on this result.
/// - `data`: Structured tool payload retained for downstream interpretation and replay.
/// - `message`: User-facing or diagnostic summary supplied by the tool contract.
///
/// ## Invariants
/// - `terminal` is tool-contract truth, not a runtime-generated final answer.
/// - `data` remains structured; it is not replaced by a summarized prompt digest.
///
/// ## Non-Goals
/// - `ToolObservation` does not choose `final_answer`, `ask_user`, or `fail`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolObservation {
	pub ok: bool,
	pub tool_name: String,
	pub error_type: Option<String>,
	pub terminal: bool,
	pub data: Value,
	pub message: String,
}

/// Step-level observation snapshot stored inside `StepRecord`.
///
/// ## Why this exists
/// A loop step may record a tool observation or a synthetic terminal message. `StepObservation`
/// keeps those cases explicit without collapsing them into one lossy string.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StepObservation {
	Tool(ToolObservation),
	AskUser { final_message: String },
	FinalMessage { final_message: String },
}

impl ToolObservation {
	pub fn from_output_value(tool_name: &str, output: &Value) -> Self {
		if let Ok(envelope) = serde_json::from_value::<ToolOutputEnvelope>(output.clone()) {
			return Self {
				ok: envelope.ok,
				tool_name: tool_name.to_string(),
				error_type: envelope.error_type,
				terminal: envelope.terminal,
				data: envelope.data,
				message: envelope.message,
			};
		}
		if allows_migration_legacy_output_fallback(tool_name)
			&& let Some(observation) = Self::from_migration_legacy_output(tool_name, output)
		{
			return observation;
		}

		let message = output
			.get("message")
			.and_then(Value::as_str)
			.unwrap_or("tool invocation completed")
			.to_string();
		Self {
			ok: true,
			tool_name: tool_name.to_string(),
			error_type: None,
			terminal: false,
			data: output.clone(),
			message,
		}
	}

	/// Temporary migration-only compatibility for emitters that still return raw output instead of
	/// `ToolOutputEnvelope`. Keep the focused fallback inventory tests in this module aligned with
	/// the remaining emitter categories so later cleanup can remove these branches deliberately.
	fn from_migration_legacy_output(tool_name: &str, output: &Value) -> Option<Self> {
		let ok = output.get("ok").and_then(Value::as_bool)?;
		let error_type = output
			.get("error_type")
			.and_then(Value::as_str)
			.map(str::to_string);
		let terminal = output
			.get("terminal")
			.and_then(Value::as_bool)
			.unwrap_or(false);
		let data = output
			.get("data")
			.cloned()
			.unwrap_or_else(|| output.clone());
		let message = output
			.get("message")
			.and_then(Value::as_str)
			.unwrap_or("tool invocation completed")
			.to_string();
		Some(Self {
			ok,
			tool_name: tool_name.to_string(),
			error_type,
			terminal,
			data,
			message,
		})
	}

	pub fn from_result_payload(tool_name: &str, payload: &Value) -> Self {
		payload
			.get("output")
			.map(|output| Self::from_output_value(tool_name, output))
			.unwrap_or_else(|| Self::from_output_value(tool_name, payload))
	}

	pub fn from_error_payload(tool_name: &str, payload: &Value, catalog: &ResourceCatalog) -> Self {
		let message = payload
			.get("message")
			.and_then(Value::as_str)
			.unwrap_or("tool invocation failed")
			.to_string();
		let error_code = payload
			.get("error_code")
			.and_then(Value::as_str)
			.unwrap_or("execution_failed");
		let (error_type, terminal) = classify_tool_error(tool_name, error_code, &message, catalog);
		Self {
			ok: false,
			tool_name: tool_name.to_string(),
			error_type: Some(error_type),
			terminal,
			data: json!({
				"tool_name": tool_name,
				"error_code": error_code,
				"message": message,
			}),
			message,
		}
	}

	pub fn from_execution_result(execution: &ToolExecutionResult) -> Self {
		Self::from_output_value(&execution.tool_name, &execution.output)
	}

	pub fn from_runtime_error(
		tool_name: &str,
		error: &ToolRuntimeError,
		catalog: &ResourceCatalog,
	) -> Self {
		let message = error.to_string();
		let error_code = tool_error_code(error);
		let (error_type, terminal) = classify_tool_error(tool_name, error_code, &message, catalog);
		Self {
			ok: false,
			tool_name: tool_name.to_string(),
			error_type: Some(error_type),
			terminal,
			data: json!({
				"tool_name": tool_name,
				"error_code": error_code,
				"error": message,
			}),
			message,
		}
	}
}

fn allows_migration_legacy_output_fallback(tool_name: &str) -> bool {
	!is_builtin_tool_name(tool_name)
}

fn tool_error_code(error: &ToolRuntimeError) -> &'static str {
	match error {
		ToolRuntimeError::ToolNotFound(_) => "tool_not_found",
		ToolRuntimeError::ToolAlreadyRegistered(_) => "tool_already_registered",
		ToolRuntimeError::InvalidDescriptor(_) => "invalid_descriptor",
		ToolRuntimeError::InputSchemaViolation { .. } => "input_schema_violation",
		ToolRuntimeError::CapabilityDenied { .. } => "capability_denied",
		ToolRuntimeError::Timeout { .. } => "timeout",
		ToolRuntimeError::ExecutionFailed { retriable, .. } => {
			if *retriable {
				"retriable_execution_failed"
			} else {
				"execution_failed"
			}
		}
	}
}

fn classify_tool_error(
	tool_name: &str,
	error_code: &str,
	message: &str,
	catalog: &ResourceCatalog,
) -> (String, bool) {
	let is_fs_tool = catalog.tool_has_tag(tool_name, TAG_CATEGORY_FILESYSTEM);

	if is_fs_tool {
		let lower = message.to_ascii_lowercase();
		if lower.contains("outside the allowed read roots") {
			return ("workspace_violation".to_string(), true);
		}
		if lower.contains("ambiguous under the allowed read roots") {
			return ("multiple_candidates".to_string(), false);
		}
		if lower.contains("is not a directory") {
			return ("not_directory".to_string(), false);
		}
		if lower.contains("is a directory") {
			return ("not_file".to_string(), false);
		}
		if lower.contains("not found") || lower.contains("failed to resolve") {
			return ("path_not_found".to_string(), false);
		}
		if lower.contains("permission denied") {
			return ("permission_denied".to_string(), true);
		}
	}
	match error_code {
		"timeout" => ("tool_timeout".to_string(), true),
		"capability_denied" => ("permission_denied".to_string(), true),
		"input_schema_violation" => ("invalid_argument".to_string(), true),
		"tool_not_found" => ("tool_not_found".to_string(), true),
		other => (other.to_string(), false),
	}
}

#[cfg(test)]
mod tests {
	use std::env;
	use std::ffi::OsString;
	use std::fs;
	use std::io::{Cursor, Write};
	use std::path::Path;
	use std::sync::{Arc, LazyLock, Mutex};

	use async_trait::async_trait;
	use roku_common_types::{ExecutionResourceScope, ToolOutputEnvelope};
	use roku_plugin_host::{ToolExecutionResult, ToolInvocation, ToolRuntime};
	use roku_plugin_llm::{
		GenerationRequest, LlmProvider, LlmRouter, ModelProfile, ProviderCallError,
		ProviderResponse, RiskTier, RoutingPolicy,
	};
	use roku_plugin_skills::{SkillRegistry, SkillRegistryError};
	use roku_plugin_tools::{
		ToolCatalogConfig, build_builtin_tool_runtime, build_llm_tool_runtime,
		build_resource_catalog, build_runtime_visible_tool_availability_snapshot,
		builtin_tool_names,
	};
	use serde_json::{Value, json};
	use zip::CompressionMethod;
	use zip::write::{FileOptions, ZipWriter};

	use super::ToolObservation;

	static SKILL_ROOT_ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

	struct ScopedSkillRoot {
		previous: Option<OsString>,
	}

	impl ScopedSkillRoot {
		fn set(path: &Path) -> Self {
			let previous = env::var_os("ROKU_SKILL_ROOT");
			// SAFETY: tests serialize mutations to the process environment with
			// `SKILL_ROOT_ENV_LOCK`, so no concurrent readers or writers in this
			// module observe a partially updated `ROKU_SKILL_ROOT`.
			unsafe { env::set_var("ROKU_SKILL_ROOT", path) };
			Self { previous }
		}
	}

	impl Drop for ScopedSkillRoot {
		fn drop(&mut self) {
			if let Some(previous) = self.previous.take() {
				// SAFETY: see `ScopedSkillRoot::set`; the same module-level lock is
				// held for the full lifetime of this guard.
				unsafe { env::set_var("ROKU_SKILL_ROOT", previous) };
			} else {
				// SAFETY: see `ScopedSkillRoot::set`; the same module-level lock is
				// held for the full lifetime of this guard.
				unsafe { env::remove_var("ROKU_SKILL_ROOT") };
			}
		}
	}

	struct StaticJsonProvider {
		output: String,
	}

	#[async_trait]
	impl LlmProvider for StaticJsonProvider {
		fn provider_name(&self) -> &'static str {
			"static-json-provider"
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
				output_tokens: 24,
				cache_creation_input_tokens: 0,
				cache_read_input_tokens: 0,
				latency_ms: 10,
				tool_calls: None,
				response_id: None,
			})
		}
	}

	fn output_contract_kind(output: &Value) -> &'static str {
		if serde_json::from_value::<ToolOutputEnvelope>(output.clone()).is_ok() {
			return "envelope";
		}
		if ToolObservation::from_migration_legacy_output("test-emitter", output).is_some() {
			return "legacy_raw";
		}
		"generic_raw"
	}

	fn invoke_tool(
		runtime: &ToolRuntime,
		tool_name: &str,
		input: Value,
		granted_capabilities: Vec<String>,
		approved_scope: Option<ExecutionResourceScope>,
	) -> ToolExecutionResult {
		runtime
			.invoke(ToolInvocation {
				tool_name: tool_name.to_string(),
				input,
				canonical_execution: None,
				approved_scope,
				skip_policy_check: false,
				granted_capabilities,
				invocation_key: Some(format!("invoke:{tool_name}")),
				attachments: Vec::new(),
			})
			.expect("tool invocation should succeed")
	}

	fn skill_execution_runtime() -> (ToolRuntime, tempfile::TempDir) {
		let root = tempfile::tempdir().expect("skill root should exist");
		let registry = SkillRegistry::file_backed(root.path().join("skills"));
		let skill_dir = root.path().join("demo-skill");
		fs::create_dir_all(skill_dir.join("scripts")).expect("skill scripts dir should exist");
		fs::write(
			skill_dir.join("SKILL.md"),
			"---\nname: demo-skill\ndescription: Execute a local demo script.\n---\n\n# Demo Skill\n",
		)
		.expect("skill manifest should write");
		fs::write(
			skill_dir.join("scripts").join("run.sh"),
			"#!/usr/bin/env bash\nprintf 'demo-skill-ran'\n",
		)
		.expect("skill script should write");
		registry
			.register_local_skill(&skill_dir, "test-suite")
			.expect("local skill should register");

		let mut router = LlmRouter::new(RoutingPolicy {
			max_request_cost_usd: 1.0,
			max_latency_ms: 5_000,
		});
		router.register_provider(StaticJsonProvider {
			output: r#"{"selected_skill":"demo-skill","execution_mode":"executable","script_relpath":"scripts/run.sh","script_args":[],"expected_artifacts":[]}"#.to_string(),
		});
		router.register_model(ModelProfile {
			model_id: "static-skill-planner".to_string(),
			provider: "static-json-provider".to_string(),
			max_context_tokens: 16_000,
			cost_per_1k_tokens_usd: 0.0,
			max_risk_tier: RiskTier::Critical,
			route_priority: 100,
		});

		let tool_config = ToolCatalogConfig::default();
		let catalog = build_resource_catalog(&registry, &tool_config);
		(
			build_llm_tool_runtime(Arc::new(router), registry, &tool_config, &catalog),
			root,
		)
	}

	#[test]
	fn builtin_tool_authority_disables_legacy_output_fallback_for_all_builtin_names() {
		let tool_config = ToolCatalogConfig::default();
		let catalog = build_resource_catalog(&SkillRegistry::disabled(), &tool_config);
		let snapshot =
			build_runtime_visible_tool_availability_snapshot(&catalog, &["inventory.describe"]);

		for tool_name in builtin_tool_names() {
			assert!(
				!super::allows_migration_legacy_output_fallback(tool_name),
				"builtin tool `{tool_name}` should not use migration raw fallback"
			);
		}
		for tool_name in &snapshot.enabled_tools {
			assert!(
				builtin_tool_names().contains(tool_name),
				"runtime-visible tool `{tool_name}` should be part of the builtin authority set"
			);
			assert!(
				!super::allows_migration_legacy_output_fallback(tool_name),
				"runtime-visible builtin `{tool_name}` should not use migration raw fallback"
			);
		}
		assert!(super::allows_migration_legacy_output_fallback(
			"custom.legacy"
		));
		assert!(super::allows_migration_legacy_output_fallback("custom.raw"));
	}

	#[test]
	fn fs_exists_observation_path_uses_envelope_without_legacy_fallback() {
		assert!(!super::allows_migration_legacy_output_fallback("Exists"));
		assert!(super::allows_migration_legacy_output_fallback(
			"custom.legacy"
		));

		let fs_root = tempfile::tempdir().expect("fs root should exist");
		let note_path = fs_root.path().join("note.txt");
		fs::write(&note_path, "hello").expect("fs fixture should write");
		let runtime =
			build_builtin_tool_runtime(SkillRegistry::disabled(), &ToolCatalogConfig::default());
		let output = invoke_tool(
			&runtime,
			"Exists",
			json!({
				"task_id": "task-fs-exists",
				"node_id": "node-fs-exists",
				"goal": "Check whether note.txt exists.",
				"summary": "Resolve the grounded file path and report whether it exists.",
				"conversation_history": "",
				"budget_tokens": 2048_u64,
				"time_budget_ms": 45_000_u64,
				"path": "note.txt"
			}),
			vec!["Exists".to_string()],
			Some(ExecutionResourceScope {
				working_directory: fs_root.path().display().to_string(),
				resolved_targets: vec![fs_root.path().join("note.txt").display().to_string()],
				effective_read_roots: vec![fs_root.path().display().to_string()],
				effective_write_roots: Vec::new(),
			}),
		)
		.output;

		assert_eq!(output_contract_kind(&output), "envelope");

		let observation = ToolObservation::from_output_value("Exists", &output);
		assert!(observation.ok);
		assert_eq!(observation.error_type, None);
		assert!(!observation.terminal);
		let observed_path = observation.data["path"]
			.as_str()
			.expect("Exists data.path should be a string");
		assert!(observed_path.ends_with("/note.txt"));
		assert_eq!(
			observation.message,
			format!("`{observed_path}` exists as file.")
		);
		assert_eq!(observation.data["exists"], true);
		assert_eq!(observation.data["kind"], "file");
	}

	#[test]
	fn skill_execute_observation_path_uses_envelope_without_legacy_fallback() {
		assert!(!super::allows_migration_legacy_output_fallback("SkillRun"));
		assert!(super::allows_migration_legacy_output_fallback(
			"custom.legacy"
		));

		let _skill_root_lock = SKILL_ROOT_ENV_LOCK
			.lock()
			.expect("skill root lock should succeed");
		let (skill_runtime, skill_root) = skill_execution_runtime();
		let _skill_root = ScopedSkillRoot::set(skill_root.path().join("generated").as_path());
		let output = invoke_tool(
			&skill_runtime,
			"SkillRun",
			json!({
				"task_id": "task-skill-execute",
				"node_id": "node-skill-execute",
				"goal": "Run the demo skill now.",
				"summary": "Execute the installed demo skill script.",
				"conversation_history": "",
				"granted_capabilities": ["SkillRun"],
				"resource_selectors": ["skill:demo-skill"],
				"budget_tokens": 4096_u64,
				"time_budget_ms": 120_000_u64
			}),
			vec!["SkillRun".to_string()],
			None,
		)
		.output;

		assert_eq!(output_contract_kind(&output), "envelope");

		let observation = ToolObservation::from_output_value("SkillRun", &output);
		assert!(observation.ok);
		assert_eq!(observation.error_type, None);
		assert!(observation.terminal);
		assert_eq!(observation.data["worker_id"], "skill-execute-worker");
		assert_eq!(observation.data["selected_skill"], "demo-skill");
		assert_eq!(observation.data["execution_mode"], "executable");
		assert_eq!(observation.data["success"], true);
		assert_eq!(observation.data["validation_status"], "not_requested");
		assert!(
			observation
				.message
				.contains("Executed skill `demo-skill` via `scripts/run.sh` successfully.")
		);
	}

	#[test]
	fn legacy_and_generic_raw_fallbacks_still_normalize_into_tool_observations() {
		let legacy_output = json!({
			"ok": false,
			"error_type": "custom_error",
			"data": { "source": "legacy" }
		});
		let legacy = ToolObservation::from_output_value("custom.legacy", &legacy_output);
		assert!(!legacy.ok);
		assert_eq!(legacy.error_type.as_deref(), Some("custom_error"));
		assert_eq!(legacy.message, "tool invocation completed");
		assert_eq!(legacy.data["source"], "legacy");

		let generic_output = json!({
			"status": "ok",
			"message": "generic raw output"
		});
		let generic = ToolObservation::from_output_value("custom.raw", &generic_output);
		assert!(generic.ok);
		assert_eq!(generic.error_type, None);
		assert_eq!(generic.message, "generic raw output");
		assert_eq!(generic.data["status"], "ok");
	}

	fn zip_skill_archive(skill_name: &str) -> Vec<u8> {
		let mut cursor = Cursor::new(Vec::new());
		let mut writer = ZipWriter::new(&mut cursor);
		let options: FileOptions<'_, ()> =
			FileOptions::default().compression_method(CompressionMethod::Stored);
		writer
			.start_file(format!("skills-main/skills/{skill_name}/SKILL.md"), options)
			.expect("archive entry should open");
		write!(
			writer,
			"---\nname: {skill_name}\ndescription: Demo archived skill.\n---\n\n# Demo Skill\n"
		)
		.expect("archive entry should write");
		writer.finish().expect("archive should finish");
		cursor.into_inner()
	}

	#[test]
	fn skill_install_output_uses_the_shared_envelope_contract() {
		#[derive(Clone)]
		struct StaticArchiveFetcher {
			bytes: Vec<u8>,
		}

		impl roku_plugin_skills::SkillArchiveFetcher for StaticArchiveFetcher {
			fn fetch(
				&self,
				_source: &roku_plugin_skills::SkillSource,
			) -> Result<roku_plugin_skills::DownloadedArchive, SkillRegistryError> {
				Ok(roku_plugin_skills::DownloadedArchive {
					archive_url: "https://example.com/demo-skill.zip".to_string(),
					bytes: self.bytes.clone(),
					resolved_reference: Some("main".to_string()),
				})
			}
		}

		let root = tempfile::tempdir().expect("skill install root should exist");
		let registry = SkillRegistry::file_backed(root.path().join("skills")).with_fetcher(
			Arc::new(StaticArchiveFetcher {
				bytes: zip_skill_archive("archived-demo-skill"),
			}),
		);
		let runtime = build_builtin_tool_runtime(registry, &ToolCatalogConfig::default());
		let output = invoke_tool(
			&runtime,
			"SkillInstall",
			json!({
				"task_id": "task-3",
				"node_id": "node-3",
				"goal": "Install the archived demo skill from its source URL.",
				"summary": "Install the skill so the runtime can confirm its local availability.",
				"conversation_history": "",
				"budget_tokens": 2048_u64,
				"time_budget_ms": 120_000_u64,
				"source_url": "https://github.com/example/skills/tree/main/skills/archived-demo-skill"
			}),
			vec!["SkillInstall".to_string()],
			None,
		)
		.output;

		assert_eq!(output_contract_kind(&output), "envelope");
		assert_eq!(
			ToolObservation::from_output_value("SkillInstall", &output).data["skill_name"],
			"archived-demo-skill"
		);
	}
}
