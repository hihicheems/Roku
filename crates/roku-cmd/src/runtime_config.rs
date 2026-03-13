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

use roku_agent_runtime::{ToolsRuntimeConfig, ToolsRuntimeConfigPatch};
use roku_plugin_llm::{OpenRouterRuntimeConfig, OpenRouterRuntimeConfigPatch};
use roku_plugin_skills::{SkillsRuntimeConfig, SkillsRuntimeConfigPatch};
use roku_plugin_telegram::{TelegramRuntimeConfig, TelegramRuntimeConfigPatch};
use serde::Deserialize;

use crate::{CommandError, storage::LocalStorageLayout};

/// Effective runtime configuration bundle for plugin crates.
///
/// The startup layer owns file parsing and composition. Each plugin crate owns
/// its typed config, patch application, and validate/clamp logic.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PluginRuntimeConfigs {
	pub tools: ToolsRuntimeConfig,
	pub openrouter: OpenRouterRuntimeConfig,
	pub telegram: TelegramRuntimeConfig,
	pub skills: SkillsRuntimeConfig,
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
	tools: ToolsRuntimeConfigPatch,
	#[serde(default)]
	llm: LlmSections,
	#[serde(default)]
	telegram: TelegramRuntimeConfigPatch,
	#[serde(default)]
	skills: SkillsRuntimeConfigPatch,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct LlmSections {
	#[serde(default)]
	openrouter: OpenRouterRuntimeConfigPatch,
}

pub(crate) fn load_plugin_runtime_configs(
	layout: &LocalStorageLayout,
) -> Result<PluginRuntimeConfigs, CommandError> {
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

	let mut openrouter = OpenRouterRuntimeConfig::default();
	openrouter.apply_patch(parsed.runtime.llm.openrouter);
	openrouter
		.apply_env_overrides()
		.map_err(CommandError::from)?;
	openrouter
		.validate_and_clamp()
		.map_err(CommandError::from)?;

	let mut telegram = TelegramRuntimeConfig::default();
	telegram.apply_patch(parsed.runtime.telegram);
	telegram.apply_env_overrides().map_err(CommandError::from)?;
	telegram.validate_and_clamp().map_err(CommandError::from)?;

	let mut skills = SkillsRuntimeConfig::default();
	skills.apply_patch(parsed.runtime.skills);
	skills.apply_env_overrides().map_err(CommandError::from)?;
	skills.validate_and_clamp().map_err(CommandError::from)?;

	Ok(PluginRuntimeConfigs {
		tools,
		openrouter,
		telegram,
		skills,
	})
}

#[cfg(test)]
mod tests {
	use std::fs;
	use std::sync::{LazyLock, Mutex};

	use super::*;

	const HARD_MAX_READ_BYTES: usize = 256 * 1024;
	const HARD_MAX_DIR_ENTRIES: usize = 2_000;
	const HARD_MAX_GLOB_MATCHES: usize = 2_000;
	const HARD_MAX_DESCENDANT_SCAN_ENTRIES: usize = 50_000;
	const HARD_MAX_WEB_TOP_K: usize = 20;
	const HARD_MAX_LLM_TOOL_TIMEOUT_MS: u64 = 180_000;

	static ENV_MUTEX: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

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
			sqlite_path: root.join(".roku/state/control-plane.db"),
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

		let configs = load_plugin_runtime_configs(&layout).expect("defaults should load");

		assert_eq!(configs.tools.fs.default_max_bytes, 4_096);
		assert_eq!(configs.openrouter.max_latency_ms, 60_000);
		assert_eq!(configs.telegram.poll_timeout_seconds, 30);
		assert_eq!(configs.skills.max_prompt_documents, 24);
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
"#,
		);

		let configs = load_plugin_runtime_configs(&layout).expect("runtime toml should load");

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

		let error =
			load_plugin_runtime_configs(&layout).expect_err("unknown field should fail bootstrap");

		assert!(matches!(error, CommandError::RuntimeConfigBootstrap(_)));
		assert!(error.to_string().contains("unknown field `api_key`"));
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
"#,
		);

		let _clear_tools = EnvGuard::remove("ROKU_RUNTIME__TOOLS__WEB__ENDPOINT");
		let _clear_tools_legacy = EnvGuard::remove("ROKU_WEB_SEARCH_URL");
		let _clear_model = EnvGuard::remove("ROKU_RUNTIME__LLM__OPENROUTER__PRIMARY_MODEL");
		let _clear_model_legacy = EnvGuard::remove("OPENROUTER_PRIMARY_MODEL");
		let _clear_poll = EnvGuard::remove("ROKU_RUNTIME__TELEGRAM__POLL_TIMEOUT_SECONDS");
		let _clear_poll_legacy = EnvGuard::remove("TELEGRAM_POLL_TIMEOUT_SECONDS");

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

		let configs = load_plugin_runtime_configs(&layout).expect("env overrides should load");

		assert_eq!(
			configs.tools.web.endpoint.as_deref(),
			Some("https://canonical.example/search")
		);
		assert_eq!(configs.openrouter.primary_model, "canonical-model");
		assert_eq!(configs.telegram.poll_timeout_seconds, 44);
	}
}
