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

use roku_common_types::{Artifact, ArtifactId, ArtifactMetadataEntry, ResultEnvelope, TaskId};

use crate::repository::{
	ArtifactRepository, ArtifactStoreError, FileArtifactRepository, InMemoryArtifactRepository,
};

pub struct ArtifactStore {
	repository: Box<dyn ArtifactRepository + Send>,
}

impl ArtifactStore {
	pub fn new(repository: Box<dyn ArtifactRepository + Send>) -> Self {
		Self { repository }
	}

	pub fn in_memory() -> Self {
		Self::new(Box::new(InMemoryArtifactRepository::default()))
	}

	pub fn file_backed(path: impl Into<std::path::PathBuf>) -> Self {
		Self::new(Box::new(FileArtifactRepository::new(path)))
	}

	pub fn persist_result_artifact(
		&mut self,
		result: &ResultEnvelope,
	) -> Result<Artifact, ArtifactStoreError> {
		let artifact = Artifact {
			artifact_id: ArtifactId(format!(
				"artifact-{}-{}",
				result.task_id.0, result.node_id.0
			)),
			task_id: result.task_id.clone(),
			node_id: result.node_id.clone(),
			kind: "node_result".to_string(),
			uri: result_artifact_uri(&result.task_id, &result.node_id),
			schema_version: result.schema_version.clone(),
			checksum: payload_checksum(&result.payload),
			metadata: vec![
				ArtifactMetadataEntry {
					key: "producer".to_string(),
					value: result.producer.clone(),
				},
				ArtifactMetadataEntry {
					key: "status".to_string(),
					value: format!("{:?}", result.status),
				},
			],
		};
		self.repository.save_artifact(artifact.clone())?;
		self.repository
			.save_content(&artifact.uri, result.payload.clone())?;
		Ok(artifact)
	}

	pub fn load_artifact(
		&self,
		artifact_id: &ArtifactId,
	) -> Result<Option<Artifact>, ArtifactStoreError> {
		self.repository.load_artifact(artifact_id)
	}

	pub fn load_by_uri(&self, uri: &str) -> Result<Option<Artifact>, ArtifactStoreError> {
		self.repository.load_by_uri(uri)
	}

	pub fn list_by_task(&self, task_id: &TaskId) -> Result<Vec<Artifact>, ArtifactStoreError> {
		self.repository.list_by_task(task_id)
	}

	pub fn load_content_by_uri(&self, uri: &str) -> Result<Option<String>, ArtifactStoreError> {
		self.repository.load_content_by_uri(uri)
	}
}

impl Default for ArtifactStore {
	fn default() -> Self {
		Self::in_memory()
	}
}

fn result_artifact_uri(task_id: &TaskId, node_id: &roku_common_types::NodeId) -> String {
	format!("artifact://{}/{}", task_id.0, node_id.0)
}

fn payload_checksum(payload: &str) -> String {
	format!("bytes:{}", payload.len())
}

#[cfg(test)]
mod tests {
	use std::time::{SystemTime, UNIX_EPOCH};

	use roku_common_types::{EvidenceItem, NodeId, ResultStatus, TaskId};

	use super::*;

	fn sample_result() -> ResultEnvelope {
		ResultEnvelope {
			task_id: TaskId("task-1".to_string()),
			node_id: NodeId("node-1".to_string()),
			producer: "agent-1".to_string(),
			schema_version: "result.v1".to_string(),
			status: ResultStatus::Ok,
			payload: "payload".to_string(),
			evidence: vec![EvidenceItem {
				kind: "runtime".to_string(),
				value: "generic".to_string(),
			}],
			confidence: 0.9,
		}
	}

	fn temp_path(suffix: &str) -> std::path::PathBuf {
		let nanos = SystemTime::now()
			.duration_since(UNIX_EPOCH)
			.expect("clock should be after epoch")
			.as_nanos();
		std::env::temp_dir().join(format!("roku-artifact-{suffix}-{nanos}"))
	}

	#[test]
	fn persist_and_load_result_artifact_in_memory() {
		let mut store = ArtifactStore::default();
		let artifact = store
			.persist_result_artifact(&sample_result())
			.expect("artifact persistence should succeed");

		let loaded = store
			.load_by_uri(&artifact.uri)
			.expect("artifact lookup should succeed")
			.expect("artifact should exist");
		assert_eq!(loaded.artifact_id, artifact.artifact_id);
		assert_eq!(loaded.checksum, "bytes:7");
		let content = store
			.load_content_by_uri(&artifact.uri)
			.expect("artifact content should load")
			.expect("artifact content should exist");
		assert_eq!(content, "payload");
	}

	#[test]
	fn persist_and_load_result_artifact_file_backed() {
		let path = temp_path("store");
		let mut store = ArtifactStore::file_backed(path.clone());
		let artifact = store
			.persist_result_artifact(&sample_result())
			.expect("artifact persistence should succeed");

		let reloaded = ArtifactStore::file_backed(path.clone());
		let loaded = reloaded
			.load_artifact(&artifact.artifact_id)
			.expect("artifact load should succeed")
			.expect("artifact should exist");
		assert_eq!(loaded.uri, artifact.uri);
		let content = reloaded
			.load_content_by_uri(&artifact.uri)
			.expect("artifact content should load")
			.expect("artifact content should exist");
		assert_eq!(content, "payload");

		let _ = std::fs::remove_dir_all(path);
	}
}
