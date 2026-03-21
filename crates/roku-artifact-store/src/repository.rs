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
	#[error("immutable snapshot ref conflict for uri `{uri}`")]
	ImmutableSnapshotConflict { uri: String },
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

	fn save_immutable_artifact(
		&mut self,
		artifact: Artifact,
		content: String,
	) -> Result<(), ArtifactStoreError> {
		let conflict = || ArtifactStoreError::ImmutableSnapshotConflict {
			uri: artifact.uri.clone(),
		};

		if let Some(existing) = self.load_artifact(&artifact.artifact_id)?
			&& !artifacts_match(&existing, &artifact)
		{
			return Err(conflict());
		}
		if let Some(existing) = self.load_by_uri(&artifact.uri)?
			&& !artifacts_match(&existing, &artifact)
		{
			return Err(conflict());
		}
		if let Some(existing_content) = self.load_content_by_uri(&artifact.uri)?
			&& existing_content != content
		{
			return Err(conflict());
		}

		self.save_artifact(artifact.clone())?;
		self.save_content(&artifact.uri, content)?;
		Ok(())
	}
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
	root: PathBuf,
}

impl FileArtifactRepository {
	pub fn new(root: impl Into<PathBuf>) -> Self {
		Self { root: root.into() }
	}

	fn metadata_dir(&self) -> PathBuf {
		self.root.join("metadata")
	}

	fn content_dir(&self) -> PathBuf {
		self.root.join("content")
	}

	fn metadata_path(&self, artifact_id: &ArtifactId) -> PathBuf {
		self.metadata_dir().join(format!("{}.json", artifact_id.0))
	}

	fn content_path(&self, task_id: &TaskId, artifact_id: &ArtifactId, content: &str) -> PathBuf {
		let extension = if serde_json::from_str::<serde_json::Value>(content).is_ok() {
			"json"
		} else {
			"md"
		};
		self.content_dir()
			.join(&task_id.0)
			.join(format!("{}.{}", artifact_id.0, extension))
	}

	fn load_record(
		&self,
		artifact_id: &ArtifactId,
	) -> Result<Option<FileArtifactRecord>, ArtifactStoreError> {
		let path = self.metadata_path(artifact_id);
		if !path.exists() {
			return Ok(None);
		}
		let data = fs::read_to_string(path)?;
		Ok(Some(serde_json::from_str(&data)?))
	}

	fn save_record(&self, record: &FileArtifactRecord) -> Result<(), ArtifactStoreError> {
		let path = self.metadata_path(&record.artifact.artifact_id);
		ensure_parent_dir(&path)?;
		write_text_atomically(&path, &serde_json::to_string_pretty(record)?)
	}

	fn iter_records(&self) -> Result<Vec<FileArtifactRecord>, ArtifactStoreError> {
		let metadata_dir = self.metadata_dir();
		if !metadata_dir.exists() {
			return Ok(Vec::new());
		}
		let mut records: Vec<FileArtifactRecord> = Vec::new();
		for entry in fs::read_dir(metadata_dir)? {
			let entry = entry?;
			if entry.file_type()?.is_file() {
				let data = fs::read_to_string(entry.path())?;
				records.push(serde_json::from_str(&data)?);
			}
		}
		records.sort_by(|left, right| {
			left.artifact
				.artifact_id
				.0
				.cmp(&right.artifact.artifact_id.0)
		});
		Ok(records)
	}
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FileArtifactRecord {
	artifact: Artifact,
	#[serde(default)]
	content_rel_path: Option<String>,
}

impl ArtifactRepository for FileArtifactRepository {
	fn save_artifact(&mut self, artifact: Artifact) -> Result<(), ArtifactStoreError> {
		let mut record = self
			.load_record(&artifact.artifact_id)?
			.unwrap_or(FileArtifactRecord {
				artifact: artifact.clone(),
				content_rel_path: None,
			});
		record.artifact = artifact;
		self.save_record(&record)
	}

	fn load_artifact(
		&self,
		artifact_id: &ArtifactId,
	) -> Result<Option<Artifact>, ArtifactStoreError> {
		Ok(self.load_record(artifact_id)?.map(|record| record.artifact))
	}

	fn load_by_uri(&self, uri: &str) -> Result<Option<Artifact>, ArtifactStoreError> {
		Ok(self
			.iter_records()?
			.into_iter()
			.find(|record| record.artifact.uri == uri)
			.map(|record| record.artifact))
	}

	fn list_by_task(&self, task_id: &TaskId) -> Result<Vec<Artifact>, ArtifactStoreError> {
		Ok(self
			.iter_records()?
			.into_iter()
			.map(|record| record.artifact)
			.filter(|artifact| artifact.task_id == *task_id)
			.collect())
	}

	fn save_content(&mut self, uri: &str, content: String) -> Result<(), ArtifactStoreError> {
		let Some(mut record) = self
			.iter_records()?
			.into_iter()
			.find(|record| record.artifact.uri == uri)
		else {
			return Ok(());
		};
		let content_path = self.content_path(
			&record.artifact.task_id,
			&record.artifact.artifact_id,
			&content,
		);
		ensure_parent_dir(&content_path)?;
		write_text_atomically(&content_path, &content)?;
		record.content_rel_path = Some(
			content_path
				.strip_prefix(&self.root)
				.unwrap_or(&content_path)
				.to_string_lossy()
				.to_string(),
		);
		self.save_record(&record)
	}

	fn load_content_by_uri(&self, uri: &str) -> Result<Option<String>, ArtifactStoreError> {
		let Some(record) = self
			.iter_records()?
			.into_iter()
			.find(|record| record.artifact.uri == uri)
		else {
			return Ok(None);
		};
		let Some(content_rel_path) = record.content_rel_path else {
			return Ok(None);
		};
		let content_path = self.root.join(content_rel_path);
		if !content_path.exists() {
			return Ok(None);
		}
		Ok(Some(fs::read_to_string(content_path)?))
	}
}

fn ensure_parent_dir(path: &Path) -> Result<(), ArtifactStoreError> {
	if let Some(parent) = path.parent() {
		fs::create_dir_all(parent)?;
	}
	Ok(())
}

fn write_text_atomically(path: &Path, content: &str) -> Result<(), ArtifactStoreError> {
	ensure_parent_dir(path)?;
	let temp_path = path.with_extension(format!(
		"{}.tmp",
		path.extension()
			.and_then(|extension| extension.to_str())
			.unwrap_or("data")
	));
	fs::write(&temp_path, content)?;
	fs::rename(temp_path, path)?;
	Ok(())
}

fn artifacts_match(left: &Artifact, right: &Artifact) -> bool {
	left.artifact_id.0 == right.artifact_id.0
		&& left.task_id == right.task_id
		&& left.node_id == right.node_id
		&& left.kind == right.kind
		&& left.uri == right.uri
		&& left.schema_version == right.schema_version
		&& left.checksum == right.checksum
		&& metadata_matches(&left.metadata, &right.metadata)
}

fn metadata_matches(
	left: &[roku_common_types::ArtifactMetadataEntry],
	right: &[roku_common_types::ArtifactMetadataEntry],
) -> bool {
	left.len() == right.len()
		&& left
			.iter()
			.zip(right.iter())
			.all(|(left, right)| left.key == right.key && left.value == right.value)
}
