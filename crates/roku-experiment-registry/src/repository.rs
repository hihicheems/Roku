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

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use roku_common_types::{ExperimentRun, ExperimentRunId, TaskId};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ExperimentRegistryError {
	#[error("io error: {0}")]
	Io(#[from] std::io::Error),
	#[error("serialization error: {0}")]
	Serde(#[from] serde_json::Error),
	#[error("experiment run not found: {0}")]
	RunNotFound(String),
}

pub trait ExperimentRunRepository {
	fn save_run(&mut self, run: ExperimentRun) -> Result<(), ExperimentRegistryError>;
	fn load_run(
		&self,
		run_id: &ExperimentRunId,
	) -> Result<Option<ExperimentRun>, ExperimentRegistryError>;
	fn load_by_task(
		&self,
		task_id: &TaskId,
	) -> Result<Option<ExperimentRun>, ExperimentRegistryError>;
}

#[derive(Debug, Default)]
pub struct InMemoryExperimentRunRepository {
	runs: HashMap<String, ExperimentRun>,
}

impl ExperimentRunRepository for InMemoryExperimentRunRepository {
	fn save_run(&mut self, run: ExperimentRun) -> Result<(), ExperimentRegistryError> {
		self.runs.insert(run.run_id.0.clone(), run);
		Ok(())
	}

	fn load_run(
		&self,
		run_id: &ExperimentRunId,
	) -> Result<Option<ExperimentRun>, ExperimentRegistryError> {
		Ok(self.runs.get(&run_id.0).cloned())
	}

	fn load_by_task(
		&self,
		task_id: &TaskId,
	) -> Result<Option<ExperimentRun>, ExperimentRegistryError> {
		Ok(self
			.runs
			.values()
			.find(|run| run.task_id == *task_id)
			.cloned())
	}
}

#[derive(Debug, Clone)]
pub struct FileExperimentRunRepository {
	root: PathBuf,
}

impl FileExperimentRunRepository {
	pub fn new(root: impl Into<PathBuf>) -> Self {
		Self { root: root.into() }
	}

	fn runs_dir(&self) -> PathBuf {
		self.root.join("runs")
	}

	fn path_for_task(&self, task_id: &TaskId) -> PathBuf {
		self.runs_dir().join(format!("{}.json", task_id.0))
	}

	fn save_run_record(&self, run: &ExperimentRun) -> Result<(), ExperimentRegistryError> {
		let path = self.path_for_task(&run.task_id);
		ensure_parent_dir(&path)?;
		write_text_atomically(&path, &serde_json::to_string_pretty(run)?)
	}

	fn load_run_record(
		&self,
		task_id: &TaskId,
	) -> Result<Option<ExperimentRun>, ExperimentRegistryError> {
		let path = self.path_for_task(task_id);
		if !path.exists() {
			return Ok(None);
		}
		Ok(Some(serde_json::from_str(&fs::read_to_string(path)?)?))
	}

	fn iter_runs(&self) -> Result<Vec<ExperimentRun>, ExperimentRegistryError> {
		let runs_dir = self.runs_dir();
		if !runs_dir.exists() {
			return Ok(Vec::new());
		}
		let mut runs: Vec<ExperimentRun> = Vec::new();
		for entry in fs::read_dir(runs_dir)? {
			let entry = entry?;
			if entry.file_type()?.is_file() {
				runs.push(serde_json::from_str(&fs::read_to_string(entry.path())?)?);
			}
		}
		runs.sort_by(|left, right| left.run_id.0.cmp(&right.run_id.0));
		Ok(runs)
	}
}

impl ExperimentRunRepository for FileExperimentRunRepository {
	fn save_run(&mut self, run: ExperimentRun) -> Result<(), ExperimentRegistryError> {
		self.save_run_record(&run)
	}

	fn load_run(
		&self,
		run_id: &ExperimentRunId,
	) -> Result<Option<ExperimentRun>, ExperimentRegistryError> {
		Ok(self
			.iter_runs()?
			.into_iter()
			.find(|run| run.run_id == *run_id))
	}

	fn load_by_task(
		&self,
		task_id: &TaskId,
	) -> Result<Option<ExperimentRun>, ExperimentRegistryError> {
		self.load_run_record(task_id)
	}
}

fn ensure_parent_dir(path: &Path) -> Result<(), ExperimentRegistryError> {
	if let Some(parent) = path.parent() {
		fs::create_dir_all(parent)?;
	}
	Ok(())
}

fn write_text_atomically(path: &Path, content: &str) -> Result<(), ExperimentRegistryError> {
	ensure_parent_dir(path)?;
	let temp_path = path.with_extension(format!(
		"{}.tmp",
		path.extension()
			.and_then(|extension| extension.to_str())
			.unwrap_or("json")
	));
	fs::write(&temp_path, content)?;
	fs::rename(temp_path, path)?;
	Ok(())
}
