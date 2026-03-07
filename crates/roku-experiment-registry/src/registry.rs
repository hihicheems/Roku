use roku_common_types::{
	ArtifactId, ExperimentMetric, ExperimentRun, ExperimentRunId, ExperimentStatus, RequestId,
	TaskId,
};

use crate::repository::{
	ExperimentRegistryError, ExperimentRunRepository, FileExperimentRunRepository,
	InMemoryExperimentRunRepository,
};

pub struct ExperimentRegistry {
	repository: Box<dyn ExperimentRunRepository + Send>,
}

impl ExperimentRegistry {
	pub fn new(repository: Box<dyn ExperimentRunRepository + Send>) -> Self {
		Self { repository }
	}

	pub fn in_memory() -> Self {
		Self::new(Box::new(InMemoryExperimentRunRepository::default()))
	}

	pub fn file_backed(path: impl Into<std::path::PathBuf>) -> Self {
		Self::new(Box::new(FileExperimentRunRepository::new(path)))
	}

	pub fn start_run(
		&mut self,
		task_id: &TaskId,
		request_id: &RequestId,
		goal: &str,
		strategy: &str,
	) -> Result<ExperimentRun, ExperimentRegistryError> {
		let run = ExperimentRun {
			run_id: ExperimentRunId(format!("experiment-{}", task_id.0)),
			task_id: task_id.clone(),
			request_id: request_id.clone(),
			goal: goal.to_string(),
			strategy: strategy.to_string(),
			status: ExperimentStatus::Running,
			summary: None,
			metrics: Vec::new(),
			artifact_ids: Vec::new(),
			failure_reason: None,
		};
		self.repository.save_run(run.clone())?;
		Ok(run)
	}

	pub fn attach_artifact(
		&mut self,
		task_id: &TaskId,
		artifact_id: ArtifactId,
	) -> Result<ExperimentRun, ExperimentRegistryError> {
		let mut run = self
			.repository
			.load_by_task(task_id)?
			.ok_or_else(|| ExperimentRegistryError::RunNotFound(task_id.0.clone()))?;
		if run.artifact_ids.iter().all(|existing| existing != &artifact_id) {
			run.artifact_ids.push(artifact_id);
		}
		self.repository.save_run(run.clone())?;
		Ok(run)
	}

	pub fn complete_run(
		&mut self,
		task_id: &TaskId,
		summary: impl Into<String>,
		metrics: Vec<ExperimentMetric>,
	) -> Result<ExperimentRun, ExperimentRegistryError> {
		let mut run = self
			.repository
			.load_by_task(task_id)?
			.ok_or_else(|| ExperimentRegistryError::RunNotFound(task_id.0.clone()))?;
		run.status = ExperimentStatus::Succeeded;
		run.summary = Some(summary.into());
		run.metrics = metrics;
		run.failure_reason = None;
		self.repository.save_run(run.clone())?;
		Ok(run)
	}

	pub fn fail_run(
		&mut self,
		task_id: &TaskId,
		reason: impl Into<String>,
	) -> Result<ExperimentRun, ExperimentRegistryError> {
		let mut run = self
			.repository
			.load_by_task(task_id)?
			.ok_or_else(|| ExperimentRegistryError::RunNotFound(task_id.0.clone()))?;
		run.status = ExperimentStatus::Failed;
		run.failure_reason = Some(reason.into());
		self.repository.save_run(run.clone())?;
		Ok(run)
	}

	pub fn load_by_task(
		&self,
		task_id: &TaskId,
	) -> Result<Option<ExperimentRun>, ExperimentRegistryError> {
		self.repository.load_by_task(task_id)
	}
}

impl Default for ExperimentRegistry {
	fn default() -> Self {
		Self::in_memory()
	}
}

#[cfg(test)]
mod tests {
	use std::time::{SystemTime, UNIX_EPOCH};

	use roku_common_types::{ArtifactId, ExperimentStatus};

	use super::*;

	fn temp_path(suffix: &str) -> std::path::PathBuf {
		let nanos = SystemTime::now()
			.duration_since(UNIX_EPOCH)
			.expect("clock should be after epoch")
			.as_nanos();
		std::env::temp_dir().join(format!("roku-experiment-{suffix}-{nanos}.json"))
	}

	#[test]
	fn run_lifecycle_works_in_memory() {
		let mut registry = ExperimentRegistry::default();
		let task_id = TaskId("task-1".to_string());
		let request_id = RequestId("req-1".to_string());

		registry
			.start_run(&task_id, &request_id, "goal", "ReAct")
			.expect("run should start");
		registry
			.attach_artifact(&task_id, ArtifactId("artifact-1".to_string()))
			.expect("artifact should attach");
		let completed = registry
			.complete_run(
				&task_id,
				"summary",
				vec![ExperimentMetric {
					name: "validated_results".to_string(),
					value: 1.0,
				}],
			)
			.expect("run should complete");

		assert_eq!(completed.status, ExperimentStatus::Succeeded);
		assert_eq!(completed.artifact_ids.len(), 1);
		assert_eq!(completed.metrics.len(), 1);
	}

	#[test]
	fn failed_run_roundtrips_file_backed() {
		let path = temp_path("registry");
		let mut registry = ExperimentRegistry::file_backed(path.clone());
		let task_id = TaskId("task-2".to_string());
		let request_id = RequestId("req-2".to_string());

		registry
			.start_run(&task_id, &request_id, "goal", "TreeSearch")
			.expect("run should start");
		registry
			.fail_run(&task_id, "validation failed")
			.expect("run should fail");

		let reloaded = ExperimentRegistry::file_backed(path.clone());
		let loaded = reloaded
			.load_by_task(&task_id)
			.expect("run load should succeed")
			.expect("run should exist");
		assert_eq!(loaded.status, ExperimentStatus::Failed);
		assert_eq!(loaded.failure_reason.as_deref(), Some("validation failed"));

		let _ = std::fs::remove_file(path);
	}
}
