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
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocalStorageLayout {
	pub home_dir: PathBuf,
	pub state_dir: PathBuf,
	pub sqlite_path: PathBuf,
	pub artifact_root: PathBuf,
	pub experiment_root: PathBuf,
	pub report_root: PathBuf,
	pub skill_root: PathBuf,
	pub generated_skill_root: PathBuf,
	pub tool_config_path: PathBuf,
	pub runtime_config_path: PathBuf,
	pub plugin_config_path: PathBuf,
	pub workspace_plugin_root: PathBuf,
	pub user_plugin_root: PathBuf,
	pub prompt_archive_dir: PathBuf,
	pub memory_summary_dir: PathBuf,
	pub audit_export_dir: PathBuf,
	pub log_dir: PathBuf,
	pub run_dir: PathBuf,
	pub cache_dir: PathBuf,
}

impl LocalStorageLayout {
	pub fn from_env() -> Self {
		let roku_home = env_path("ROKU_HOME").unwrap_or_else(default_roku_home);
		let state_dir = env_path("ROKU_STATE_DIR").unwrap_or_else(|| roku_home.join("state"));
		let sqlite_path =
			env_path("ROKU_SQLITE_PATH").unwrap_or_else(|| state_dir.join("control-plane.db"));
		let artifact_root =
			env_path("ROKU_ARTIFACT_ROOT").unwrap_or_else(|| roku_home.join("artifacts"));
		let experiment_root =
			env_path("ROKU_EXPERIMENT_ROOT").unwrap_or_else(|| roku_home.join("experiments"));
		let report_root = env_path("ROKU_REPORT_ROOT").unwrap_or_else(|| roku_home.join("reports"));
		let skill_root =
			normalize_path(env_path("ROKU_SKILL_ROOT").unwrap_or_else(default_project_skill_root));
		let generated_skill_root = skill_root.clone();
		let tool_config_path = env_path("ROKU_TOOL_CONFIG_PATH")
			.unwrap_or_else(|| PathBuf::from("config").join("tools.toml"));
		let runtime_config_path = env_path("ROKU_RUNTIME_CONFIG_PATH")
			.unwrap_or_else(|| PathBuf::from("config").join("runtime.toml"));
		let plugin_config_path = env_path("ROKU_PLUGIN_CONFIG_PATH")
			.unwrap_or_else(|| PathBuf::from("config").join("plugins.toml"));
		let workspace_plugin_root = normalize_path(PathBuf::from(".roku").join("plugins"));
		let user_plugin_root = home_dir()
			.map(|home| home.join(".roku").join("plugins"))
			.unwrap_or_else(|| PathBuf::from(".roku").join("plugins"));
		let prompt_archive_dir =
			env_path("ROKU_PROMPT_ARCHIVE_DIR").unwrap_or_else(|| roku_home.join("prompts"));
		let memory_summary_dir =
			env_path("ROKU_MEMORY_SUMMARY_DIR").unwrap_or_else(|| roku_home.join("memory"));
		let audit_export_dir = env_path("ROKU_AUDIT_EXPORT_DIR")
			.unwrap_or_else(|| roku_home.join("exports").join("audit"));
		let log_dir = env_path("ROKU_LOG_DIR").unwrap_or_else(|| roku_home.join("logs"));
		let run_dir = env_path("ROKU_RUN_DIR").unwrap_or_else(|| roku_home.join("run"));
		let cache_dir = env_path("ROKU_CACHE_DIR").unwrap_or_else(|| roku_home.join("cache"));

		Self {
			home_dir: roku_home,
			state_dir,
			sqlite_path,
			artifact_root,
			experiment_root,
			report_root,
			skill_root,
			generated_skill_root,
			tool_config_path,
			runtime_config_path,
			plugin_config_path,
			workspace_plugin_root,
			user_plugin_root,
			prompt_archive_dir,
			memory_summary_dir,
			audit_export_dir,
			log_dir,
			run_dir,
			cache_dir,
		}
	}

	pub fn ensure_dirs(&self) -> std::io::Result<()> {
		for directory in [
			&self.home_dir,
			&self.state_dir,
			&self.artifact_root,
			&self.experiment_root,
			&self.report_root,
			&self.skill_root,
			&self.generated_skill_root,
			&self.workspace_plugin_root,
			&self.prompt_archive_dir,
			&self.memory_summary_dir,
			&self.audit_export_dir,
			&self.log_dir,
			&self.run_dir,
			&self.cache_dir,
		] {
			fs::create_dir_all(directory)?;
		}
		Ok(())
	}
}

fn env_path(key: &str) -> Option<PathBuf> {
	env::var(key)
		.ok()
		.filter(|value| !value.trim().is_empty())
		.map(|value| expand_home(value.trim()))
}

fn default_roku_home() -> PathBuf {
	home_dir()
		.map(|home| home.join(".roku"))
		.unwrap_or_else(|| PathBuf::from(".roku"))
}

fn default_project_skill_root() -> PathBuf {
	env::current_dir()
		.map(|cwd| cwd.join(".roku").join("skills"))
		.unwrap_or_else(|_| PathBuf::from(".roku").join("skills"))
}

fn normalize_path(path: PathBuf) -> PathBuf {
	if path.is_absolute() {
		path
	} else {
		env::current_dir()
			.map(|cwd| cwd.join(&path))
			.unwrap_or_else(|_| PathBuf::from(".").join(path))
	}
}

fn expand_home(value: &str) -> PathBuf {
	if value == "~" {
		return home_dir().unwrap_or_else(|| PathBuf::from(value));
	}
	if let Some(suffix) = value.strip_prefix("~/")
		&& let Some(home) = home_dir()
	{
		return home.join(suffix);
	}
	PathBuf::from(value)
}

fn home_dir() -> Option<PathBuf> {
	env::var_os("HOME")
		.map(PathBuf::from)
		.or_else(|| env::var_os("USERPROFILE").map(PathBuf::from))
		.or_else(|| {
			let drive = env::var_os("HOMEDRIVE")?;
			let path = env::var_os("HOMEPATH")?;
			Some(PathBuf::from(drive).join(path))
		})
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn defaults_to_hidden_roku_home() {
		let layout = LocalStorageLayout::from_env();
		assert!(layout.home_dir.ends_with(".roku"));
		assert!(layout.sqlite_path.ends_with("state/control-plane.db"));
		assert!(layout.artifact_root.ends_with("artifacts"));
		assert!(layout.skill_root.ends_with(".roku/skills"));
		assert_eq!(layout.generated_skill_root, layout.skill_root);
		assert!(layout.tool_config_path.ends_with("config/tools.toml"));
		assert!(layout.runtime_config_path.ends_with("config/runtime.toml"));
		assert!(layout.plugin_config_path.ends_with("config/plugins.toml"));
		assert!(layout.workspace_plugin_root.ends_with(".roku/plugins"));
		assert!(layout.user_plugin_root.ends_with(".roku/plugins"));
	}
}
