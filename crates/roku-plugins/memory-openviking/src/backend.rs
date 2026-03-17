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

//! OpenViking-backed implementation of Roku's long-term memory contract.
//!
//! This module is deliberately adapter-shaped: it translates provider-neutral
//! query and write requests into OpenViking HTTP calls, normalizes provider
//! responses back into Roku memory records, and surfaces provider health/errors.
//! It does not decide when recall occurs or what runtime should persist.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::StatusCode;
use reqwest::Url;
use reqwest::blocking::{Client, RequestBuilder};
use roku_common_types::ConversationTurn;
use roku_memory::{
	LongTermMemoryBackend, MemoryBackendHealth, MemoryBackendStatus, MemoryDeleteSelector,
	MemoryError, MemoryHit, MemoryKind, MemoryMetadata, MemoryProvenance, MemoryQuery,
	MemoryRecord, MemoryScope, MemorySourceRef, MemoryWriteAck, MemoryWriteReason,
	MemoryWriteRequest, PendingLoopSnapshot, PendingLoopSnapshotBackend, PendingLoopSnapshotError,
	SessionState, SessionStateBackend, SessionStateError, ShortTermContinuityBackend,
	ShortTermContinuityError,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::config::{OpenVikingBackendConfig, OpenVikingBackendConfigError};

static NEXT_RECORD_COUNTER: AtomicU64 = AtomicU64::new(1);
static NEXT_CONTINUITY_RECORD_COUNTER: AtomicU64 = AtomicU64::new(1);

/// OpenViking adapter that implements Roku's provider-neutral memory backend trait.
///
/// The adapter owns only provider-facing concerns: HTTP transport, request/response
/// mapping, staging local markdown files for ingestion, and basic health/error
/// normalization. Runtime policy stays upstream in Roku.
pub struct OpenVikingLongTermMemoryBackend {
	client: Client,
	config: OpenVikingBackendConfig,
}

/// Errors produced while constructing an [`OpenVikingLongTermMemoryBackend`].
#[derive(Debug, Error)]
pub enum OpenVikingBackendBootstrapError {
	#[error(transparent)]
	InvalidConfig(#[from] OpenVikingBackendConfigError),
	#[error("failed to build OpenViking HTTP client: {0}")]
	BuildClient(String),
}

impl std::fmt::Debug for OpenVikingLongTermMemoryBackend {
	fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		formatter
			.debug_struct("OpenVikingLongTermMemoryBackend")
			.field("base_url", &self.config.base_url)
			.field("resource_root_uri", &self.config.resource_root_uri)
			.field("staging_dir", &self.config.staging_dir)
			.field("strict", &self.config.strict)
			.finish()
	}
}

impl Clone for OpenVikingLongTermMemoryBackend {
	fn clone(&self) -> Self {
		Self {
			client: self.client.clone(),
			config: self.config.clone(),
		}
	}
}

impl OpenVikingLongTermMemoryBackend {
	/// Builds a new OpenViking adapter from validated typed config.
	///
	/// The constructor also installs the optional API key into the default request
	/// headers for subsequent backend calls.
	pub fn new(config: OpenVikingBackendConfig) -> Result<Self, OpenVikingBackendBootstrapError> {
		config.validate()?;
		let mut headers = reqwest::header::HeaderMap::new();
		if let Some(api_key) = config.api_key.clone() {
			let header_value = reqwest::header::HeaderValue::from_str(api_key.trim())
				.map_err(|error| OpenVikingBackendBootstrapError::BuildClient(error.to_string()))?;
			headers.insert("X-API-Key", header_value);
		}
		let client = Client::builder()
			.connect_timeout(Duration::from_millis(config.connect_timeout_ms))
			.timeout(Duration::from_millis(config.request_timeout_ms))
			.default_headers(headers)
			.build()
			.map_err(|error| OpenVikingBackendBootstrapError::BuildClient(error.to_string()))?;

		Ok(Self { client, config })
	}

	/// Resolves a provider-relative path against the configured base URL.
	fn endpoint(&self, path: &str) -> String {
		format!("{}{}", self.config.base_url.trim_end_matches('/'), path)
	}

	/// Sends a request and decodes OpenViking's `{ status, result, error }` envelope.
	fn send_json<T>(&self, request: RequestBuilder) -> Result<T, MemoryError>
	where
		T: DeserializeOwned,
	{
		let response = request.send().map_err(map_transport_error)?;
		parse_json_response(response)
	}

	/// Computes the OpenViking root URI to search for a recall query.
	///
	/// When the query narrows to a single memory kind, the adapter searches that
	/// subtree directly so provider-side retrieval stays as tight as possible.
	fn scope_root_uri_for_query(&self, query: &MemoryQuery) -> Result<String, MemoryError> {
		let mut root = scope_root_uri(
			&self.config.resource_root_uri,
			query.scope,
			query.session_id.as_deref(),
			query.user_id.as_deref(),
			query.project_id.as_deref(),
			query.workspace_id.as_deref(),
		)?;
		if query.filters.kinds.len() == 1 {
			root = format!("{}/{}", root, memory_kind_segment(query.filters.kinds[0]));
		}
		Ok(root)
	}

	/// Computes the storage subtree used for a new memory record.
	fn scope_root_uri_for_write(
		&self,
		request: &MemoryWriteRequest,
	) -> Result<String, MemoryError> {
		Ok(format!(
			"{}/{}",
			scope_root_uri(
				&self.config.resource_root_uri,
				request.scope,
				request.session_id.as_deref(),
				request.user_id.as_deref(),
				request.project_id.as_deref(),
				request.workspace_id.as_deref(),
			)?,
			memory_kind_segment(request.kind)
		))
	}

	/// Materializes a provider-ingestible markdown file for the pending write.
	///
	/// Local-file ingestion against a local OpenViking server makes the staged
	/// path part of the provider request.
	fn stage_memory_record(&self, request: &MemoryWriteRequest) -> Result<PathBuf, MemoryError> {
		let now_ms = unix_ms_now();
		let counter = NEXT_RECORD_COUNTER.fetch_add(1, Ordering::Relaxed);
		let file_name = format!("record-{now_ms}-{counter}.md");
		let path = stage_path(&self.config.staging_dir, request, &file_name);
		if let Some(parent) = path.parent() {
			fs::create_dir_all(parent).map_err(|error| MemoryError::Internal(error.to_string()))?;
		}
		fs::write(&path, render_memory_markdown(request)).map_err(|error| {
			MemoryError::Internal(format!("failed to write staged memory record: {error}"))
		})?;
		Ok(path)
	}

	/// Materializes a staged text resource under the adapter staging directory.
	fn stage_text_resource(
		&self,
		relative_path: &Path,
		content: &str,
	) -> Result<PathBuf, MemoryError> {
		let path = self.config.staging_dir.join(relative_path);
		if let Some(parent) = path.parent() {
			fs::create_dir_all(parent).map_err(|error| MemoryError::Internal(error.to_string()))?;
		}
		fs::write(&path, content).map_err(|error| {
			MemoryError::Internal(format!("failed to write staged resource: {error}"))
		})?;
		Ok(path)
	}

	/// Reads one text resource, returning `None` when the provider reports `NOT_FOUND`.
	fn read_text_resource(&self, uri: &str) -> Result<Option<String>, MemoryError> {
		let response = self
			.client
			.get(self.endpoint("/api/v1/content/read"))
			.query(&[("uri", uri), ("offset", "0"), ("limit", "-1")])
			.send()
			.map_err(map_transport_error)?;
		let status = response.status();
		let body = response
			.text()
			.map_err(|error| MemoryError::Internal(error.to_string()))?;
		if status == StatusCode::NOT_FOUND {
			return Ok(None);
		}
		if !status.is_success() {
			return Err(map_status_error(status, body));
		}
		parse_provider_result(&body).map(Some)
	}

	/// Lists one directory using OpenViking's simple listing mode.
	fn list_simple_entries(&self, uri: &str) -> Result<Vec<String>, MemoryError> {
		let response = self
			.client
			.get(self.endpoint("/api/v1/fs/ls"))
			.query(&[
				("uri", uri),
				("simple", "true"),
				("recursive", "false"),
				("output", "original"),
				("show_all_hidden", "false"),
				("node_limit", "1000"),
			])
			.send()
			.map_err(map_transport_error)?;
		let status = response.status();
		let body = response
			.text()
			.map_err(|error| MemoryError::Internal(error.to_string()))?;
		if status == StatusCode::NOT_FOUND {
			return Ok(Vec::new());
		}
		if !status.is_success() {
			return Err(map_status_error(status, body));
		}
		parse_provider_result(&body)
	}

	/// Returns whether a provider URI currently exists.
	fn resource_exists(&self, uri: &str) -> Result<bool, MemoryError> {
		let response = self
			.client
			.get(self.endpoint("/api/v1/fs/stat"))
			.query(&[("uri", uri)])
			.send()
			.map_err(map_transport_error)?;
		let status = response.status();
		let body = response
			.text()
			.map_err(|error| MemoryError::Internal(error.to_string()))?;
		if status == StatusCode::NOT_FOUND {
			return Ok(false);
		}
		if !status.is_success() {
			return Err(map_status_error(status, body));
		}
		let _: serde_json::Value = parse_provider_result(&body)?;
		Ok(true)
	}

	/// Removes one provider URI when it exists.
	fn remove_resource_if_exists(&self, uri: &str, recursive: bool) -> Result<(), MemoryError> {
		let response = self
			.client
			.delete(self.endpoint("/api/v1/fs"))
			.query(&[
				("uri", uri),
				("recursive", if recursive { "true" } else { "false" }),
			])
			.send()
			.map_err(map_transport_error)?;
		let status = response.status();
		let body = response
			.text()
			.map_err(|error| MemoryError::Internal(error.to_string()))?;
		if status == StatusCode::NOT_FOUND {
			return Ok(());
		}
		if !status.is_success() {
			return Err(map_status_error(status, body));
		}
		parse_empty_provider_result(&body)
	}

	/// Writes one text resource to a concrete provider URI.
	fn write_text_resource(
		&self,
		target_uri: &str,
		relative_stage_path: &Path,
		content: &str,
		reason: &str,
		instruction: &str,
	) -> Result<(), MemoryError> {
		if !server_accepts_local_paths(&self.config.base_url) {
			return Err(MemoryError::Rejected(
				"OpenViking runtime-state writes currently require a localhost server; remote temp_upload is not implemented yet".to_string(),
			));
		}
		let staged_path = self.stage_text_resource(relative_stage_path, content)?;
		if self.resource_exists(target_uri)? {
			self.remove_resource_if_exists(target_uri, false)?;
		}
		let _: AddResourceResultPayload =
			self.send_json(self.client.post(self.endpoint("/api/v1/resources")).json(
				&AddResourceRequestPayload {
					path: staged_path.display().to_string(),
					to: target_uri.to_string(),
					reason: reason.to_string(),
					instruction: instruction.to_string(),
					wait: true,
					timeout: Some(self.config.write_wait_timeout_ms as f64 / 1000.0),
					strict: self.config.strict,
					preserve_structure: Some(false),
				},
			))?;
		Ok(())
	}
}

impl LongTermMemoryBackend for OpenVikingLongTermMemoryBackend {
	fn backend_name(&self) -> &'static str {
		"openviking"
	}

	fn search(&self, query: &MemoryQuery) -> Result<Vec<MemoryHit>, MemoryError> {
		let target_uri = self.scope_root_uri_for_query(query)?;
		let response: FindResultPayload =
			self.send_json(self.client.post(self.endpoint("/api/v1/search/find")).json(
				&FindRequestPayload {
					query: query.query_text.clone(),
					target_uri,
					limit: query.limit.max(1),
					score_threshold: None,
				},
			))?;
		let mut hits = response.into_hits(&self.config.resource_root_uri, query);
		hits.sort_by(|left, right| right.score.total_cmp(&left.score));
		hits.truncate(query.limit.max(1));
		Ok(hits)
	}

	fn write(&self, request: &MemoryWriteRequest) -> Result<MemoryWriteAck, MemoryError> {
		if !server_accepts_local_paths(&self.config.base_url) {
			return Err(MemoryError::Rejected(
				"phase-4 OpenViking write support requires a localhost server; remote temp_upload is not implemented yet".to_string(),
			));
		}
		let staged_path = self.stage_memory_record(request)?;
		let scope_root_uri = self.scope_root_uri_for_write(request)?;
		let target_uri = format!(
			"{}/{}",
			scope_root_uri,
			staged_path
				.file_name()
				.and_then(|name| name.to_str())
				.ok_or_else(|| MemoryError::Internal("invalid staged file name".to_string()))?
		);
		let response: AddResourceResultPayload =
			self.send_json(self.client.post(self.endpoint("/api/v1/resources")).json(
				&AddResourceRequestPayload {
					path: staged_path.display().to_string(),
					to: target_uri.clone(),
					reason: format!(
						"roku-memory:{}",
						memory_write_reason_segment(request.write_reason)
					),
					instruction: "ingest Roku long-term memory record".to_string(),
					wait: false,
					timeout: None,
					strict: self.config.strict,
					preserve_structure: Some(false),
				},
			))?;
		let record_id = response.root_uri.unwrap_or(target_uri);
		Ok(MemoryWriteAck {
			accepted: true,
			record_id: Some(record_id),
		})
	}

	fn delete(&self, selector: &MemoryDeleteSelector) -> Result<(), MemoryError> {
		let response = self
			.client
			.delete(self.endpoint("/api/v1/fs"))
			.query(&[("uri", selector.record_id.as_str()), ("recursive", "true")])
			.send()
			.map_err(map_transport_error)?;
		parse_empty_response(response)
	}

	fn health(&self) -> Result<MemoryBackendHealth, MemoryError> {
		let response = self
			.client
			.get(self.endpoint("/health"))
			.send()
			.map_err(map_transport_error)?;
		let status = response.status();
		let body = response
			.text()
			.map_err(|error| MemoryError::Internal(error.to_string()))?;
		if !status.is_success() {
			return Err(map_status_error(status, body));
		}
		let payload = serde_json::from_str::<HealthPayload>(&body)
			.map_err(|error| MemoryError::Internal(error.to_string()))?;
		let is_healthy = payload
			.healthy
			.unwrap_or_else(|| payload.status.eq_ignore_ascii_case("ok"));
		let status = if is_healthy {
			MemoryBackendStatus::Healthy
		} else if payload.status.eq_ignore_ascii_case("ok") {
			MemoryBackendStatus::Degraded
		} else {
			MemoryBackendStatus::Unavailable
		};
		let detail = payload.version.map(|version| format!("version={version}"));
		Ok(MemoryBackendHealth {
			backend: self.backend_name().to_string(),
			status,
			detail,
		})
	}
}

/// Concrete OpenViking-backed adapter bundle for memory subsystem contracts.
pub struct OpenVikingMemoryAdapters {
	pub long_term: OpenVikingLongTermMemoryBackend,
	pub short_term: OpenVikingShortTermContinuityAdapter,
	pub session_state: OpenVikingSessionStateAdapter,
	pub pending_loop: OpenVikingPendingLoopSnapshotAdapter,
}

impl OpenVikingMemoryAdapters {
	/// Connects all currently implemented OpenViking-backed memory adapters.
	pub fn connect(
		config: OpenVikingBackendConfig,
	) -> Result<Self, OpenVikingBackendBootstrapError> {
		let backend = OpenVikingLongTermMemoryBackend::new(config)?;
		Ok(Self {
			long_term: backend.clone(),
			short_term: OpenVikingShortTermContinuityAdapter::new(backend.clone()),
			session_state: OpenVikingSessionStateAdapter::new(backend.clone()),
			pending_loop: OpenVikingPendingLoopSnapshotAdapter::new(backend),
		})
	}
}

/// OpenViking-backed session-state adapter.
#[derive(Debug, Clone)]
pub struct OpenVikingSessionStateAdapter {
	inner: OpenVikingLongTermMemoryBackend,
}

impl OpenVikingSessionStateAdapter {
	fn new(inner: OpenVikingLongTermMemoryBackend) -> Self {
		Self { inner }
	}
}

impl SessionStateBackend for OpenVikingSessionStateAdapter {
	fn save_session_state(
		&mut self,
		session_id: &str,
		state: SessionState,
	) -> Result<(), SessionStateError> {
		let target_uri =
			runtime_session_state_uri(&self.inner.config.resource_root_uri, session_id);
		let stage_path = runtime_session_state_stage_path(session_id);
		let content = serde_json::to_string_pretty(&state)
			.map_err(|error| SessionStateError::Backend(error.to_string()))?;
		self.inner
			.write_text_resource(
				&target_uri,
				&stage_path,
				&content,
				"roku-memory:session-state",
				"persist Roku session state",
			)
			.map_err(map_session_state_memory_error)
	}

	fn load_session_state(
		&self,
		session_id: &str,
	) -> Result<Option<SessionState>, SessionStateError> {
		let target_uri =
			runtime_session_state_uri(&self.inner.config.resource_root_uri, session_id);
		let content = self
			.inner
			.read_text_resource(&target_uri)
			.map_err(map_session_state_memory_error)?;
		content
			.map(|raw| {
				serde_json::from_str::<SessionState>(&raw)
					.map_err(|error| SessionStateError::Backend(error.to_string()))
			})
			.transpose()
	}

	fn delete_session_state(&mut self, session_id: &str) -> Result<(), SessionStateError> {
		let target_uri =
			runtime_session_state_uri(&self.inner.config.resource_root_uri, session_id);
		self.inner
			.remove_resource_if_exists(&target_uri, false)
			.map_err(map_session_state_memory_error)
	}
}

/// OpenViking-backed short-term continuity adapter.
#[derive(Debug, Clone)]
pub struct OpenVikingShortTermContinuityAdapter {
	inner: OpenVikingLongTermMemoryBackend,
}

impl OpenVikingShortTermContinuityAdapter {
	fn new(inner: OpenVikingLongTermMemoryBackend) -> Self {
		Self { inner }
	}
}

impl ShortTermContinuityBackend for OpenVikingShortTermContinuityAdapter {
	fn append_continuity_turn(
		&mut self,
		session_id: &str,
		turn: ConversationTurn,
	) -> Result<(), ShortTermContinuityError> {
		let created_at_unix_ms = if turn.created_at_unix_ms == 0 {
			unix_ms_now()
		} else {
			turn.created_at_unix_ms
		};
		let normalized_turn = ConversationTurn {
			created_at_unix_ms,
			..turn
		};
		let counter = NEXT_CONTINUITY_RECORD_COUNTER.fetch_add(1, Ordering::Relaxed);
		let file_name = continuity_file_name(created_at_unix_ms, counter);
		let target_uri = runtime_continuity_record_uri(
			&self.inner.config.resource_root_uri,
			session_id,
			&file_name,
		);
		let stage_path = runtime_continuity_stage_path(session_id, &file_name);
		let content = serde_json::to_string_pretty(&normalized_turn)
			.map_err(|error| ShortTermContinuityError::Backend(error.to_string()))?;
		self.inner
			.write_text_resource(
				&target_uri,
				&stage_path,
				&content,
				"roku-memory:short-term-continuity",
				"persist Roku short-term continuity turn",
			)
			.map_err(map_short_term_memory_error)
	}

	fn load_short_term_continuity(
		&self,
		session_id: &str,
		limit: usize,
	) -> Result<Vec<ConversationTurn>, ShortTermContinuityError> {
		if limit == 0 {
			return Ok(Vec::new());
		}
		let root_uri =
			runtime_continuity_root_uri(&self.inner.config.resource_root_uri, session_id);
		let mut entries = self
			.inner
			.list_simple_entries(&root_uri)
			.map_err(map_short_term_memory_error)?;
		entries.retain(|entry| entry.ends_with(".json"));
		entries.sort();
		let start = entries.len().saturating_sub(limit);
		let mut turns = Vec::with_capacity(entries.len().saturating_sub(start));
		for entry in entries.into_iter().skip(start) {
			let record_uri = runtime_continuity_record_uri(
				&self.inner.config.resource_root_uri,
				session_id,
				&entry,
			);
			let raw = self
				.inner
				.read_text_resource(&record_uri)
				.map_err(map_short_term_memory_error)?
				.ok_or_else(|| {
					ShortTermContinuityError::Backend(format!(
						"OpenViking continuity record disappeared while reading: {record_uri}"
					))
				})?;
			let turn = serde_json::from_str::<ConversationTurn>(&raw)
				.map_err(|error| ShortTermContinuityError::Backend(error.to_string()))?;
			turns.push(turn);
		}
		turns.sort_by_key(|turn| turn.created_at_unix_ms);
		Ok(turns)
	}

	fn delete_continuity(&mut self, session_id: &str) -> Result<(), ShortTermContinuityError> {
		let root_uri =
			runtime_continuity_root_uri(&self.inner.config.resource_root_uri, session_id);
		self.inner
			.remove_resource_if_exists(&root_uri, true)
			.map_err(map_short_term_memory_error)
	}
}

/// OpenViking-backed pending-loop snapshot adapter.
#[derive(Debug, Clone)]
pub struct OpenVikingPendingLoopSnapshotAdapter {
	inner: OpenVikingSessionStateAdapter,
}

impl OpenVikingPendingLoopSnapshotAdapter {
	fn new(inner: OpenVikingLongTermMemoryBackend) -> Self {
		Self {
			inner: OpenVikingSessionStateAdapter::new(inner),
		}
	}
}

impl PendingLoopSnapshotBackend for OpenVikingPendingLoopSnapshotAdapter {
	fn load_pending_loop_snapshot(
		&self,
		session_id: &str,
	) -> Result<Option<PendingLoopSnapshot>, PendingLoopSnapshotError> {
		self.inner
			.load_session_state(session_id)
			.map(|state| state.and_then(|value| value.pending_loop))
			.map_err(|error| PendingLoopSnapshotError::Backend(error.to_string()))
	}

	fn save_pending_loop_snapshot(
		&self,
		session_id: &str,
		snapshot: Option<PendingLoopSnapshot>,
	) -> Result<(), PendingLoopSnapshotError> {
		let mut session_state = self
			.inner
			.load_session_state(session_id)
			.map_err(|error| PendingLoopSnapshotError::Backend(error.to_string()))?
			.unwrap_or_default();
		session_state.pending_loop = snapshot;
		let mut adapter = self.inner.clone();
		adapter
			.save_session_state(session_id, session_state)
			.map_err(|error| PendingLoopSnapshotError::Backend(error.to_string()))
	}
}

/// OpenViking `search/find` request body for Roku recall.
#[derive(Debug, Serialize)]
struct FindRequestPayload {
	query: String,
	target_uri: String,
	limit: usize,
	#[serde(skip_serializing_if = "Option::is_none")]
	score_threshold: Option<f32>,
}

/// OpenViking `resources` ingestion request used for long-term memory writes.
#[derive(Debug, Serialize)]
struct AddResourceRequestPayload {
	path: String,
	to: String,
	reason: String,
	instruction: String,
	wait: bool,
	#[serde(skip_serializing_if = "Option::is_none")]
	timeout: Option<f64>,
	strict: bool,
	#[serde(skip_serializing_if = "Option::is_none")]
	preserve_structure: Option<bool>,
}

/// Common provider envelope returned by OpenViking JSON endpoints.
#[derive(Debug, Deserialize)]
struct ProviderEnvelope {
	status: String,
	#[serde(default)]
	result: Option<serde_json::Value>,
	#[serde(default)]
	error: Option<ProviderErrorPayload>,
}

/// Provider-side error details embedded inside [`ProviderEnvelope`].
#[derive(Debug, Deserialize)]
struct ProviderErrorPayload {
	#[serde(default)]
	code: Option<String>,
	#[serde(default)]
	message: Option<String>,
}

/// Search results returned by OpenViking's `find` endpoint.
#[derive(Debug, Default, Deserialize)]
struct FindResultPayload {
	#[serde(default)]
	memories: Vec<MatchedContextPayload>,
	#[serde(default)]
	resources: Vec<MatchedContextPayload>,
	#[serde(default)]
	skills: Vec<MatchedContextPayload>,
}

impl FindResultPayload {
	/// Projects provider search payloads into Roku memory hits.
	fn into_hits(self, resource_root_uri: &str, query: &MemoryQuery) -> Vec<MemoryHit> {
		self.memories
			.into_iter()
			.chain(self.resources)
			.chain(self.skills)
			.map(|context| matched_context_into_hit(context, resource_root_uri, query))
			.collect()
	}
}

/// Provider match payload shared across `memories`, `resources`, and `skills`.
#[derive(Debug, Deserialize)]
struct MatchedContextPayload {
	uri: String,
	#[serde(rename = "abstract", default)]
	abstract_text: String,
	#[serde(default)]
	overview: Option<String>,
	#[serde(default)]
	score: f32,
	#[serde(default)]
	match_reason: String,
}

/// Result payload returned by resource ingestion.
#[derive(Debug, Default, Deserialize)]
struct AddResourceResultPayload {
	#[serde(default)]
	root_uri: Option<String>,
}

/// Minimal health payload used by the adapter's readiness check.
#[derive(Debug, Deserialize)]
struct HealthPayload {
	status: String,
	#[serde(default)]
	healthy: Option<bool>,
	#[serde(default)]
	version: Option<String>,
}

/// Parses a successful JSON response that uses OpenViking's common envelope shape.
fn parse_json_response<T>(response: reqwest::blocking::Response) -> Result<T, MemoryError>
where
	T: DeserializeOwned,
{
	let status = response.status();
	let body = response
		.text()
		.map_err(|error| MemoryError::Internal(error.to_string()))?;
	if !status.is_success() {
		return Err(map_status_error(status, body));
	}
	parse_provider_result(&body)
}

/// Parses provider responses whose success case does not carry a JSON `result`.
fn parse_empty_response(response: reqwest::blocking::Response) -> Result<(), MemoryError> {
	let status = response.status();
	let body = response
		.text()
		.map_err(|error| MemoryError::Internal(error.to_string()))?;
	if !status.is_success() {
		return Err(map_status_error(status, body));
	}
	if body.trim().is_empty() {
		return Ok(());
	}
	parse_empty_provider_result(&body)
}

/// Parses a successful provider envelope body into the expected result payload.
fn parse_provider_result<T>(body: &str) -> Result<T, MemoryError>
where
	T: DeserializeOwned,
{
	serde_json::from_str::<ProviderEnvelope>(body)
		.map_err(|error| MemoryError::Internal(error.to_string()))
		.and_then(|payload| match payload.status.as_str() {
			"ok" => {
				let value = payload
					.result
					.ok_or_else(|| MemoryError::Internal("missing provider result".to_string()))?;
				serde_json::from_value::<T>(value)
					.map_err(|error| MemoryError::Internal(error.to_string()))
			}
			"error" => Err(map_provider_error(payload.error)),
			other => Err(MemoryError::Internal(format!(
				"unexpected provider status: {other}"
			))),
		})
}

/// Parses provider envelopes whose success case does not require a typed result payload.
fn parse_empty_provider_result(body: &str) -> Result<(), MemoryError> {
	let payload = serde_json::from_str::<ProviderEnvelope>(body)
		.map_err(|error| MemoryError::Internal(error.to_string()))?;
	if payload.status == "ok" {
		Ok(())
	} else {
		Err(map_provider_error(payload.error))
	}
}

/// Maps provider-specific error codes into Roku's backend-neutral error taxonomy.
fn map_provider_error(error: Option<ProviderErrorPayload>) -> MemoryError {
	let Some(error) = error else {
		return MemoryError::Internal("provider returned status=error without details".to_string());
	};
	let message = error
		.message
		.unwrap_or_else(|| "provider request failed".to_string());
	match error.code.as_deref() {
		Some("INVALID_ARGUMENT")
		| Some("INVALID_URI")
		| Some("NOT_FOUND")
		| Some("ALREADY_EXISTS")
		| Some("UNAUTHENTICATED")
		| Some("PERMISSION_DENIED") => MemoryError::Rejected(message),
		Some("UNAVAILABLE") | Some("DEADLINE_EXCEEDED") => MemoryError::Unavailable(message),
		_ => MemoryError::Internal(message),
	}
}

/// Maps raw HTTP failures into Roku's backend-neutral error taxonomy.
fn map_status_error(status: StatusCode, body: String) -> MemoryError {
	let detail = if body.trim().is_empty() {
		format!("http {}", status.as_u16())
	} else {
		format!("http {}: {}", status.as_u16(), body.trim())
	};
	match status {
		StatusCode::BAD_REQUEST
		| StatusCode::UNAUTHORIZED
		| StatusCode::FORBIDDEN
		| StatusCode::NOT_FOUND
		| StatusCode::CONFLICT => MemoryError::Rejected(detail),
		StatusCode::REQUEST_TIMEOUT
		| StatusCode::TOO_MANY_REQUESTS
		| StatusCode::BAD_GATEWAY
		| StatusCode::SERVICE_UNAVAILABLE
		| StatusCode::GATEWAY_TIMEOUT => MemoryError::Unavailable(detail),
		_ => MemoryError::Internal(detail),
	}
}

/// Converts reqwest transport failures into backend-neutral errors.
fn map_transport_error(error: reqwest::Error) -> MemoryError {
	if error.is_timeout() || error.is_connect() {
		return MemoryError::Unavailable(error.to_string());
	}
	MemoryError::Internal(error.to_string())
}

/// Projects a matched OpenViking context into Roku's canonical [`MemoryHit`] shape.
///
/// Provider URIs are normalized so that derived child resources such as
/// `.abstract.md` still point back to the canonical record id.
fn matched_context_into_hit(
	context: MatchedContextPayload,
	resource_root_uri: &str,
	query: &MemoryQuery,
) -> MemoryHit {
	let provider_locator = context.uri.clone();
	let canonical_record_id = canonical_record_uri(&provider_locator);
	let descriptor = parse_uri_descriptor(&context.uri, resource_root_uri).unwrap_or_else(|| {
		MemoryUriDescriptor::fallback(
			query.scope,
			query.session_id.clone(),
			query.user_id.clone(),
			query.project_id.clone(),
			query.workspace_id.clone(),
			query.filters.kinds.first().copied(),
		)
	});
	let summary = if context.abstract_text.trim().is_empty() {
		context
			.overview
			.clone()
			.unwrap_or_else(|| provider_locator.clone())
	} else {
		context.abstract_text.clone()
	};
	let content = context
		.overview
		.clone()
		.filter(|value| !value.trim().is_empty())
		.unwrap_or_else(|| summary.clone());
	MemoryHit {
		record: MemoryRecord {
			record_id: canonical_record_id.clone(),
			kind: descriptor.kind,
			scope: descriptor.scope,
			content,
			summary,
			source_refs: vec![
				MemorySourceRef {
					kind: "provider_locator".to_string(),
					value: provider_locator.clone(),
				},
				MemorySourceRef {
					kind: "canonical_record_id".to_string(),
					value: canonical_record_id,
				},
			],
			metadata: MemoryMetadata::default(),
			session_id: descriptor.session_id,
			user_id: descriptor.user_id,
			project_id: descriptor.project_id,
			workspace_id: descriptor.workspace_id,
			created_at_unix_ms: 0,
			updated_at_unix_ms: 0,
		},
		score: context.score,
		provenance: MemoryProvenance {
			backend: "openviking".to_string(),
			locator: Some(provider_locator),
			detail: if context.match_reason.trim().is_empty() {
				None
			} else {
				Some(context.match_reason)
			},
		},
	}
}

/// Normalizes provider child-resource URIs back to the canonical record URI.
fn canonical_record_uri(uri: &str) -> String {
	let trimmed = uri.trim_end_matches('/');
	let mut segments = trimmed.rsplitn(2, '/');
	let last_segment = segments.next().unwrap_or(trimmed);
	let parent_path = match segments.next() {
		Some(parent_path) => parent_path,
		None => return trimmed.to_string(),
	};
	if last_segment.starts_with('.') && last_segment.ends_with(".md") {
		return parent_path.to_string();
	}
	let parent_name = match parent_path.rsplit('/').next() {
		Some(parent_name) => parent_name,
		None => return trimmed.to_string(),
	};
	if last_segment == parent_name {
		parent_path.to_string()
	} else {
		trimmed.to_string()
	}
}

/// Returns whether the configured server can ingest local filesystem paths.
///
/// The current adapter write path stages markdown files locally and sends the path
/// directly to OpenViking, so non-local servers are rejected until a remote upload
/// flow is implemented.
fn server_accepts_local_paths(base_url: &str) -> bool {
	let Ok(url) = Url::parse(base_url) else {
		return false;
	};
	let Some(host) = url.host_str() else {
		return false;
	};
	matches!(host, "127.0.0.1" | "localhost" | "::1")
}

/// Builds the local staging path used for a pending memory write.
fn stage_path(root: &Path, request: &MemoryWriteRequest, file_name: &str) -> PathBuf {
	let mut path = root.to_path_buf();
	match request.scope {
		MemoryScope::Session => {
			path.push("session");
			path.push(sanitize_segment(
				request.session_id.as_deref().unwrap_or("unknown-session"),
			));
		}
		MemoryScope::User => {
			path.push("user");
			path.push(sanitize_segment(
				request.user_id.as_deref().unwrap_or("unknown-user"),
			));
		}
		MemoryScope::Project => {
			path.push("project");
			path.push(sanitize_segment(
				request.project_id.as_deref().unwrap_or("unknown-project"),
			));
		}
		MemoryScope::Workspace => {
			path.push("workspace");
			path.push(sanitize_segment(
				request
					.workspace_id
					.as_deref()
					.unwrap_or("unknown-workspace"),
			));
		}
		MemoryScope::Global => {
			path.push("global");
		}
	}
	path.push(memory_kind_segment(request.kind));
	path.push(file_name);
	path
}

/// Returns the OpenViking subtree reserved for Roku runtime-owned continuity/session state.
fn runtime_state_root_uri(resource_root_uri: &str) -> String {
	format!("{}/runtime-state", resource_root_uri.trim_end_matches('/'))
}

/// Returns the per-session runtime-state root.
fn runtime_session_root_uri(resource_root_uri: &str, session_id: &str) -> String {
	format!(
		"{}/session/{}",
		runtime_state_root_uri(resource_root_uri),
		sanitize_segment(session_id)
	)
}

/// Returns the provider URI used to store one session-state document.
fn runtime_session_state_uri(resource_root_uri: &str, session_id: &str) -> String {
	format!(
		"{}/session-state.json",
		runtime_session_root_uri(resource_root_uri, session_id)
	)
}

/// Returns the provider URI used to store continuity files for one session.
fn runtime_continuity_root_uri(resource_root_uri: &str, session_id: &str) -> String {
	format!(
		"{}/continuity",
		runtime_session_root_uri(resource_root_uri, session_id)
	)
}

/// Returns the provider URI for one continuity record file.
fn runtime_continuity_record_uri(
	resource_root_uri: &str,
	session_id: &str,
	file_name: &str,
) -> String {
	format!(
		"{}/{}",
		runtime_continuity_root_uri(resource_root_uri, session_id),
		file_name
	)
}

/// Returns the relative staging path used for one session-state document.
fn runtime_session_state_stage_path(session_id: &str) -> PathBuf {
	PathBuf::from("runtime-state")
		.join("session")
		.join(sanitize_segment(session_id))
		.join("session-state.json")
}

/// Returns the relative staging path used for one continuity record.
fn runtime_continuity_stage_path(session_id: &str, file_name: &str) -> PathBuf {
	PathBuf::from("runtime-state")
		.join("session")
		.join(sanitize_segment(session_id))
		.join("continuity")
		.join(file_name)
}

/// Returns a lexicographically sortable continuity file name.
fn continuity_file_name(created_at_unix_ms: u64, counter: u64) -> String {
	format!("turn-{created_at_unix_ms:020}-{counter:020}.json")
}

/// Renders a Roku memory record into the markdown format ingested by OpenViking.
fn render_memory_markdown(request: &MemoryWriteRequest) -> String {
	let mut lines = Vec::new();
	lines.push("# Roku Memory Record".to_string());
	lines.push(String::new());
	lines.push(format!("kind: {}", memory_kind_segment(request.kind)));
	lines.push(format!("scope: {}", memory_scope_segment(request.scope)));
	lines.push(format!(
		"write_reason: {}",
		memory_write_reason_segment(request.write_reason)
	));
	if let Some(session_id) = request.session_id.as_deref() {
		lines.push(format!("session_id: {session_id}"));
	}
	if let Some(user_id) = request.user_id.as_deref() {
		lines.push(format!("user_id: {user_id}"));
	}
	if let Some(project_id) = request.project_id.as_deref() {
		lines.push(format!("project_id: {project_id}"));
	}
	if let Some(workspace_id) = request.workspace_id.as_deref() {
		lines.push(format!("workspace_id: {workspace_id}"));
	}
	lines.push(String::new());
	lines.push("## Summary".to_string());
	lines.push(request.summary.clone());
	lines.push(String::new());
	lines.push("## Content".to_string());
	lines.push(request.content.clone());
	if !request.source_refs.is_empty() {
		lines.push(String::new());
		lines.push("## Source Refs".to_string());
		for source_ref in &request.source_refs {
			lines.push(format!("- {}: {}", source_ref.kind, source_ref.value));
		}
	}
	if !request.metadata.is_empty() {
		lines.push(String::new());
		lines.push("## Metadata".to_string());
		for (key, value) in &request.metadata {
			lines.push(format!("- {key}: {value}"));
		}
	}
	lines.join("\n")
}

/// Resolves the provider resource subtree that corresponds to a Roku memory scope.
fn scope_root_uri(
	resource_root_uri: &str,
	scope: MemoryScope,
	session_id: Option<&str>,
	user_id: Option<&str>,
	project_id: Option<&str>,
	workspace_id: Option<&str>,
) -> Result<String, MemoryError> {
	let root = resource_root_uri.trim_end_matches('/');
	match scope {
		MemoryScope::Session => {
			let session_id = session_id.ok_or_else(|| {
				MemoryError::Rejected("session-scoped memory requires session_id".to_string())
			})?;
			Ok(format!("{root}/session/{}", sanitize_segment(session_id)))
		}
		MemoryScope::User => {
			let user_id = user_id.ok_or_else(|| {
				MemoryError::Rejected("user-scoped memory requires user_id".to_string())
			})?;
			Ok(format!("{root}/user/{}", sanitize_segment(user_id)))
		}
		MemoryScope::Project => {
			let project_id = project_id.ok_or_else(|| {
				MemoryError::Rejected("project-scoped memory requires project_id".to_string())
			})?;
			Ok(format!("{root}/project/{}", sanitize_segment(project_id)))
		}
		MemoryScope::Workspace => {
			let workspace_id = workspace_id.ok_or_else(|| {
				MemoryError::Rejected("workspace-scoped memory requires workspace_id".to_string())
			})?;
			Ok(format!(
				"{root}/workspace/{}",
				sanitize_segment(workspace_id)
			))
		}
		MemoryScope::Global => Ok(format!("{root}/global")),
	}
}

/// Parses scope and kind back out of an OpenViking resource URI when possible.
fn parse_uri_descriptor(uri: &str, resource_root_uri: &str) -> Option<MemoryUriDescriptor> {
	let prefix = resource_root_uri.trim_end_matches('/');
	let remainder = uri.strip_prefix(prefix)?.trim_start_matches('/');
	let mut segments = remainder.split('/').filter(|segment| !segment.is_empty());
	let scope_segment = segments.next()?;
	let descriptor = match scope_segment {
		"session" => {
			let session_id = segments.next()?.to_string();
			let kind = parse_memory_kind_segment(segments.next()?)?;
			MemoryUriDescriptor {
				kind,
				scope: MemoryScope::Session,
				session_id: Some(session_id),
				user_id: None,
				project_id: None,
				workspace_id: None,
			}
		}
		"user" => {
			let user_id = segments.next()?.to_string();
			let kind = parse_memory_kind_segment(segments.next()?)?;
			MemoryUriDescriptor {
				kind,
				scope: MemoryScope::User,
				session_id: None,
				user_id: Some(user_id),
				project_id: None,
				workspace_id: None,
			}
		}
		"project" => {
			let project_id = segments.next()?.to_string();
			let kind = parse_memory_kind_segment(segments.next()?)?;
			MemoryUriDescriptor {
				kind,
				scope: MemoryScope::Project,
				session_id: None,
				user_id: None,
				project_id: Some(project_id),
				workspace_id: None,
			}
		}
		"workspace" => {
			let workspace_id = segments.next()?.to_string();
			let kind = parse_memory_kind_segment(segments.next()?)?;
			MemoryUriDescriptor {
				kind,
				scope: MemoryScope::Workspace,
				session_id: None,
				user_id: None,
				project_id: None,
				workspace_id: Some(workspace_id),
			}
		}
		"global" => MemoryUriDescriptor {
			kind: parse_memory_kind_segment(segments.next()?)?,
			scope: MemoryScope::Global,
			session_id: None,
			user_id: None,
			project_id: None,
			workspace_id: None,
		},
		_ => return None,
	};
	Some(descriptor)
}

/// Sanitizes user-controlled scope identifiers for filesystem and URI segments.
fn sanitize_segment(value: &str) -> String {
	let sanitized = value
		.chars()
		.map(|character| {
			if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
				character
			} else {
				'_'
			}
		})
		.collect::<String>();
	if sanitized.is_empty() {
		"unknown".to_string()
	} else {
		sanitized
	}
}

/// Returns the provider path segment for a Roku memory kind.
fn memory_kind_segment(kind: MemoryKind) -> &'static str {
	match kind {
		MemoryKind::UserPreference => "user_preference",
		MemoryKind::UserFact => "user_fact",
		MemoryKind::ProjectFact => "project_fact",
		MemoryKind::WorkspaceFact => "workspace_fact",
		MemoryKind::HistoricalCase => "historical_case",
		MemoryKind::Constraint => "constraint",
		MemoryKind::WorkflowInsight => "workflow_insight",
	}
}

/// Parses an OpenViking path segment back into a Roku memory kind.
fn parse_memory_kind_segment(segment: &str) -> Option<MemoryKind> {
	match segment {
		"user_preference" => Some(MemoryKind::UserPreference),
		"user_fact" => Some(MemoryKind::UserFact),
		"project_fact" => Some(MemoryKind::ProjectFact),
		"workspace_fact" => Some(MemoryKind::WorkspaceFact),
		"historical_case" => Some(MemoryKind::HistoricalCase),
		"constraint" => Some(MemoryKind::Constraint),
		"workflow_insight" => Some(MemoryKind::WorkflowInsight),
		_ => None,
	}
}

/// Returns the provider path segment for a Roku memory scope.
fn memory_scope_segment(scope: MemoryScope) -> &'static str {
	match scope {
		MemoryScope::Session => "session",
		MemoryScope::User => "user",
		MemoryScope::Project => "project",
		MemoryScope::Workspace => "workspace",
		MemoryScope::Global => "global",
	}
}

/// Returns the provider path segment for a Roku write reason.
fn memory_write_reason_segment(reason: MemoryWriteReason) -> &'static str {
	match reason {
		MemoryWriteReason::TaskSucceeded => "task_succeeded",
		MemoryWriteReason::HighValueObservation => "high_value_observation",
		MemoryWriteReason::OperatorRequested => "operator_requested",
	}
}

/// Returns the current Unix timestamp in milliseconds.
fn unix_ms_now() -> u64 {
	SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.unwrap_or_default()
		.as_millis() as u64
}

fn map_session_state_memory_error(error: MemoryError) -> SessionStateError {
	SessionStateError::Backend(error.to_string())
}

fn map_short_term_memory_error(error: MemoryError) -> ShortTermContinuityError {
	ShortTermContinuityError::Backend(error.to_string())
}

/// Parsed scope metadata extracted from a provider URI.
#[derive(Debug, Clone)]
struct MemoryUriDescriptor {
	kind: MemoryKind,
	scope: MemoryScope,
	session_id: Option<String>,
	user_id: Option<String>,
	project_id: Option<String>,
	workspace_id: Option<String>,
}

impl MemoryUriDescriptor {
	/// Falls back to query/request context when the provider URI is not parseable.
	fn fallback(
		scope: MemoryScope,
		session_id: Option<String>,
		user_id: Option<String>,
		project_id: Option<String>,
		workspace_id: Option<String>,
		kind: Option<MemoryKind>,
	) -> Self {
		Self {
			kind: kind.unwrap_or(MemoryKind::HistoricalCase),
			scope,
			session_id,
			user_id,
			project_id,
			workspace_id,
		}
	}
}

#[cfg(test)]
mod tests {
	use std::collections::{BTreeSet, HashMap};
	use std::sync::{Arc, Mutex};
	use std::thread;

	use super::{
		OpenVikingBackendConfig, OpenVikingMemoryAdapters, canonical_record_uri,
		memory_kind_segment, parse_memory_kind_segment, parse_uri_descriptor,
		render_memory_markdown, runtime_continuity_root_uri, runtime_state_root_uri,
		scope_root_uri, server_accepts_local_paths,
	};
	use reqwest::Url;
	use roku_common_types::{ConversationRole, ConversationTurn, PendingLoopBinding};
	use roku_memory::{
		MemoryKind, MemoryScope, MemoryWriteReason, MemoryWriteRequest, PendingLoopSnapshotBackend,
		SessionState, SessionStateBackend, ShortTermContinuityBackend,
	};
	use serde_json::json;
	use tiny_http::{Header, Method, Response, Server, StatusCode};

	struct MockOpenVikingServer {
		base_url: String,
		shutdown_url: String,
		handle: Option<std::thread::JoinHandle<()>>,
	}

	impl MockOpenVikingServer {
		fn start() -> Self {
			let server = Server::http("127.0.0.1:0").expect("mock server should bind");
			let base_url = format!("http://{}", server.server_addr());
			let shutdown_url = format!("{base_url}/__shutdown");
			let state = Arc::new(Mutex::new(HashMap::<String, String>::new()));
			let handle = {
				let state = Arc::clone(&state);
				thread::spawn(move || {
					for mut request in server.incoming_requests() {
						let parsed = Url::parse(&format!("http://localhost{}", request.url()))
							.expect("request url should parse");
						let path = parsed.path().to_string();
						if path == "/__shutdown" {
							respond_json(
								request,
								200,
								json!({"status":"ok","result":{"shutdown":true}}),
							);
							break;
						}
						match (request.method(), path.as_str()) {
							(&Method::Post, "/api/v1/resources") => {
								let mut body = String::new();
								request
									.as_reader()
									.read_to_string(&mut body)
									.expect("request body should read");
								let payload: serde_json::Value =
									serde_json::from_str(&body).expect("json body should parse");
								let path = payload
									.get("path")
									.and_then(serde_json::Value::as_str)
									.expect("path should exist");
								let target_uri = payload
									.get("to")
									.and_then(serde_json::Value::as_str)
									.expect("target uri should exist")
									.to_string();
								let content = std::fs::read_to_string(path)
									.expect("staged resource should exist");
								state
									.lock()
									.expect("state mutex")
									.insert(target_uri.clone(), content);
								respond_json(
									request,
									200,
									json!({"status":"ok","result":{"root_uri":target_uri}}),
								);
							}
							(&Method::Get, "/api/v1/content/read") => {
								let uri =
									query_value(&parsed, "uri").expect("uri query should exist");
								let maybe_content =
									state.lock().expect("state mutex").get(&uri).cloned();
								match maybe_content {
									Some(content) => respond_json(
										request,
										200,
										json!({"status":"ok","result":content}),
									),
									None => respond_json(
										request,
										404,
										json!({"status":"error","error":{"code":"NOT_FOUND","message":"missing resource"}}),
									),
								}
							}
							(&Method::Get, "/api/v1/fs/stat") => {
								let uri =
									query_value(&parsed, "uri").expect("uri query should exist");
								let exists = {
									let state = state.lock().expect("state mutex");
									let prefix = format!("{}/", uri.trim_end_matches('/'));
									state.contains_key(&uri)
										|| state.keys().any(|key| key.starts_with(&prefix))
								};
								if exists {
									respond_json(
										request,
										200,
										json!({"status":"ok","result":{"uri":uri}}),
									);
								} else {
									respond_json(
										request,
										404,
										json!({"status":"error","error":{"code":"NOT_FOUND","message":"missing resource"}}),
									);
								}
							}
							(&Method::Get, "/api/v1/fs/ls") => {
								let uri =
									query_value(&parsed, "uri").expect("uri query should exist");
								let prefix = format!("{}/", uri.trim_end_matches('/'));
								let entries = {
									let state = state.lock().expect("state mutex");
									let mut names = BTreeSet::new();
									for key in state.keys() {
										if let Some(remainder) = key.strip_prefix(&prefix)
											&& let Some(name) = remainder
												.split('/')
												.next()
												.filter(|value| !value.is_empty())
										{
											names.insert(name.to_string());
										}
									}
									names.into_iter().collect::<Vec<_>>()
								};
								if entries.is_empty() {
									respond_json(
										request,
										404,
										json!({"status":"error","error":{"code":"NOT_FOUND","message":"missing directory"}}),
									);
								} else {
									respond_json(
										request,
										200,
										json!({"status":"ok","result":entries}),
									);
								}
							}
							(&Method::Delete, "/api/v1/fs") => {
								let uri =
									query_value(&parsed, "uri").expect("uri query should exist");
								let recursive =
									query_value(&parsed, "recursive").as_deref() == Some("true");
								let removed = {
									let mut state = state.lock().expect("state mutex");
									if recursive {
										let prefix = format!("{}/", uri.trim_end_matches('/'));
										let before = state.len();
										state.retain(|key, _| {
											key != &uri && !key.starts_with(&prefix)
										});
										before != state.len()
									} else {
										state.remove(&uri).is_some()
									}
								};
								if removed {
									respond_json(
										request,
										200,
										json!({"status":"ok","result":{"uri":uri}}),
									);
								} else {
									respond_json(
										request,
										404,
										json!({"status":"error","error":{"code":"NOT_FOUND","message":"missing resource"}}),
									);
								}
							}
							_ => respond_json(
								request,
								404,
								json!({"status":"error","error":{"code":"NOT_FOUND","message":"unsupported route"}}),
							),
						}
					}
				})
			};

			Self {
				base_url,
				shutdown_url,
				handle: Some(handle),
			}
		}
	}

	impl Drop for MockOpenVikingServer {
		fn drop(&mut self) {
			let _ = reqwest::blocking::get(&self.shutdown_url);
			if let Some(handle) = self.handle.take() {
				handle.join().expect("mock server thread should stop");
			}
		}
	}

	fn query_value(url: &Url, key: &str) -> Option<String> {
		url.query_pairs()
			.find_map(|(name, value)| (name == key).then(|| value.into_owned()))
	}

	fn respond_json(request: tiny_http::Request, status: u16, body: serde_json::Value) {
		let response = Response::from_string(body.to_string())
			.with_status_code(StatusCode(status))
			.with_header(
				Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
					.expect("json header should build"),
			);
		request.respond(response).expect("response should send");
	}

	#[test]
	fn uri_descriptor_roundtrips_scope_and_kind() {
		let uri = "viking://resources/roku-memory/session/session-1/user_preference/record-1.md";
		let descriptor =
			parse_uri_descriptor(uri, "viking://resources/roku-memory").expect("uri should parse");
		assert_eq!(descriptor.scope, MemoryScope::Session);
		assert_eq!(descriptor.kind, MemoryKind::UserPreference);
		assert_eq!(descriptor.session_id.as_deref(), Some("session-1"));
	}

	#[test]
	fn scope_root_uri_requires_scope_identity() {
		let error = scope_root_uri(
			"viking://resources/roku-memory",
			MemoryScope::Session,
			None,
			None,
			None,
			None,
		)
		.expect_err("session scope should require session id");
		assert!(error.to_string().contains("session_id"));
	}

	#[test]
	fn rendered_markdown_keeps_summary_and_content_searchable() {
		let request = MemoryWriteRequest::new(
			MemoryKind::Constraint,
			MemoryScope::Global,
			"Always answer in concise Chinese.",
			"Chinese output constraint",
			MemoryWriteReason::OperatorRequested,
		);
		let rendered = render_memory_markdown(&request);
		assert!(rendered.contains("Chinese output constraint"));
		assert!(rendered.contains("Always answer in concise Chinese."));
	}

	#[test]
	fn kind_segments_roundtrip() {
		for kind in [
			MemoryKind::UserPreference,
			MemoryKind::UserFact,
			MemoryKind::ProjectFact,
			MemoryKind::WorkspaceFact,
			MemoryKind::HistoricalCase,
			MemoryKind::Constraint,
			MemoryKind::WorkflowInsight,
		] {
			assert_eq!(
				parse_memory_kind_segment(memory_kind_segment(kind)),
				Some(kind)
			);
		}
	}

	#[test]
	fn canonical_record_uri_normalizes_provider_child_resources() {
		assert_eq!(
			canonical_record_uri(
				"viking://resources/roku-memory/session/s1/user_preference/record-1.md/.abstract.md"
			),
			"viking://resources/roku-memory/session/s1/user_preference/record-1.md"
		);
		assert_eq!(
			canonical_record_uri(
				"viking://resources/roku-memory/session/s1/user_preference/record-1.md/.overview.md"
			),
			"viking://resources/roku-memory/session/s1/user_preference/record-1.md"
		);
		assert_eq!(
			canonical_record_uri(
				"viking://resources/roku-memory/session/s1/user_preference/record-1.md/record-1.md"
			),
			"viking://resources/roku-memory/session/s1/user_preference/record-1.md"
		);
	}

	#[test]
	fn remote_servers_are_rejected_for_local_path_ingest() {
		assert!(server_accepts_local_paths("http://127.0.0.1:1933"));
		assert!(server_accepts_local_paths("http://localhost:1933"));
		assert!(!server_accepts_local_paths("https://memory.example.com"));
	}

	#[test]
	fn runtime_state_uris_stay_outside_long_term_session_tree() {
		let long_term_root = scope_root_uri(
			"viking://resources/roku-memory",
			MemoryScope::Session,
			Some("session-1"),
			None,
			None,
			None,
		)
		.expect("session scope should resolve");
		let runtime_root = runtime_state_root_uri("viking://resources/roku-memory");
		let continuity_root =
			runtime_continuity_root_uri("viking://resources/roku-memory", "session-1");

		assert_ne!(runtime_root, long_term_root);
		assert!(!continuity_root.starts_with(&format!("{long_term_root}/")));
	}

	#[test]
	fn openviking_runtime_state_adapters_roundtrip_over_http() {
		let server = MockOpenVikingServer::start();
		let tempdir = tempfile::tempdir().expect("tempdir should exist");
		let adapters = OpenVikingMemoryAdapters::connect(OpenVikingBackendConfig {
			base_url: server.base_url.clone(),
			api_key: None,
			connect_timeout_ms: 1_000,
			request_timeout_ms: 1_000,
			resource_root_uri: "viking://resources/roku-memory".to_string(),
			staging_dir: tempdir.path().join("staging"),
			write_wait_timeout_ms: 5_000,
			strict: true,
		})
		.expect("openviking adapters should connect");

		let OpenVikingMemoryAdapters {
			mut session_state,
			mut short_term,
			pending_loop,
			..
		} = adapters;

		let saved_pending_loop = Some(PendingLoopBinding {
			run_id: "run-1".to_string(),
			loop_state_json: "{\"status\":\"waiting\"}".to_string(),
		});
		let state = SessionState {
			planning_mode: None,
			pending_loop: saved_pending_loop.clone(),
		};
		session_state
			.save_session_state("session-1", state.clone())
			.expect("session state should save");
		assert_eq!(
			session_state
				.load_session_state("session-1")
				.expect("session state should load"),
			Some(state.clone())
		);

		short_term
			.append_continuity_turn(
				"session-1",
				ConversationTurn {
					role: ConversationRole::User,
					content: "hello".to_string(),
					created_at_unix_ms: 1,
				},
			)
			.expect("first turn should save");
		short_term
			.append_continuity_turn(
				"session-1",
				ConversationTurn {
					role: ConversationRole::Assistant,
					content: "world".to_string(),
					created_at_unix_ms: 2,
				},
			)
			.expect("second turn should save");
		let turns = short_term
			.load_short_term_continuity("session-1", 8)
			.expect("continuity should load");
		assert_eq!(turns.len(), 2);
		assert_eq!(turns[0].content, "hello");
		assert_eq!(turns[1].content, "world");

		assert_eq!(
			pending_loop
				.load_pending_loop_snapshot("session-1")
				.expect("pending loop should load"),
			saved_pending_loop.clone()
		);
		pending_loop
			.save_pending_loop_snapshot("session-1", None)
			.expect("pending loop should clear");
		assert_eq!(
			session_state
				.load_session_state("session-1")
				.expect("session state should reload")
				.expect("session state should still exist")
				.pending_loop,
			None
		);

		short_term
			.delete_continuity("session-1")
			.expect("continuity should delete");
		assert!(
			short_term
				.load_short_term_continuity("session-1", 8)
				.expect("continuity should reload")
				.is_empty()
		);
		session_state
			.delete_session_state("session-1")
			.expect("session state should delete");
		assert!(
			session_state
				.load_session_state("session-1")
				.expect("session state should reload")
				.is_none()
		);
	}
}
