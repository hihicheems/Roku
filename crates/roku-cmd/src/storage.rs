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
	pub prompt_archive_dir: PathBuf,
	pub memory_summary_dir: PathBuf,
	pub audit_export_dir: PathBuf,
	pub log_dir: PathBuf,
	pub run_dir: PathBuf,
	pub cache_dir: PathBuf,
}

impl LocalStorageLayout {
	pub fn from_env() -> Self {
		let home_dir = env_path("ROKU_HOME").unwrap_or_else(default_roku_home);
		let state_dir = env_path("ROKU_STATE_DIR").unwrap_or_else(|| home_dir.join("state"));
		let sqlite_path =
			env_path("ROKU_SQLITE_PATH").unwrap_or_else(|| state_dir.join("control-plane.db"));
		let artifact_root =
			env_path("ROKU_ARTIFACT_ROOT").unwrap_or_else(|| home_dir.join("artifacts"));
		let experiment_root =
			env_path("ROKU_EXPERIMENT_ROOT").unwrap_or_else(|| home_dir.join("experiments"));
		let report_root = env_path("ROKU_REPORT_ROOT").unwrap_or_else(|| home_dir.join("reports"));
		let prompt_archive_dir =
			env_path("ROKU_PROMPT_ARCHIVE_DIR").unwrap_or_else(|| home_dir.join("prompts"));
		let memory_summary_dir =
			env_path("ROKU_MEMORY_SUMMARY_DIR").unwrap_or_else(|| home_dir.join("memory"));
		let audit_export_dir = env_path("ROKU_AUDIT_EXPORT_DIR")
			.unwrap_or_else(|| home_dir.join("exports").join("audit"));
		let log_dir = env_path("ROKU_LOG_DIR").unwrap_or_else(|| home_dir.join("logs"));
		let run_dir = env_path("ROKU_RUN_DIR").unwrap_or_else(|| home_dir.join("run"));
		let cache_dir = env_path("ROKU_CACHE_DIR").unwrap_or_else(|| home_dir.join("cache"));

		Self {
			home_dir,
			state_dir,
			sqlite_path,
			artifact_root,
			experiment_root,
			report_root,
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
	}
}
