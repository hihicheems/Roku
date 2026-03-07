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
	fn load_by_task(&self, task_id: &TaskId) -> Result<Option<ExperimentRun>, ExperimentRegistryError>;
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

	fn load_by_task(&self, task_id: &TaskId) -> Result<Option<ExperimentRun>, ExperimentRegistryError> {
		Ok(self
			.runs
			.values()
			.find(|run| run.task_id == *task_id)
			.cloned())
	}
}

#[derive(Debug, Clone)]
pub struct FileExperimentRunRepository {
	path: PathBuf,
}

impl FileExperimentRunRepository {
	pub fn new(path: impl Into<PathBuf>) -> Self {
		Self { path: path.into() }
	}

	fn read_all(&self) -> Result<HashMap<String, ExperimentRun>, ExperimentRegistryError> {
		if !self.path.exists() {
			return Ok(HashMap::new());
		}
		let data = fs::read_to_string(&self.path)?;
		if data.trim().is_empty() {
			return Ok(HashMap::new());
		}
		Ok(serde_json::from_str(&data)?)
	}

	fn write_all(
		&self,
		runs: &HashMap<String, ExperimentRun>,
	) -> Result<(), ExperimentRegistryError> {
		ensure_parent_dir(&self.path)?;
		let encoded = serde_json::to_string_pretty(runs)?;
		fs::write(&self.path, encoded)?;
		Ok(())
	}
}

impl ExperimentRunRepository for FileExperimentRunRepository {
	fn save_run(&mut self, run: ExperimentRun) -> Result<(), ExperimentRegistryError> {
		let mut runs = self.read_all()?;
		runs.insert(run.run_id.0.clone(), run);
		self.write_all(&runs)
	}

	fn load_run(
		&self,
		run_id: &ExperimentRunId,
	) -> Result<Option<ExperimentRun>, ExperimentRegistryError> {
		let runs = self.read_all()?;
		Ok(runs.get(&run_id.0).cloned())
	}

	fn load_by_task(&self, task_id: &TaskId) -> Result<Option<ExperimentRun>, ExperimentRegistryError> {
		let runs = self.read_all()?;
		Ok(runs
			.values()
			.find(|run| run.task_id == *task_id)
			.cloned())
	}
}

fn ensure_parent_dir(path: &Path) -> Result<(), ExperimentRegistryError> {
	if let Some(parent) = path.parent() {
		fs::create_dir_all(parent)?;
	}
	Ok(())
}
