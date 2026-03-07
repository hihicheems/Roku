use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use roku_common_types::{Artifact, ArtifactId, TaskId};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ArtifactStoreError {
	#[error("io error: {0}")]
	Io(#[from] std::io::Error),
	#[error("serialization error: {0}")]
	Serde(#[from] serde_json::Error),
}

pub trait ArtifactRepository {
	fn save_artifact(&mut self, artifact: Artifact) -> Result<(), ArtifactStoreError>;
	fn load_artifact(
		&self,
		artifact_id: &ArtifactId,
	) -> Result<Option<Artifact>, ArtifactStoreError>;
	fn load_by_uri(&self, uri: &str) -> Result<Option<Artifact>, ArtifactStoreError>;
	fn list_by_task(&self, task_id: &TaskId) -> Result<Vec<Artifact>, ArtifactStoreError>;
}

#[derive(Debug, Default)]
pub struct InMemoryArtifactRepository {
	artifacts: HashMap<String, Artifact>,
}

impl ArtifactRepository for InMemoryArtifactRepository {
	fn save_artifact(&mut self, artifact: Artifact) -> Result<(), ArtifactStoreError> {
		self.artifacts
			.insert(artifact.artifact_id.0.clone(), artifact);
		Ok(())
	}

	fn load_artifact(
		&self,
		artifact_id: &ArtifactId,
	) -> Result<Option<Artifact>, ArtifactStoreError> {
		Ok(self.artifacts.get(&artifact_id.0).cloned())
	}

	fn load_by_uri(&self, uri: &str) -> Result<Option<Artifact>, ArtifactStoreError> {
		Ok(self
			.artifacts
			.values()
			.find(|artifact| artifact.uri == uri)
			.cloned())
	}

	fn list_by_task(&self, task_id: &TaskId) -> Result<Vec<Artifact>, ArtifactStoreError> {
		Ok(self
			.artifacts
			.values()
			.filter(|artifact| artifact.task_id == *task_id)
			.cloned()
			.collect())
	}
}

#[derive(Debug, Clone)]
pub struct FileArtifactRepository {
	path: PathBuf,
}

impl FileArtifactRepository {
	pub fn new(path: impl Into<PathBuf>) -> Self {
		Self { path: path.into() }
	}

	fn read_all(&self) -> Result<HashMap<String, Artifact>, ArtifactStoreError> {
		if !self.path.exists() {
			return Ok(HashMap::new());
		}
		let data = fs::read_to_string(&self.path)?;
		if data.trim().is_empty() {
			return Ok(HashMap::new());
		}
		Ok(serde_json::from_str(&data)?)
	}

	fn write_all(&self, artifacts: &HashMap<String, Artifact>) -> Result<(), ArtifactStoreError> {
		ensure_parent_dir(&self.path)?;
		let encoded = serde_json::to_string_pretty(artifacts)?;
		fs::write(&self.path, encoded)?;
		Ok(())
	}
}

impl ArtifactRepository for FileArtifactRepository {
	fn save_artifact(&mut self, artifact: Artifact) -> Result<(), ArtifactStoreError> {
		let mut artifacts = self.read_all()?;
		artifacts.insert(artifact.artifact_id.0.clone(), artifact);
		self.write_all(&artifacts)
	}

	fn load_artifact(
		&self,
		artifact_id: &ArtifactId,
	) -> Result<Option<Artifact>, ArtifactStoreError> {
		let artifacts = self.read_all()?;
		Ok(artifacts.get(&artifact_id.0).cloned())
	}

	fn load_by_uri(&self, uri: &str) -> Result<Option<Artifact>, ArtifactStoreError> {
		let artifacts = self.read_all()?;
		Ok(artifacts
			.values()
			.find(|artifact| artifact.uri == uri)
			.cloned())
	}

	fn list_by_task(&self, task_id: &TaskId) -> Result<Vec<Artifact>, ArtifactStoreError> {
		let artifacts = self.read_all()?;
		Ok(artifacts
			.values()
			.filter(|artifact| artifact.task_id == *task_id)
			.cloned()
			.collect())
	}
}

fn ensure_parent_dir(path: &Path) -> Result<(), ArtifactStoreError> {
	if let Some(parent) = path.parent() {
		fs::create_dir_all(parent)?;
	}
	Ok(())
}
