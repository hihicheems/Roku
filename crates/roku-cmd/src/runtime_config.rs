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

//! Startup-side runtime configuration loading and composition.
//!
//! This module is intentionally narrow: it reads `runtime.toml`, applies env overrides, and hands
//! typed config bundles back to the crates that actually enforce runtime semantics.
//! For memory specifically, startup only performs parsing/materialization during the migration;
//! provider-neutral ownership belongs to `roku-memory`.

use std::fs;
use std::path::PathBuf;

use roku_agent_runtime::{
	AgentRuntimeConfig, AgentRuntimeConfigPatch, ToolsRuntimeConfig, ToolsRuntimeConfigPatch,
};
use roku_common_types::{LogLevel, LogRecord, emit_global_log};
use roku_memory::MemoryBackendId;
use roku_plugin_llm::{
	AnthropicRuntimeConfig, AnthropicRuntimeConfigPatch, LlmProviderKind, OpenAiRuntimeConfig,
	OpenAiRuntimeConfigPatch, OpenRouterRuntimeConfig, OpenRouterRuntimeConfigPatch,
};
use roku_plugin_skills::{SkillsRuntimeConfig, SkillsRuntimeConfigPatch};
use roku_plugin_telegram::{TelegramRuntimeConfig, TelegramRuntimeConfigPatch};
use serde::Deserialize;

use crate::memory_runtime_config::{MemoryRuntimeConfig, MemoryRuntimeConfigPatch};
use crate::{CommandError, storage::LocalStorageLayout};

/// Effective runtime configuration bundle for startup-owned runtime defaults.
///
/// The startup layer owns file parsing and composition. Each runtime-owning
/// crate owns its typed config, patch application, and validate/clamp logic.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RuntimeConfigs {
	pub agent: AgentRuntimeConfig,
	pub tools: ToolsRuntimeConfig,
	pub llm_provider: LlmProviderKind,
	/// Whether `provider` was explicitly set in runtime.toml (vs. defaulting).
	/// When true, auth.json's `active_provider` should NOT override it.
	pub llm_provider_explicit: bool,
	/// OAuth client_id for the OpenAI PKCE flow (from `[runtime.llm]` config).
	pub oauth_client_id: Option<String>,
	pub openrouter: OpenRouterRuntimeConfig,
	pub anthropic: AnthropicRuntimeConfig,
	pub openai: OpenAiRuntimeConfig,
	pub telegram: TelegramRuntimeConfig,
	pub skills: SkillsRuntimeConfig,
	pub memory: MemoryRuntimeConfig,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeTomlConfig {
	#[serde(default)]
	runtime: RuntimeSections,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeSections {
	#[serde(default)]
	agent: AgentRuntimeConfigPatch,
	#[serde(default)]
	tools: ToolsRuntimeConfigPatch,
	#[serde(default)]
	llm: LlmSections,
	#[serde(default)]
	telegram: TelegramRuntimeConfigPatch,
	#[serde(default)]
	skills: SkillsRuntimeConfigPatch,
	#[serde(default)]
	memory: MemoryRuntimeConfigPatch,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct LlmSections {
	/// Which provider backs the live runtime. Defaults to OpenRouter when
	/// unset so existing deployments keep working without config changes.
	#[serde(default)]
	provider: Option<LlmProviderKind>,
	/// OAuth client_id for the OpenAI PKCE flow. Required for `roku chat`
	/// first-run OAuth setup when no API key is configured.
	#[serde(default)]
	oauth_client_id: Option<String>,
	#[serde(default)]
	openrouter: OpenRouterRuntimeConfigPatch,
	#[serde(default)]
	anthropic: AnthropicRuntimeConfigPatch,
	#[serde(default)]
	openai: OpenAiRuntimeConfigPatch,
}

/// Loads the effective runtime config bundle from disk and env for this process.
///
/// Parsing lives here so startup surfaces share one composition path. Validation and clamping stay
/// in the owning runtime crates, which keeps this layer from re-implementing per-subsystem policy.
pub(crate) fn load_runtime_configs(
	layout: &LocalStorageLayout,
) -> Result<RuntimeConfigs, CommandError> {
	let parsed = if layout.runtime_config_path.exists() {
		let content = fs::read_to_string(&layout.runtime_config_path).map_err(CommandError::Io)?;
		toml::from_str::<RuntimeTomlConfig>(&content).map_err(|error| {
			CommandError::RuntimeConfigBootstrap(format!(
				"failed to parse {}: {error}",
				layout.runtime_config_path.display()
			))
		})?
	} else {
		RuntimeTomlConfig::default()
	};

	let mut agent = AgentRuntimeConfig::default();
	agent.apply_patch(parsed.runtime.agent);
	agent.apply_env_overrides().map_err(|error| {
		CommandError::RuntimeConfigBootstrap(format!(
			"failed to load runtime.agent config: {error}"
		))
	})?;
	agent.validate_and_clamp().map_err(|error| {
		CommandError::RuntimeConfigBootstrap(format!(
			"failed to validate runtime.agent config: {error}"
		))
	})?;

	let mut tools = ToolsRuntimeConfig::default();
	tools.apply_patch(parsed.runtime.tools);
	tools.apply_env_overrides().map_err(|error| {
		CommandError::RuntimeConfigBootstrap(format!(
			"failed to load runtime.tools config: {error}"
		))
	})?;
	tools.validate_and_clamp().map_err(|error| {
		CommandError::RuntimeConfigBootstrap(format!(
			"failed to validate runtime.tools config: {error}"
		))
	})?;

	let llm_provider_explicit = parsed.runtime.llm.provider.is_some();
	let llm_provider = parsed.runtime.llm.provider.unwrap_or_default();

	let mut openrouter = OpenRouterRuntimeConfig::default();
	openrouter.apply_patch(parsed.runtime.llm.openrouter);
	openrouter
		.apply_env_overrides()
		.map_err(CommandError::from)?;
	openrouter
		.validate_and_clamp()
		.map_err(CommandError::from)?;

	let mut anthropic = AnthropicRuntimeConfig::default();
	anthropic.apply_patch(parsed.runtime.llm.anthropic);
	anthropic.apply_env_overrides().map_err(|error| {
		CommandError::RuntimeConfigBootstrap(format!(
			"failed to load runtime.llm.anthropic config: {error}"
		))
	})?;
	anthropic.validate_and_clamp().map_err(|error| {
		CommandError::RuntimeConfigBootstrap(format!(
			"failed to validate runtime.llm.anthropic config: {error}"
		))
	})?;

	let mut openai = OpenAiRuntimeConfig::default();
	openai.apply_patch(parsed.runtime.llm.openai);
	openai.apply_env_overrides().map_err(|error| {
		CommandError::RuntimeConfigBootstrap(format!(
			"failed to load runtime.llm.openai config: {error}"
		))
	})?;
	openai.validate_and_clamp().map_err(|error| {
		CommandError::RuntimeConfigBootstrap(format!(
			"failed to validate runtime.llm.openai config: {error}"
		))
	})?;

	let mut telegram = TelegramRuntimeConfig::default();
	telegram.apply_patch(parsed.runtime.telegram);
	telegram.apply_env_overrides().map_err(CommandError::from)?;
	telegram.validate_and_clamp().map_err(CommandError::from)?;

	let mut skills = SkillsRuntimeConfig::default();
	skills.apply_patch(parsed.runtime.skills);
	skills.apply_env_overrides().map_err(CommandError::from)?;
	skills.validate_and_clamp().map_err(CommandError::from)?;

	let mut memory = MemoryRuntimeConfig::default();
	memory.apply_patch(parsed.runtime.memory);
	memory.apply_env_overrides().map_err(|error| {
		CommandError::RuntimeConfigBootstrap(format!(
			"failed to load runtime.memory config: {error}"
		))
	})?;
	memory.validate_and_clamp().map_err(|error| {
		CommandError::RuntimeConfigBootstrap(format!(
			"failed to validate runtime.memory config: {error}"
		))
	})?;

	Ok(RuntimeConfigs {
		agent,
		tools,
		llm_provider,
		llm_provider_explicit,
		oauth_client_id: parsed.runtime.llm.oauth_client_id,
		openrouter,
		anthropic,
		openai,
		telegram,
		skills,
		memory,
	})
}

pub(crate) fn prepare_runtime_generated_artifacts(
	configs: &RuntimeConfigs,
) -> Result<Option<PathBuf>, CommandError> {
	let generated = configs
		.memory
		.backends
		.openviking
		.materialize_generated_config(
			configs.memory.enabled && matches!(configs.memory.backend, MemoryBackendId::OpenViking),
		)
		.map_err(|error| {
			CommandError::RuntimeConfigBootstrap(format!(
				"failed to prepare runtime.memory generated artifacts: {error}"
			))
		})?;
	if let Some(path) = generated.as_ref() {
		let _ = emit_global_log(
			LogRecord::new(
				"roku-cmd",
				LogLevel::Info,
				"generated managed OpenViking config from typed runtime.memory settings",
			)
			.with_field("path", path.display().to_string()),
		);
	}
	Ok(generated)
}

#[cfg(test)]
mod tests {
	use std::fs;
	use std::path::PathBuf;

	use roku_memory::{HARD_MAX_MEMORY_RECALL_TOP_K, HARD_MAX_MEMORY_WRITE_BATCH_SIZE};

	use super::*;
	use crate::memory_runtime_config::{
		HARD_MAX_MEMORY_REQUEST_TIMEOUT_MS, HARD_MAX_OPENVIKING_EMBED_MAX_CONCURRENT,
		HARD_MAX_OPENVIKING_VLM_MAX_CONCURRENT,
	};
	use crate::test_support::ENV_MUTEX;

	const HARD_MAX_READ_BYTES: usize = 256 * 1024;
	const HARD_MAX_DIR_ENTRIES: usize = 2_000;
	const HARD_MAX_GLOB_MATCHES: usize = 2_000;
	const HARD_MAX_DESCENDANT_SCAN_ENTRIES: usize = 50_000;
	const HARD_MAX_WEB_TOP_K: usize = 20;
	const HARD_MAX_LLM_TOOL_TIMEOUT_MS: u64 = 180_000;

	struct EnvGuard {
		key: &'static str,
		original: Option<String>,
	}

	impl EnvGuard {
		fn set(key: &'static str, value: &str) -> Self {
			let original = std::env::var(key).ok();
			unsafe {
				std::env::set_var(key, value);
			}
			Self { key, original }
		}

		fn remove(key: &'static str) -> Self {
			let original = std::env::var(key).ok();
			unsafe {
				std::env::remove_var(key);
			}
			Self { key, original }
		}
	}

	impl Drop for EnvGuard {
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

	fn temp_layout() -> LocalStorageLayout {
		let tempdir = tempfile::tempdir().expect("tempdir should exist");
		let root = tempdir.keep();
		let config_dir = root.join("config");
		fs::create_dir_all(&config_dir).expect("config dir should be created");
		LocalStorageLayout {
			home_dir: root.join(".roku"),
			state_dir: root.join(".roku/state"),
			legacy_sqlite_compat_path: root.join(".roku/state/control-plane.db"),
			artifact_root: root.join(".roku/artifacts"),
			experiment_root: root.join(".roku/experiments"),
			report_root: root.join(".roku/reports"),
			skill_root: root.join(".roku/skills"),
			generated_skill_root: root.join(".roku/skills"),
			tool_config_path: config_dir.join("tools.toml"),
			runtime_config_path: config_dir.join("runtime.toml"),
			plugin_config_path: config_dir.join("plugins.toml"),
			workspace_plugin_root: root.join(".roku/plugins"),
			user_plugin_root: root.join(".roku/user-plugins"),
			prompt_archive_dir: root.join(".roku/prompts"),
			memory_summary_dir: root.join(".roku/memory"),
			audit_export_dir: root.join(".roku/exports/audit"),
			log_dir: root.join(".roku/logs"),
			run_dir: root.join(".roku/run"),
			cache_dir: root.join(".roku/cache"),
			session_history_dir: root.join(".roku/state/sessions/chat"),
			traces_dir: root.join(".roku/traces"),
		}
	}

	fn write_runtime_toml(layout: &LocalStorageLayout, content: &str) {
		let parent = layout
			.runtime_config_path
			.parent()
			.expect("runtime config path should have parent");
		fs::create_dir_all(parent).expect("runtime config dir should exist");
		fs::write(&layout.runtime_config_path, content).expect("runtime toml should be written");
	}

	#[test]
	fn default_runtime_config_uses_typed_defaults() {
		let _env_lock = ENV_MUTEX.lock().expect("env mutex should lock");
		let layout = temp_layout();

		let configs = load_runtime_configs(&layout).expect("defaults should load");

		assert_eq!(configs.agent.r#loop.initial_step_budget, 30);
		assert_eq!(configs.agent.router.budget_tokens_remaining, 10_000);
		assert_eq!(configs.tools.fs.default_max_bytes, 65_536);
		assert_eq!(configs.openrouter.max_latency_ms, 60_000);
		assert_eq!(configs.telegram.poll_timeout_seconds, 30);
		assert_eq!(configs.skills.max_prompt_documents, 24);
		assert!(configs.memory.enabled);
		assert_eq!(configs.memory.backend, MemoryBackendId::Sqlite);
		assert_eq!(configs.memory.recall.top_k, 8);
		assert_eq!(configs.memory.write.max_batch_size, 16);
		assert_eq!(
			configs
				.memory
				.backends
				.openviking
				.process
				.config_output_path,
			PathBuf::from(".roku")
				.join("run")
				.join("openviking")
				.join("ov.conf")
		);
		assert_eq!(
			configs.memory.backends.sqlite.path,
			PathBuf::from(".roku")
				.join("state")
				.join("control-plane.db")
		);
	}

	#[test]
	fn runtime_toml_overrides_and_clamps_values() {
		let _env_lock = ENV_MUTEX.lock().expect("env mutex should lock");
		let layout = temp_layout();
		write_runtime_toml(
			&layout,
			r#"
[runtime.tools.fs]
default_max_bytes = 999999
max_dir_entries = 999999
max_glob_matches = 999999
max_descendant_scan_entries = 999999

[runtime.agent.router]
budget_tokens_remaining = 999999

[runtime.agent.prompts]
visible_tool_hint_max_chars = 999999

[runtime.tools.web]
default_top_k = 999999

[runtime.tools.workers]
llm_tool_timeout_ms = 999999

[runtime.llm.openrouter]
max_latency_ms = 999999

[runtime.telegram]
poll_timeout_seconds = 999

[runtime.skills]
max_prompt_documents = 999

[runtime.memory]
enabled = true

[runtime.memory.recall]
top_k = 999

[runtime.memory.write]
max_batch_size = 999

[runtime.memory.backends.openviking.client]
request_timeout_ms = 999999

[runtime.memory.backends.openviking.process.embedding]
max_concurrent = 999

[runtime.memory.backends.openviking.process.vlm]
max_concurrent = 999
"#,
		);

		let configs = load_runtime_configs(&layout).expect("runtime toml should load");

		assert_eq!(
			configs.agent.router.budget_tokens_remaining,
			roku_agent_runtime::HARD_MAX_ROUTE_BUDGET_TOKENS_REMAINING
		);
		assert_eq!(
			configs.agent.prompts.visible_tool_hint_max_chars,
			roku_agent_runtime::HARD_MAX_VISIBLE_TOOL_HINT_MAX_CHARS
		);
		assert_eq!(configs.tools.fs.default_max_bytes, HARD_MAX_READ_BYTES);
		assert_eq!(configs.tools.fs.max_dir_entries, HARD_MAX_DIR_ENTRIES);
		assert_eq!(configs.tools.fs.max_glob_matches, HARD_MAX_GLOB_MATCHES);
		assert_eq!(
			configs.tools.fs.max_descendant_scan_entries,
			HARD_MAX_DESCENDANT_SCAN_ENTRIES
		);
		assert_eq!(configs.tools.web.default_top_k, HARD_MAX_WEB_TOP_K);
		assert_eq!(
			configs.tools.workers.llm_tool_timeout_ms,
			HARD_MAX_LLM_TOOL_TIMEOUT_MS
		);
		assert_eq!(configs.openrouter.max_latency_ms, 300_000);
		assert_eq!(configs.telegram.poll_timeout_seconds, 300);
		assert_eq!(configs.skills.max_prompt_documents, 128);
		assert!(configs.memory.enabled);
		assert_eq!(configs.memory.recall.top_k, HARD_MAX_MEMORY_RECALL_TOP_K);
		assert_eq!(
			configs.memory.write.max_batch_size,
			HARD_MAX_MEMORY_WRITE_BATCH_SIZE
		);
		assert_eq!(
			configs.memory.backends.openviking.client.request_timeout_ms,
			HARD_MAX_MEMORY_REQUEST_TIMEOUT_MS
		);
		assert_eq!(
			configs
				.memory
				.backends
				.openviking
				.process
				.embedding
				.max_concurrent,
			HARD_MAX_OPENVIKING_EMBED_MAX_CONCURRENT
		);
		assert_eq!(
			configs
				.memory
				.backends
				.openviking
				.process
				.vlm
				.max_concurrent,
			HARD_MAX_OPENVIKING_VLM_MAX_CONCURRENT
		);
	}

	#[test]
	fn runtime_toml_rejects_unknown_and_secret_like_fields() {
		let _env_lock = ENV_MUTEX.lock().expect("env mutex should lock");
		let layout = temp_layout();
		write_runtime_toml(
			&layout,
			r#"
[runtime.llm.openrouter]
api_key = "should-not-be-configurable"
"#,
		);

		let error = load_runtime_configs(&layout).expect_err("unknown field should fail bootstrap");

		assert!(matches!(error, CommandError::RuntimeConfigBootstrap(_)));
		assert!(error.to_string().contains("unknown field `api_key`"));
	}

	#[test]
	fn default_llm_provider_is_openrouter() {
		let _env_lock = ENV_MUTEX.lock().expect("env mutex should lock");
		let layout = temp_layout();

		let configs = load_runtime_configs(&layout).expect("defaults should load");

		assert_eq!(configs.llm_provider, LlmProviderKind::Openrouter);
	}

	#[test]
	fn runtime_toml_selects_anthropic_provider_and_parses_subsection() {
		let _env_lock = ENV_MUTEX.lock().expect("env mutex should lock");
		let _clear_primary = EnvGuard::remove("ROKU_ANTHROPIC_PRIMARY_MODEL");
		let _clear_base_url = EnvGuard::remove("ROKU_ANTHROPIC_BASE_URL");
		let _clear_max_tokens = EnvGuard::remove("ROKU_ANTHROPIC_MAX_TOKENS");
		let layout = temp_layout();
		write_runtime_toml(
			&layout,
			r#"
[runtime.llm]
provider = "anthropic"

[runtime.llm.anthropic]
primary_model = "claude-opus-4-20250805"
fallback_models = ["claude-sonnet-4-5-20250929"]
max_tokens = 4096
"#,
		);

		let configs = load_runtime_configs(&layout).expect("anthropic config should load");

		assert_eq!(configs.llm_provider, LlmProviderKind::Anthropic);
		assert_eq!(configs.anthropic.primary_model, "claude-opus-4-20250805");
		assert_eq!(
			configs.anthropic.fallback_models,
			vec!["claude-sonnet-4-5-20250929".to_string()]
		);
		assert_eq!(configs.anthropic.max_tokens, 4096);
	}

	#[test]
	fn runtime_toml_selects_openai_provider_and_parses_subsection() {
		let _env_lock = ENV_MUTEX.lock().expect("env mutex should lock");
		let _clear_primary = EnvGuard::remove("ROKU_OPENAI_PRIMARY_MODEL");
		let _clear_base_url = EnvGuard::remove("ROKU_OPENAI_BASE_URL");
		let _clear_max_tokens = EnvGuard::remove("ROKU_OPENAI_MAX_TOKENS");
		let layout = temp_layout();
		write_runtime_toml(
			&layout,
			r#"
[runtime.llm]
provider = "openai"

[runtime.llm.openai]
primary_model = "gpt-4o-2024-11-20"
fallback_models = ["gpt-4o-mini"]
max_tokens = 2048
"#,
		);

		let configs = load_runtime_configs(&layout).expect("openai config should load");

		assert_eq!(configs.llm_provider, LlmProviderKind::Openai);
		assert_eq!(configs.openai.primary_model, "gpt-4o-2024-11-20");
		assert_eq!(
			configs.openai.fallback_models,
			vec!["gpt-4o-mini".to_string()]
		);
		assert_eq!(configs.openai.max_tokens, 2048);
	}

	#[test]
	fn runtime_toml_rejects_api_key_in_anthropic_section() {
		let _env_lock = ENV_MUTEX.lock().expect("env mutex should lock");
		let layout = temp_layout();
		write_runtime_toml(
			&layout,
			r#"
[runtime.llm.anthropic]
api_key = "should-not-be-configurable"
"#,
		);

		let error =
			load_runtime_configs(&layout).expect_err("api_key in runtime.toml must be rejected");

		assert!(matches!(error, CommandError::RuntimeConfigBootstrap(_)));
		assert!(error.to_string().contains("unknown field `api_key`"));
	}

	#[test]
	fn runtime_toml_rejects_api_key_in_openai_section() {
		let _env_lock = ENV_MUTEX.lock().expect("env mutex should lock");
		let layout = temp_layout();
		write_runtime_toml(
			&layout,
			r#"
[runtime.llm.openai]
api_key = "should-not-be-configurable"
"#,
		);

		let error =
			load_runtime_configs(&layout).expect_err("api_key in runtime.toml must be rejected");

		assert!(matches!(error, CommandError::RuntimeConfigBootstrap(_)));
		assert!(error.to_string().contains("unknown field `api_key`"));
	}

	#[test]
	fn runtime_toml_rejects_unknown_llm_provider_value() {
		let _env_lock = ENV_MUTEX.lock().expect("env mutex should lock");
		let layout = temp_layout();
		write_runtime_toml(
			&layout,
			r#"
[runtime.llm]
provider = "bogus"
"#,
		);

		let error =
			load_runtime_configs(&layout).expect_err("unknown provider variant should fail");

		assert!(matches!(error, CommandError::RuntimeConfigBootstrap(_)));
	}

	#[test]
	fn runtime_toml_rejects_provider_specific_memory_fields_at_top_level() {
		let _env_lock = ENV_MUTEX.lock().expect("env mutex should lock");
		let layout = temp_layout();
		write_runtime_toml(
			&layout,
			r#"
[runtime.memory]
enabled = true
base_url = "http://127.0.0.1:1933"
"#,
		);

		let error = load_runtime_configs(&layout)
			.expect_err("provider-specific memory fields should stay under backends.*");

		assert!(matches!(error, CommandError::RuntimeConfigBootstrap(_)));
		assert!(error.to_string().contains("unknown field `base_url`"));
	}

	#[test]
	fn canonical_and_legacy_env_overrides_apply_on_top_of_toml() {
		let _env_lock = ENV_MUTEX.lock().expect("env mutex should lock");
		let layout = temp_layout();
		write_runtime_toml(
			&layout,
			r#"
[runtime.tools.web]
endpoint = "https://toml.example/search"

[runtime.llm.openrouter]
primary_model = "toml-model"

[runtime.telegram]
poll_timeout_seconds = 12

[runtime.agent.next_step]
expected_output_tokens = 222

[runtime.memory]
enabled = false

[runtime.memory.recall]
top_k = 5

[runtime.memory.backends.openviking.process]
managed = false
"#,
		);

		let _clear_tools = EnvGuard::remove("ROKU_RUNTIME__TOOLS__WEB__ENDPOINT");
		let _clear_tools_legacy = EnvGuard::remove("ROKU_WEB_SEARCH_URL");
		let _clear_model = EnvGuard::remove("ROKU_RUNTIME__LLM__OPENROUTER__PRIMARY_MODEL");
		let _clear_model_legacy = EnvGuard::remove("OPENROUTER_PRIMARY_MODEL");
		let _clear_poll = EnvGuard::remove("ROKU_RUNTIME__TELEGRAM__POLL_TIMEOUT_SECONDS");
		let _clear_poll_legacy = EnvGuard::remove("TELEGRAM_POLL_TIMEOUT_SECONDS");
		let _clear_next_step =
			EnvGuard::remove("ROKU_RUNTIME__AGENT__NEXT_STEP__EXPECTED_OUTPUT_TOKENS");
		let _clear_memory_enabled = EnvGuard::remove("ROKU_RUNTIME__MEMORY__ENABLED");
		let _clear_memory_top_k = EnvGuard::remove("ROKU_RUNTIME__MEMORY__RECALL__TOP_K");
		let _clear_memory_embedding_key = EnvGuard::remove(
			"ROKU_RUNTIME__MEMORY__BACKENDS__OPENVIKING__PROCESS__EMBEDDING__API_KEY",
		);
		let _clear_memory_vlm_key =
			EnvGuard::remove("ROKU_RUNTIME__MEMORY__BACKENDS__OPENVIKING__PROCESS__VLM__API_KEY");
		let _clear_memory_legacy_key = EnvGuard::remove("OPENROUTER_API_KEY");

		let _legacy_tools = EnvGuard::set("ROKU_WEB_SEARCH_URL", "https://legacy.example/search");
		let _canonical_tools = EnvGuard::set(
			"ROKU_RUNTIME__TOOLS__WEB__ENDPOINT",
			"https://canonical.example/search",
		);
		let _legacy_model = EnvGuard::set("OPENROUTER_PRIMARY_MODEL", "legacy-model");
		let _canonical_model = EnvGuard::set(
			"ROKU_RUNTIME__LLM__OPENROUTER__PRIMARY_MODEL",
			"canonical-model",
		);
		let _legacy_poll = EnvGuard::set("TELEGRAM_POLL_TIMEOUT_SECONDS", "33");
		let _canonical_poll = EnvGuard::set("ROKU_RUNTIME__TELEGRAM__POLL_TIMEOUT_SECONDS", "44");
		let _canonical_next_step = EnvGuard::set(
			"ROKU_RUNTIME__AGENT__NEXT_STEP__EXPECTED_OUTPUT_TOKENS",
			"321",
		);
		let _canonical_memory_enabled = EnvGuard::set("ROKU_RUNTIME__MEMORY__ENABLED", "true");
		let _canonical_memory_top_k = EnvGuard::set("ROKU_RUNTIME__MEMORY__RECALL__TOP_K", "12");
		let _legacy_memory_key = EnvGuard::set("OPENROUTER_API_KEY", "legacy-memory-key");

		let configs = load_runtime_configs(&layout).expect("env overrides should load");

		assert_eq!(configs.agent.next_step.expected_output_tokens, 321);
		assert_eq!(
			configs.tools.web.endpoint.as_deref(),
			Some("https://canonical.example/search")
		);
		assert_eq!(configs.openrouter.primary_model, "canonical-model");
		assert_eq!(configs.telegram.poll_timeout_seconds, 44);
		assert!(configs.memory.enabled);
		assert_eq!(configs.memory.recall.top_k, 12);
		assert_eq!(
			configs
				.memory
				.backends
				.openviking
				.process
				.embedding
				.api_key
				.as_deref(),
			Some("legacy-memory-key")
		);
		assert_eq!(
			configs
				.memory
				.backends
				.openviking
				.process
				.vlm
				.api_key
				.as_deref(),
			Some("legacy-memory-key")
		);
	}

	#[test]
	fn managed_openviking_config_is_generated_from_typed_memory_config() {
		let _env_lock = ENV_MUTEX.lock().expect("env mutex should lock");
		let layout = temp_layout();
		write_runtime_toml(
			&layout,
			r#"
[runtime.memory]
enabled = true
backend = "openviking"

[runtime.memory.backends.openviking.process]
managed = true
config_output_path = ".roku/run/openviking/generated.ov.conf"
"#,
		);

		let _clear_embedding_key = EnvGuard::remove(
			"ROKU_RUNTIME__MEMORY__BACKENDS__OPENVIKING__PROCESS__EMBEDDING__API_KEY",
		);
		let _clear_vlm_key =
			EnvGuard::remove("ROKU_RUNTIME__MEMORY__BACKENDS__OPENVIKING__PROCESS__VLM__API_KEY");
		let _clear_legacy = EnvGuard::remove("OPENROUTER_API_KEY");
		let _legacy_key = EnvGuard::set("OPENROUTER_API_KEY", "phase1-generated-key");

		let configs = load_runtime_configs(&layout).expect("memory config should load");
		let generated_path = prepare_runtime_generated_artifacts(&configs)
			.expect("generated config should be written")
			.expect("managed config should produce file");
		let content =
			fs::read_to_string(&generated_path).expect("generated OpenViking config should exist");

		assert!(generated_path.ends_with(".roku/run/openviking/generated.ov.conf"));
		assert!(content.contains("\"storage\""));
		assert!(content.contains("\"embedding\""));
		assert!(content.contains("\"dense\""));
		assert!(!content.contains("\"vectordb\""));
		assert!(!content.contains("\"agfs\""));
		assert!(!content.contains("\"server\""));
		assert!(content.contains("\"model\": \"thenlper/gte-base\""));
		assert!(content.contains("\"model\": \"qwen/qwen3.5-flash-02-23\""));
		assert!(content.contains("\"max_concurrent\": 8"));
		assert!(content.contains("\"api_key\": \"phase1-generated-key\""));
	}
}
