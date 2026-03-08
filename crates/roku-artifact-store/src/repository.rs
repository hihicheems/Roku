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

use roku_common_types::{Artifact, ArtifactId, TaskId};
use serde::{Deserialize, Serialize};
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
	fn save_content(&mut self, uri: &str, content: String) -> Result<(), ArtifactStoreError>;
	fn load_content_by_uri(&self, uri: &str) -> Result<Option<String>, ArtifactStoreError>;
}

#[derive(Debug, Default)]
pub struct InMemoryArtifactRepository {
	artifacts: HashMap<String, Artifact>,
	contents: HashMap<String, String>,
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

	fn save_content(&mut self, uri: &str, content: String) -> Result<(), ArtifactStoreError> {
		self.contents.insert(uri.to_string(), content);
		Ok(())
	}

	fn load_content_by_uri(&self, uri: &str) -> Result<Option<String>, ArtifactStoreError> {
		Ok(self.contents.get(uri).cloned())
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

	fn read_all(&self) -> Result<FileArtifactSnapshot, ArtifactStoreError> {
		if !self.path.exists() {
			return Ok(FileArtifactSnapshot::default());
		}
		let data = fs::read_to_string(&self.path)?;
		if data.trim().is_empty() {
			return Ok(FileArtifactSnapshot::default());
		}
		if let Ok(snapshot) = serde_json::from_str::<FileArtifactSnapshot>(&data) {
			return Ok(snapshot);
		}

		// Backward compatibility: previous format stored only artifact map.
		let artifacts = serde_json::from_str::<HashMap<String, Artifact>>(&data)?;
		Ok(FileArtifactSnapshot {
			artifacts,
			contents: HashMap::new(),
		})
	}

	fn write_all(&self, snapshot: &FileArtifactSnapshot) -> Result<(), ArtifactStoreError> {
		ensure_parent_dir(&self.path)?;
		let encoded = serde_json::to_string_pretty(snapshot)?;
		fs::write(&self.path, encoded)?;
		Ok(())
	}
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct FileArtifactSnapshot {
	#[serde(default)]
	artifacts: HashMap<String, Artifact>,
	#[serde(default)]
	contents: HashMap<String, String>,
}

impl ArtifactRepository for FileArtifactRepository {
	fn save_artifact(&mut self, artifact: Artifact) -> Result<(), ArtifactStoreError> {
		let mut snapshot = self.read_all()?;
		snapshot
			.artifacts
			.insert(artifact.artifact_id.0.clone(), artifact);
		self.write_all(&snapshot)
	}

	fn load_artifact(
		&self,
		artifact_id: &ArtifactId,
	) -> Result<Option<Artifact>, ArtifactStoreError> {
		let snapshot = self.read_all()?;
		Ok(snapshot.artifacts.get(&artifact_id.0).cloned())
	}

	fn load_by_uri(&self, uri: &str) -> Result<Option<Artifact>, ArtifactStoreError> {
		let snapshot = self.read_all()?;
		Ok(snapshot
			.artifacts
			.values()
			.find(|artifact| artifact.uri == uri)
			.cloned())
	}

	fn list_by_task(&self, task_id: &TaskId) -> Result<Vec<Artifact>, ArtifactStoreError> {
		let snapshot = self.read_all()?;
		Ok(snapshot
			.artifacts
			.values()
			.filter(|artifact| artifact.task_id == *task_id)
			.cloned()
			.collect())
	}

	fn save_content(&mut self, uri: &str, content: String) -> Result<(), ArtifactStoreError> {
		let mut snapshot = self.read_all()?;
		snapshot.contents.insert(uri.to_string(), content);
		self.write_all(&snapshot)
	}

	fn load_content_by_uri(&self, uri: &str) -> Result<Option<String>, ArtifactStoreError> {
		let snapshot = self.read_all()?;
		Ok(snapshot.contents.get(uri).cloned())
	}
}

fn ensure_parent_dir(path: &Path) -> Result<(), ArtifactStoreError> {
	if let Some(parent) = path.parent() {
		fs::create_dir_all(parent)?;
	}
	Ok(())
}
