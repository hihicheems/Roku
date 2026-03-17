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

//! Typed configuration for the OpenViking memory adapter.

use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::str::FromStr;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

const HARD_MAX_MEMORY_CONNECT_TIMEOUT_MS: u64 = 30_000;
const HARD_MAX_MEMORY_REQUEST_TIMEOUT_MS: u64 = 120_000;
const HARD_MAX_OPENVIKING_EMBED_MAX_CONCURRENT: usize = 64;
const HARD_MAX_OPENVIKING_VLM_MAX_CONCURRENT: usize = 64;
const HARD_MAX_OPENVIKING_WRITE_WAIT_TIMEOUT_MS: u64 = 600_000;

const DEFAULT_OPENVIKING_CLIENT_BASE_URL: &str = "http://127.0.0.1:1933";
const DEFAULT_OPENVIKING_EMBEDDING_API_BASE: &str = "https://openrouter.ai/api/v1";
const DEFAULT_OPENVIKING_EMBEDDING_MODEL: &str = "thenlper/gte-base";
const DEFAULT_OPENVIKING_VLM_API_BASE: &str = "https://openrouter.ai/api/v1";
const DEFAULT_OPENVIKING_VLM_MODEL: &str = "qwen/qwen3.5-flash-02-23";
const OPENROUTER_API_KEY_LEGACY_ALIAS: &str = "OPENROUTER_API_KEY";
const GENERATED_OPENVIKING_LOG_LEVEL: &str = "INFO";
const GENERATED_OPENVIKING_LOG_OUTPUT: &str = "stdout";

/// Validated backend connection config for [`crate::OpenVikingLongTermMemoryBackend`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenVikingBackendConfig {
	/// Base HTTP endpoint for the OpenViking server, such as `http://127.0.0.1:1933`.
	pub base_url: String,
	/// Optional API key sent as `X-API-Key` when the server requires authentication.
	pub api_key: Option<String>,
	/// Connect timeout applied when establishing the HTTP connection.
	pub connect_timeout_ms: u64,
	/// End-to-end HTTP timeout for individual OpenViking requests.
	pub request_timeout_ms: u64,
	/// Root URI under which Roku-owned memory records are stored in OpenViking.
	pub resource_root_uri: String,
	/// Local staging directory used to materialize markdown records before upload.
	pub staging_dir: PathBuf,
	/// Timeout budget reserved for higher-level write completion probes.
	pub write_wait_timeout_ms: u64,
	/// Whether OpenViking should treat ingestion warnings as strict failures.
	pub strict: bool,
}

/// Adapter-owned runtime config subtree consumed from `runtime.memory.backends.openviking.*`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpenVikingRuntimeConfig {
	pub client: OpenVikingClientConfig,
	pub adapter: OpenVikingAdapterConfig,
	pub process: OpenVikingProcessConfig,
}

/// Partial runtime patch for [`OpenVikingRuntimeConfig`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenVikingRuntimeConfigPatch {
	#[serde(default)]
	pub client: Option<OpenVikingClientConfigPatch>,
	#[serde(default)]
	pub adapter: Option<OpenVikingAdapterConfigPatch>,
	#[serde(default)]
	pub process: Option<OpenVikingProcessConfigPatch>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenVikingClientConfig {
	pub base_url: String,
	pub connect_timeout_ms: u64,
	pub request_timeout_ms: u64,
	pub api_key: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenVikingClientConfigPatch {
	pub base_url: Option<String>,
	pub connect_timeout_ms: Option<u64>,
	pub request_timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenVikingAdapterConfig {
	pub resource_root_uri: String,
	pub staging_dir: PathBuf,
	pub write_wait_timeout_ms: u64,
	pub strict: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenVikingAdapterConfigPatch {
	pub resource_root_uri: Option<String>,
	pub staging_dir: Option<PathBuf>,
	pub write_wait_timeout_ms: Option<u64>,
	pub strict: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenVikingProcessConfig {
	pub managed: bool,
	pub config_output_path: PathBuf,
	pub storage: OpenVikingStorageConfig,
	pub server: OpenVikingServerConfig,
	pub embedding: OpenVikingEmbeddingConfig,
	pub vlm: OpenVikingVlmConfig,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenVikingProcessConfigPatch {
	pub managed: Option<bool>,
	pub config_output_path: Option<PathBuf>,
	#[serde(default)]
	pub storage: Option<OpenVikingStorageConfigPatch>,
	#[serde(default)]
	pub server: Option<OpenVikingServerConfigPatch>,
	#[serde(default)]
	pub embedding: Option<OpenVikingEmbeddingConfigPatch>,
	#[serde(default)]
	pub vlm: Option<OpenVikingVlmConfigPatch>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenVikingStorageConfig {
	pub workspace: PathBuf,
	pub vectordb_backend: OpenVikingStorageBackend,
	pub agfs_backend: OpenVikingStorageBackend,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenVikingStorageConfigPatch {
	pub workspace: Option<PathBuf>,
	pub vectordb_backend: Option<OpenVikingStorageBackend>,
	pub agfs_backend: Option<OpenVikingStorageBackend>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenVikingServerConfig {
	pub host: String,
	pub port: u16,
	pub root_api_key: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenVikingServerConfigPatch {
	pub host: Option<String>,
	pub port: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenVikingEmbeddingConfig {
	pub provider: OpenVikingEmbeddingProvider,
	pub api_base: String,
	pub api_key: Option<String>,
	pub model: String,
	pub dimension: u32,
	pub max_concurrent: usize,
	pub input: OpenVikingEmbeddingInput,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenVikingEmbeddingConfigPatch {
	pub provider: Option<OpenVikingEmbeddingProvider>,
	pub api_base: Option<String>,
	pub model: Option<String>,
	pub dimension: Option<u32>,
	pub max_concurrent: Option<usize>,
	pub input: Option<OpenVikingEmbeddingInput>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenVikingVlmConfig {
	pub provider: OpenVikingVlmProvider,
	pub api_base: String,
	pub api_key: Option<String>,
	pub model: String,
	pub max_concurrent: usize,
	pub thinking: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenVikingVlmConfigPatch {
	pub provider: Option<OpenVikingVlmProvider>,
	pub api_base: Option<String>,
	pub model: Option<String>,
	pub max_concurrent: Option<usize>,
	pub thinking: Option<bool>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OpenVikingStorageBackend {
	#[default]
	Local,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OpenVikingEmbeddingProvider {
	#[default]
	OpenAi,
	Volcengine,
	Jina,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OpenVikingVlmProvider {
	#[default]
	OpenAi,
	Volcengine,
	Litellm,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OpenVikingEmbeddingInput {
	#[default]
	Text,
}

/// Validation errors for [`OpenVikingBackendConfig`].
#[derive(Debug, Error)]
pub enum OpenVikingBackendConfigError {
	#[error("base_url cannot be empty")]
	EmptyBaseUrl,
	#[error("resource_root_uri cannot be empty")]
	EmptyResourceRootUri,
	#[error("staging_dir cannot be empty")]
	EmptyStagingDir,
	#[error("connect_timeout_ms must be greater than zero")]
	InvalidConnectTimeout,
	#[error("request_timeout_ms must be greater than zero")]
	InvalidRequestTimeout,
	#[error("write_wait_timeout_ms must be greater than zero")]
	InvalidWriteWaitTimeout,
}

/// Errors returned while parsing or materializing the OpenViking runtime config subtree.
#[derive(Debug, Error)]
pub enum OpenVikingRuntimeConfigError {
	#[error("invalid environment variable {key}: {message}")]
	InvalidEnv { key: &'static str, message: String },
	#[error("invalid runtime.memory.backends.openviking configuration for {field}: {message}")]
	InvalidConfig {
		field: &'static str,
		message: String,
	},
	#[error("failed to encode generated OpenViking config: {0}")]
	SerializeGeneratedConfig(String),
	#[error("failed to create generated OpenViking config directory {path}: {source}")]
	CreateGeneratedConfigDir {
		path: PathBuf,
		#[source]
		source: std::io::Error,
	},
	#[error("failed to write generated OpenViking config to {path}: {source}")]
	WriteGeneratedConfig {
		path: PathBuf,
		#[source]
		source: std::io::Error,
	},
}

impl Default for OpenVikingClientConfig {
	fn default() -> Self {
		Self {
			base_url: DEFAULT_OPENVIKING_CLIENT_BASE_URL.to_string(),
			connect_timeout_ms: 1_000,
			request_timeout_ms: 5_000,
			api_key: None,
		}
	}
}

impl Default for OpenVikingAdapterConfig {
	fn default() -> Self {
		Self {
			resource_root_uri: "viking://resources/roku-memory".to_string(),
			staging_dir: PathBuf::from(".roku").join("openviking").join("staging"),
			write_wait_timeout_ms: 120_000,
			strict: true,
		}
	}
}

impl Default for OpenVikingProcessConfig {
	fn default() -> Self {
		Self {
			managed: false,
			config_output_path: PathBuf::from(".roku")
				.join("run")
				.join("openviking")
				.join("ov.conf"),
			storage: OpenVikingStorageConfig::default(),
			server: OpenVikingServerConfig::default(),
			embedding: OpenVikingEmbeddingConfig::default(),
			vlm: OpenVikingVlmConfig::default(),
		}
	}
}

impl Default for OpenVikingStorageConfig {
	fn default() -> Self {
		Self {
			workspace: PathBuf::from(".roku").join("openviking").join("workspace"),
			vectordb_backend: OpenVikingStorageBackend::default(),
			agfs_backend: OpenVikingStorageBackend::default(),
		}
	}
}

impl Default for OpenVikingServerConfig {
	fn default() -> Self {
		Self {
			host: "127.0.0.1".to_string(),
			port: 1933,
			root_api_key: None,
		}
	}
}

impl Default for OpenVikingEmbeddingConfig {
	fn default() -> Self {
		Self {
			provider: OpenVikingEmbeddingProvider::default(),
			api_base: DEFAULT_OPENVIKING_EMBEDDING_API_BASE.to_string(),
			api_key: None,
			model: DEFAULT_OPENVIKING_EMBEDDING_MODEL.to_string(),
			dimension: 768,
			max_concurrent: 8,
			input: OpenVikingEmbeddingInput::default(),
		}
	}
}

impl Default for OpenVikingVlmConfig {
	fn default() -> Self {
		Self {
			provider: OpenVikingVlmProvider::default(),
			api_base: DEFAULT_OPENVIKING_VLM_API_BASE.to_string(),
			api_key: None,
			model: DEFAULT_OPENVIKING_VLM_MODEL.to_string(),
			max_concurrent: 16,
			thinking: false,
		}
	}
}

impl OpenVikingBackendConfig {
	/// Validates that required adapter settings are present and non-zero.
	pub fn validate(&self) -> Result<(), OpenVikingBackendConfigError> {
		if self.base_url.trim().is_empty() {
			return Err(OpenVikingBackendConfigError::EmptyBaseUrl);
		}
		if self.resource_root_uri.trim().is_empty() {
			return Err(OpenVikingBackendConfigError::EmptyResourceRootUri);
		}
		if self.staging_dir.as_os_str().is_empty() {
			return Err(OpenVikingBackendConfigError::EmptyStagingDir);
		}
		if self.connect_timeout_ms == 0 {
			return Err(OpenVikingBackendConfigError::InvalidConnectTimeout);
		}
		if self.request_timeout_ms == 0 {
			return Err(OpenVikingBackendConfigError::InvalidRequestTimeout);
		}
		if self.write_wait_timeout_ms == 0 {
			return Err(OpenVikingBackendConfigError::InvalidWriteWaitTimeout);
		}
		Ok(())
	}
}

impl OpenVikingRuntimeConfig {
	/// Applies a parsed runtime.toml patch.
	pub fn apply_patch(&mut self, patch: OpenVikingRuntimeConfigPatch) {
		if let Some(value) = patch.client {
			self.client.apply_patch(value);
		}
		if let Some(value) = patch.adapter {
			self.adapter.apply_patch(value);
		}
		if let Some(value) = patch.process {
			self.process.apply_patch(value);
		}
	}

	/// Applies canonical and legacy env overrides from the current process.
	pub fn apply_env_overrides(
		&mut self,
		memory_enabled: bool,
	) -> Result<(), OpenVikingRuntimeConfigError> {
		if let Some(value) = env_override_string_pair(
			"ROKU_RUNTIME__MEMORY__BACKENDS__OPENVIKING__CLIENT__BASE_URL",
			"ROKU_RUNTIME__MEMORY__OPENVIKING__CLIENT__BASE_URL",
		) {
			self.client.base_url = value;
		}
		if let Some(value) = env_override_u64_pair(
			"ROKU_RUNTIME__MEMORY__BACKENDS__OPENVIKING__CLIENT__CONNECT_TIMEOUT_MS",
			"ROKU_RUNTIME__MEMORY__OPENVIKING__CLIENT__CONNECT_TIMEOUT_MS",
		)? {
			self.client.connect_timeout_ms = value;
		}
		if let Some(value) = env_override_u64_pair(
			"ROKU_RUNTIME__MEMORY__BACKENDS__OPENVIKING__CLIENT__REQUEST_TIMEOUT_MS",
			"ROKU_RUNTIME__MEMORY__OPENVIKING__CLIENT__REQUEST_TIMEOUT_MS",
		)? {
			self.client.request_timeout_ms = value;
		}
		if let Some(value) = env_override_secret_pair(
			"ROKU_RUNTIME__MEMORY__BACKENDS__OPENVIKING__CLIENT__API_KEY",
			"ROKU_RUNTIME__MEMORY__OPENVIKING__CLIENT__API_KEY",
		) {
			self.client.api_key = Some(value);
		}
		if let Some(value) = env_override_string_pair(
			"ROKU_RUNTIME__MEMORY__BACKENDS__OPENVIKING__ADAPTER__RESOURCE_ROOT_URI",
			"ROKU_RUNTIME__MEMORY__OPENVIKING__ADAPTER__RESOURCE_ROOT_URI",
		) {
			self.adapter.resource_root_uri = value;
		}
		if let Some(value) = env_override_path_pair(
			"ROKU_RUNTIME__MEMORY__BACKENDS__OPENVIKING__ADAPTER__STAGING_DIR",
			"ROKU_RUNTIME__MEMORY__OPENVIKING__ADAPTER__STAGING_DIR",
		) {
			self.adapter.staging_dir = value;
		}
		if let Some(value) = env_override_u64_pair(
			"ROKU_RUNTIME__MEMORY__BACKENDS__OPENVIKING__ADAPTER__WRITE_WAIT_TIMEOUT_MS",
			"ROKU_RUNTIME__MEMORY__OPENVIKING__ADAPTER__WRITE_WAIT_TIMEOUT_MS",
		)? {
			self.adapter.write_wait_timeout_ms = value;
		}
		if let Some(value) = env_override_bool_pair(
			"ROKU_RUNTIME__MEMORY__BACKENDS__OPENVIKING__ADAPTER__STRICT",
			"ROKU_RUNTIME__MEMORY__OPENVIKING__ADAPTER__STRICT",
		)? {
			self.adapter.strict = value;
		}
		if let Some(value) = env_override_bool_pair(
			"ROKU_RUNTIME__MEMORY__BACKENDS__OPENVIKING__PROCESS__MANAGED",
			"ROKU_RUNTIME__MEMORY__OPENVIKING__PROCESS__MANAGED",
		)? {
			self.process.managed = value;
		}
		if let Some(value) = env_override_path_pair(
			"ROKU_RUNTIME__MEMORY__BACKENDS__OPENVIKING__PROCESS__CONFIG_OUTPUT_PATH",
			"ROKU_RUNTIME__MEMORY__OPENVIKING__PROCESS__CONFIG_OUTPUT_PATH",
		) {
			self.process.config_output_path = value;
		}
		if let Some(value) = env_override_path_pair(
			"ROKU_RUNTIME__MEMORY__BACKENDS__OPENVIKING__PROCESS__STORAGE__WORKSPACE",
			"ROKU_RUNTIME__MEMORY__OPENVIKING__PROCESS__STORAGE__WORKSPACE",
		) {
			self.process.storage.workspace = value;
		}
		if let Some(value) = env_override_enum_pair(
			"ROKU_RUNTIME__MEMORY__BACKENDS__OPENVIKING__PROCESS__STORAGE__VECTORDB_BACKEND",
			"ROKU_RUNTIME__MEMORY__OPENVIKING__PROCESS__STORAGE__VECTORDB_BACKEND",
		)? {
			self.process.storage.vectordb_backend = value;
		}
		if let Some(value) = env_override_enum_pair(
			"ROKU_RUNTIME__MEMORY__BACKENDS__OPENVIKING__PROCESS__STORAGE__AGFS_BACKEND",
			"ROKU_RUNTIME__MEMORY__OPENVIKING__PROCESS__STORAGE__AGFS_BACKEND",
		)? {
			self.process.storage.agfs_backend = value;
		}
		if let Some(value) = env_override_string_pair(
			"ROKU_RUNTIME__MEMORY__BACKENDS__OPENVIKING__PROCESS__SERVER__HOST",
			"ROKU_RUNTIME__MEMORY__OPENVIKING__PROCESS__SERVER__HOST",
		) {
			self.process.server.host = value;
		}
		if let Some(value) = env_override_u16_pair(
			"ROKU_RUNTIME__MEMORY__BACKENDS__OPENVIKING__PROCESS__SERVER__PORT",
			"ROKU_RUNTIME__MEMORY__OPENVIKING__PROCESS__SERVER__PORT",
		)? {
			self.process.server.port = value;
		}
		if let Some(value) = env_override_secret_pair(
			"ROKU_RUNTIME__MEMORY__BACKENDS__OPENVIKING__PROCESS__SERVER__ROOT_API_KEY",
			"ROKU_RUNTIME__MEMORY__OPENVIKING__PROCESS__SERVER__ROOT_API_KEY",
		) {
			self.process.server.root_api_key = Some(value);
		}
		if let Some(value) = env_override_enum_pair(
			"ROKU_RUNTIME__MEMORY__BACKENDS__OPENVIKING__PROCESS__EMBEDDING__PROVIDER",
			"ROKU_RUNTIME__MEMORY__OPENVIKING__PROCESS__EMBEDDING__PROVIDER",
		)? {
			self.process.embedding.provider = value;
		}
		if let Some(value) = env_override_string_pair(
			"ROKU_RUNTIME__MEMORY__BACKENDS__OPENVIKING__PROCESS__EMBEDDING__API_BASE",
			"ROKU_RUNTIME__MEMORY__OPENVIKING__PROCESS__EMBEDDING__API_BASE",
		) {
			self.process.embedding.api_base = value;
		}
		if let Some(value) = env_override_string_pair(
			"ROKU_RUNTIME__MEMORY__BACKENDS__OPENVIKING__PROCESS__EMBEDDING__MODEL",
			"ROKU_RUNTIME__MEMORY__OPENVIKING__PROCESS__EMBEDDING__MODEL",
		) {
			self.process.embedding.model = value;
		}
		if let Some(value) = env_override_u32_pair(
			"ROKU_RUNTIME__MEMORY__BACKENDS__OPENVIKING__PROCESS__EMBEDDING__DIMENSION",
			"ROKU_RUNTIME__MEMORY__OPENVIKING__PROCESS__EMBEDDING__DIMENSION",
		)? {
			self.process.embedding.dimension = value;
		}
		if let Some(value) = env_override_usize_pair(
			"ROKU_RUNTIME__MEMORY__BACKENDS__OPENVIKING__PROCESS__EMBEDDING__MAX_CONCURRENT",
			"ROKU_RUNTIME__MEMORY__OPENVIKING__PROCESS__EMBEDDING__MAX_CONCURRENT",
		)? {
			self.process.embedding.max_concurrent = value;
		}
		if let Some(value) = env_override_enum_pair(
			"ROKU_RUNTIME__MEMORY__BACKENDS__OPENVIKING__PROCESS__EMBEDDING__INPUT",
			"ROKU_RUNTIME__MEMORY__OPENVIKING__PROCESS__EMBEDDING__INPUT",
		)? {
			self.process.embedding.input = value;
		}
		if let Some(value) = env_override_enum_pair(
			"ROKU_RUNTIME__MEMORY__BACKENDS__OPENVIKING__PROCESS__VLM__PROVIDER",
			"ROKU_RUNTIME__MEMORY__OPENVIKING__PROCESS__VLM__PROVIDER",
		)? {
			self.process.vlm.provider = value;
		}
		if let Some(value) = env_override_string_pair(
			"ROKU_RUNTIME__MEMORY__BACKENDS__OPENVIKING__PROCESS__VLM__API_BASE",
			"ROKU_RUNTIME__MEMORY__OPENVIKING__PROCESS__VLM__API_BASE",
		) {
			self.process.vlm.api_base = value;
		}
		if let Some(value) = env_override_string_pair(
			"ROKU_RUNTIME__MEMORY__BACKENDS__OPENVIKING__PROCESS__VLM__MODEL",
			"ROKU_RUNTIME__MEMORY__OPENVIKING__PROCESS__VLM__MODEL",
		) {
			self.process.vlm.model = value;
		}
		if let Some(value) = env_override_usize_pair(
			"ROKU_RUNTIME__MEMORY__BACKENDS__OPENVIKING__PROCESS__VLM__MAX_CONCURRENT",
			"ROKU_RUNTIME__MEMORY__OPENVIKING__PROCESS__VLM__MAX_CONCURRENT",
		)? {
			self.process.vlm.max_concurrent = value;
		}
		if let Some(value) = env_override_bool_pair(
			"ROKU_RUNTIME__MEMORY__BACKENDS__OPENVIKING__PROCESS__VLM__THINKING",
			"ROKU_RUNTIME__MEMORY__OPENVIKING__PROCESS__VLM__THINKING",
		)? {
			self.process.vlm.thinking = value;
		}
		if memory_enabled || self.process.managed {
			if let Some(value) = env_override_secret_with_alias_pair(
				"ROKU_RUNTIME__MEMORY__BACKENDS__OPENVIKING__PROCESS__EMBEDDING__API_KEY",
				"ROKU_RUNTIME__MEMORY__OPENVIKING__PROCESS__EMBEDDING__API_KEY",
				OPENROUTER_API_KEY_LEGACY_ALIAS,
			) {
				self.process.embedding.api_key = Some(value);
			}
			if let Some(value) = env_override_secret_with_alias_pair(
				"ROKU_RUNTIME__MEMORY__BACKENDS__OPENVIKING__PROCESS__VLM__API_KEY",
				"ROKU_RUNTIME__MEMORY__OPENVIKING__PROCESS__VLM__API_KEY",
				OPENROUTER_API_KEY_LEGACY_ALIAS,
			) {
				self.process.vlm.api_key = Some(value);
			}
		}
		Ok(())
	}

	/// Validates, trims, and clamps provider-specific config.
	pub fn validate_and_clamp(&mut self) -> Result<(), OpenVikingRuntimeConfigError> {
		self.client.base_url = self.client.base_url.trim().to_string();
		if self.client.base_url.is_empty() {
			return Err(invalid_config("client.base_url", "value cannot be empty"));
		}
		if self.client.connect_timeout_ms == 0 {
			return Err(invalid_config(
				"client.connect_timeout_ms",
				"value must be greater than zero",
			));
		}
		if self.client.request_timeout_ms == 0 {
			return Err(invalid_config(
				"client.request_timeout_ms",
				"value must be greater than zero",
			));
		}
		self.client.connect_timeout_ms = self
			.client
			.connect_timeout_ms
			.min(HARD_MAX_MEMORY_CONNECT_TIMEOUT_MS);
		self.client.request_timeout_ms = self
			.client
			.request_timeout_ms
			.min(HARD_MAX_MEMORY_REQUEST_TIMEOUT_MS);
		self.client.api_key = trim_optional_secret(self.client.api_key.take());

		self.adapter.resource_root_uri = self
			.adapter
			.resource_root_uri
			.trim()
			.trim_end_matches('/')
			.to_string();
		if self.adapter.resource_root_uri.is_empty() {
			return Err(invalid_config(
				"adapter.resource_root_uri",
				"value cannot be empty",
			));
		}
		if self.adapter.staging_dir.as_os_str().is_empty() {
			return Err(invalid_config(
				"adapter.staging_dir",
				"value cannot be empty",
			));
		}
		if self.adapter.write_wait_timeout_ms == 0 {
			return Err(invalid_config(
				"adapter.write_wait_timeout_ms",
				"value must be greater than zero",
			));
		}
		self.adapter.write_wait_timeout_ms = self
			.adapter
			.write_wait_timeout_ms
			.min(HARD_MAX_OPENVIKING_WRITE_WAIT_TIMEOUT_MS);

		if self.process.config_output_path.as_os_str().is_empty() {
			return Err(invalid_config(
				"process.config_output_path",
				"value cannot be empty",
			));
		}
		if self.process.storage.workspace.as_os_str().is_empty() {
			return Err(invalid_config(
				"process.storage.workspace",
				"value cannot be empty",
			));
		}

		self.process.server.host = self.process.server.host.trim().to_string();
		if self.process.server.host.is_empty() {
			return Err(invalid_config(
				"process.server.host",
				"value cannot be empty",
			));
		}
		if self.process.server.port == 0 {
			return Err(invalid_config(
				"process.server.port",
				"value must be greater than zero",
			));
		}
		self.process.server.root_api_key =
			trim_optional_secret(self.process.server.root_api_key.take());

		self.process.embedding.api_base = self.process.embedding.api_base.trim().to_string();
		self.process.embedding.model = self.process.embedding.model.trim().to_string();
		if self.process.embedding.api_base.is_empty() {
			return Err(invalid_config(
				"process.embedding.api_base",
				"value cannot be empty",
			));
		}
		if self.process.embedding.model.is_empty() {
			return Err(invalid_config(
				"process.embedding.model",
				"value cannot be empty",
			));
		}
		if self.process.embedding.dimension == 0 {
			return Err(invalid_config(
				"process.embedding.dimension",
				"value must be greater than zero",
			));
		}
		if self.process.embedding.max_concurrent == 0 {
			return Err(invalid_config(
				"process.embedding.max_concurrent",
				"value must be greater than zero",
			));
		}
		self.process.embedding.max_concurrent = self
			.process
			.embedding
			.max_concurrent
			.min(HARD_MAX_OPENVIKING_EMBED_MAX_CONCURRENT);
		self.process.embedding.api_key =
			trim_optional_secret(self.process.embedding.api_key.take());

		self.process.vlm.api_base = self.process.vlm.api_base.trim().to_string();
		self.process.vlm.model = self.process.vlm.model.trim().to_string();
		if self.process.vlm.api_base.is_empty() {
			return Err(invalid_config(
				"process.vlm.api_base",
				"value cannot be empty",
			));
		}
		if self.process.vlm.model.is_empty() {
			return Err(invalid_config("process.vlm.model", "value cannot be empty"));
		}
		if self.process.vlm.max_concurrent == 0 {
			return Err(invalid_config(
				"process.vlm.max_concurrent",
				"value must be greater than zero",
			));
		}
		self.process.vlm.max_concurrent = self
			.process
			.vlm
			.max_concurrent
			.min(HARD_MAX_OPENVIKING_VLM_MAX_CONCURRENT);
		self.process.vlm.api_key = trim_optional_secret(self.process.vlm.api_key.take());

		Ok(())
	}

	/// Builds the validated long-term backend config from the runtime subtree.
	pub fn to_backend_config(&self) -> OpenVikingBackendConfig {
		OpenVikingBackendConfig {
			base_url: self.client.base_url.clone(),
			api_key: self.client.api_key.clone(),
			connect_timeout_ms: self.client.connect_timeout_ms,
			request_timeout_ms: self.client.request_timeout_ms,
			resource_root_uri: self.adapter.resource_root_uri.clone(),
			staging_dir: self.adapter.staging_dir.clone(),
			write_wait_timeout_ms: self.adapter.write_wait_timeout_ms,
			strict: self.adapter.strict,
		}
	}

	/// Returns a redacted config summary suitable for operator-facing reports.
	pub fn summary_json(&self) -> Value {
		json!({
			"client": {
				"base_url": self.client.base_url.clone(),
			},
			"adapter": {
				"resource_root_uri": self.adapter.resource_root_uri.clone(),
				"staging_dir": self.adapter.staging_dir.display().to_string(),
				"write_wait_timeout_ms": self.adapter.write_wait_timeout_ms,
				"strict": self.adapter.strict,
			},
			"process": {
				"managed": self.process.managed,
			},
		})
	}

	/// Materializes a managed OpenViking config file when the current runtime
	/// selects OpenViking and managed bootstrap is enabled.
	pub fn materialize_generated_config(
		&self,
		selected_as_active_backend: bool,
	) -> Result<Option<PathBuf>, OpenVikingRuntimeConfigError> {
		if !(selected_as_active_backend && self.process.managed) {
			return Ok(None);
		}

		self.validate_managed_process_secrets()?;
		let output_path = resolve_path(&self.process.config_output_path);
		let rendered = self.render_openviking_process_config()?;
		if let Some(parent) = output_path.parent() {
			fs::create_dir_all(parent).map_err(|source| {
				OpenVikingRuntimeConfigError::CreateGeneratedConfigDir {
					path: parent.to_path_buf(),
					source,
				}
			})?;
		}
		write_generated_config(&output_path, &rendered)?;
		Ok(Some(output_path))
	}

	fn validate_managed_process_secrets(&self) -> Result<(), OpenVikingRuntimeConfigError> {
		if self.process.embedding.api_key.is_none() {
			return Err(invalid_config(
				"process.embedding.api_key",
				"value is required when managed OpenViking process config generation is enabled",
			));
		}
		if self.process.vlm.api_key.is_none() {
			return Err(invalid_config(
				"process.vlm.api_key",
				"value is required when managed OpenViking process config generation is enabled",
			));
		}
		Ok(())
	}

	fn render_openviking_process_config(&self) -> Result<String, OpenVikingRuntimeConfigError> {
		let file_config = OpenVikingFileConfig {
			storage: OpenVikingFileStorageConfig {
				workspace: resolve_path(&self.process.storage.workspace),
			},
			log: OpenVikingFileLogConfig {
				level: GENERATED_OPENVIKING_LOG_LEVEL.to_string(),
				output: GENERATED_OPENVIKING_LOG_OUTPUT.to_string(),
			},
			embedding: OpenVikingFileEmbeddingSection {
				dense: OpenVikingFileEmbeddingDenseConfig {
					provider: self.process.embedding.provider,
					api_base: self.process.embedding.api_base.clone(),
					api_key: self
						.process
						.embedding
						.api_key
						.clone()
						.expect("embedding key checked before rendering"),
					model: self.process.embedding.model.clone(),
					dimension: self.process.embedding.dimension,
					input: self.process.embedding.input,
				},
				max_concurrent: self.process.embedding.max_concurrent,
			},
			vlm: OpenVikingFileVlmConfig {
				provider: self.process.vlm.provider,
				api_base: self.process.vlm.api_base.clone(),
				api_key: self
					.process
					.vlm
					.api_key
					.clone()
					.expect("vlm key checked before rendering"),
				model: self.process.vlm.model.clone(),
				max_concurrent: self.process.vlm.max_concurrent,
				thinking: self.process.vlm.thinking,
			},
		};
		serde_json::to_string_pretty(&file_config).map_err(|error| {
			OpenVikingRuntimeConfigError::SerializeGeneratedConfig(error.to_string())
		})
	}
}

impl OpenVikingClientConfig {
	fn apply_patch(&mut self, patch: OpenVikingClientConfigPatch) {
		if let Some(value) = patch.base_url {
			self.base_url = value;
		}
		if let Some(value) = patch.connect_timeout_ms {
			self.connect_timeout_ms = value;
		}
		if let Some(value) = patch.request_timeout_ms {
			self.request_timeout_ms = value;
		}
	}
}

impl OpenVikingAdapterConfig {
	fn apply_patch(&mut self, patch: OpenVikingAdapterConfigPatch) {
		if let Some(value) = patch.resource_root_uri {
			self.resource_root_uri = value;
		}
		if let Some(value) = patch.staging_dir {
			self.staging_dir = value;
		}
		if let Some(value) = patch.write_wait_timeout_ms {
			self.write_wait_timeout_ms = value;
		}
		if let Some(value) = patch.strict {
			self.strict = value;
		}
	}
}

impl OpenVikingProcessConfig {
	fn apply_patch(&mut self, patch: OpenVikingProcessConfigPatch) {
		if let Some(value) = patch.managed {
			self.managed = value;
		}
		if let Some(value) = patch.config_output_path {
			self.config_output_path = value;
		}
		if let Some(value) = patch.storage {
			self.storage.apply_patch(value);
		}
		if let Some(value) = patch.server {
			self.server.apply_patch(value);
		}
		if let Some(value) = patch.embedding {
			self.embedding.apply_patch(value);
		}
		if let Some(value) = patch.vlm {
			self.vlm.apply_patch(value);
		}
	}
}

impl OpenVikingStorageConfig {
	fn apply_patch(&mut self, patch: OpenVikingStorageConfigPatch) {
		if let Some(value) = patch.workspace {
			self.workspace = value;
		}
		if let Some(value) = patch.vectordb_backend {
			self.vectordb_backend = value;
		}
		if let Some(value) = patch.agfs_backend {
			self.agfs_backend = value;
		}
	}
}

impl OpenVikingServerConfig {
	fn apply_patch(&mut self, patch: OpenVikingServerConfigPatch) {
		if let Some(value) = patch.host {
			self.host = value;
		}
		if let Some(value) = patch.port {
			self.port = value;
		}
	}
}

impl OpenVikingEmbeddingConfig {
	fn apply_patch(&mut self, patch: OpenVikingEmbeddingConfigPatch) {
		if let Some(value) = patch.provider {
			self.provider = value;
		}
		if let Some(value) = patch.api_base {
			self.api_base = value;
		}
		if let Some(value) = patch.model {
			self.model = value;
		}
		if let Some(value) = patch.dimension {
			self.dimension = value;
		}
		if let Some(value) = patch.max_concurrent {
			self.max_concurrent = value;
		}
		if let Some(value) = patch.input {
			self.input = value;
		}
	}
}

impl OpenVikingVlmConfig {
	fn apply_patch(&mut self, patch: OpenVikingVlmConfigPatch) {
		if let Some(value) = patch.provider {
			self.provider = value;
		}
		if let Some(value) = patch.api_base {
			self.api_base = value;
		}
		if let Some(value) = patch.model {
			self.model = value;
		}
		if let Some(value) = patch.max_concurrent {
			self.max_concurrent = value;
		}
		if let Some(value) = patch.thinking {
			self.thinking = value;
		}
	}
}

impl FromStr for OpenVikingStorageBackend {
	type Err = &'static str;

	fn from_str(value: &str) -> Result<Self, Self::Err> {
		match value.trim().to_ascii_lowercase().as_str() {
			"local" => Ok(Self::Local),
			_ => Err("expected one of: local"),
		}
	}
}

impl FromStr for OpenVikingEmbeddingProvider {
	type Err = &'static str;

	fn from_str(value: &str) -> Result<Self, Self::Err> {
		match value.trim().to_ascii_lowercase().as_str() {
			"openai" => Ok(Self::OpenAi),
			"volcengine" => Ok(Self::Volcengine),
			"jina" => Ok(Self::Jina),
			_ => Err("expected one of: openai, volcengine, jina"),
		}
	}
}

impl FromStr for OpenVikingVlmProvider {
	type Err = &'static str;

	fn from_str(value: &str) -> Result<Self, Self::Err> {
		match value.trim().to_ascii_lowercase().as_str() {
			"openai" => Ok(Self::OpenAi),
			"volcengine" => Ok(Self::Volcengine),
			"litellm" => Ok(Self::Litellm),
			_ => Err("expected one of: openai, volcengine, litellm"),
		}
	}
}

impl FromStr for OpenVikingEmbeddingInput {
	type Err = &'static str;

	fn from_str(value: &str) -> Result<Self, Self::Err> {
		match value.trim().to_ascii_lowercase().as_str() {
			"text" => Ok(Self::Text),
			_ => Err("expected one of: text"),
		}
	}
}

#[derive(Debug, Clone, Serialize)]
struct OpenVikingFileConfig {
	storage: OpenVikingFileStorageConfig,
	log: OpenVikingFileLogConfig,
	embedding: OpenVikingFileEmbeddingSection,
	vlm: OpenVikingFileVlmConfig,
}

#[derive(Debug, Clone, Serialize)]
struct OpenVikingFileStorageConfig {
	workspace: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
struct OpenVikingFileLogConfig {
	level: String,
	output: String,
}

#[derive(Debug, Clone, Serialize)]
struct OpenVikingFileEmbeddingSection {
	dense: OpenVikingFileEmbeddingDenseConfig,
	max_concurrent: usize,
}

#[derive(Debug, Clone, Serialize)]
struct OpenVikingFileEmbeddingDenseConfig {
	provider: OpenVikingEmbeddingProvider,
	api_base: String,
	api_key: String,
	model: String,
	dimension: u32,
	input: OpenVikingEmbeddingInput,
}

#[derive(Debug, Clone, Serialize)]
struct OpenVikingFileVlmConfig {
	provider: OpenVikingVlmProvider,
	api_base: String,
	api_key: String,
	model: String,
	max_concurrent: usize,
	thinking: bool,
}

fn env_override_string_pair(canonical: &'static str, legacy: &'static str) -> Option<String> {
	env_override_string(canonical).or_else(|| env_override_string(legacy))
}

fn env_override_secret_pair(canonical: &'static str, legacy: &'static str) -> Option<String> {
	env_override_string_pair(canonical, legacy)
}

fn env_override_secret_with_alias_pair(
	canonical: &'static str,
	legacy: &'static str,
	alias: &'static str,
) -> Option<String> {
	env_override_secret_pair(canonical, legacy).or_else(|| env_override_string(alias))
}

fn env_override_string(key: &'static str) -> Option<String> {
	env::var(key)
		.ok()
		.map(|value| value.trim().to_string())
		.filter(|value| !value.is_empty())
}

fn env_override_bool_pair(
	canonical: &'static str,
	legacy: &'static str,
) -> Result<Option<bool>, OpenVikingRuntimeConfigError> {
	env_override_bool(canonical).and_then(|value| match value {
		Some(_) => Ok(value),
		None => env_override_bool(legacy),
	})
}

fn env_override_bool(key: &'static str) -> Result<Option<bool>, OpenVikingRuntimeConfigError> {
	let Some(raw) = env_override_string(key) else {
		return Ok(None);
	};
	match raw.to_ascii_lowercase().as_str() {
		"1" | "true" | "yes" | "on" => Ok(Some(true)),
		"0" | "false" | "no" | "off" => Ok(Some(false)),
		_ => Err(OpenVikingRuntimeConfigError::InvalidEnv {
			key,
			message: "expected boolean value".to_string(),
		}),
	}
}

fn env_override_usize_pair(
	canonical: &'static str,
	legacy: &'static str,
) -> Result<Option<usize>, OpenVikingRuntimeConfigError> {
	env_override_usize(canonical).and_then(|value| match value {
		Some(_) => Ok(value),
		None => env_override_usize(legacy),
	})
}

fn env_override_usize(key: &'static str) -> Result<Option<usize>, OpenVikingRuntimeConfigError> {
	let Some(raw) = env_override_string(key) else {
		return Ok(None);
	};
	raw.parse::<usize>()
		.map(Some)
		.map_err(|error| OpenVikingRuntimeConfigError::InvalidEnv {
			key,
			message: error.to_string(),
		})
}

fn env_override_u64_pair(
	canonical: &'static str,
	legacy: &'static str,
) -> Result<Option<u64>, OpenVikingRuntimeConfigError> {
	env_override_u64(canonical).and_then(|value| match value {
		Some(_) => Ok(value),
		None => env_override_u64(legacy),
	})
}

fn env_override_u64(key: &'static str) -> Result<Option<u64>, OpenVikingRuntimeConfigError> {
	let Some(raw) = env_override_string(key) else {
		return Ok(None);
	};
	raw.parse::<u64>()
		.map(Some)
		.map_err(|error| OpenVikingRuntimeConfigError::InvalidEnv {
			key,
			message: error.to_string(),
		})
}

fn env_override_u32_pair(
	canonical: &'static str,
	legacy: &'static str,
) -> Result<Option<u32>, OpenVikingRuntimeConfigError> {
	env_override_u32(canonical).and_then(|value| match value {
		Some(_) => Ok(value),
		None => env_override_u32(legacy),
	})
}

fn env_override_u32(key: &'static str) -> Result<Option<u32>, OpenVikingRuntimeConfigError> {
	let Some(raw) = env_override_string(key) else {
		return Ok(None);
	};
	raw.parse::<u32>()
		.map(Some)
		.map_err(|error| OpenVikingRuntimeConfigError::InvalidEnv {
			key,
			message: error.to_string(),
		})
}

fn env_override_u16_pair(
	canonical: &'static str,
	legacy: &'static str,
) -> Result<Option<u16>, OpenVikingRuntimeConfigError> {
	env_override_u16(canonical).and_then(|value| match value {
		Some(_) => Ok(value),
		None => env_override_u16(legacy),
	})
}

fn env_override_u16(key: &'static str) -> Result<Option<u16>, OpenVikingRuntimeConfigError> {
	let Some(raw) = env_override_string(key) else {
		return Ok(None);
	};
	raw.parse::<u16>()
		.map(Some)
		.map_err(|error| OpenVikingRuntimeConfigError::InvalidEnv {
			key,
			message: error.to_string(),
		})
}

fn env_override_enum_pair<T>(
	canonical: &'static str,
	legacy: &'static str,
) -> Result<Option<T>, OpenVikingRuntimeConfigError>
where
	T: FromStr<Err = &'static str>,
{
	env_override_enum(canonical).and_then(|value| match value {
		Some(_) => Ok(value),
		None => env_override_enum(legacy),
	})
}

fn env_override_enum<T>(key: &'static str) -> Result<Option<T>, OpenVikingRuntimeConfigError>
where
	T: FromStr<Err = &'static str>,
{
	let Some(raw) = env_override_string(key) else {
		return Ok(None);
	};
	T::from_str(&raw)
		.map(Some)
		.map_err(|message| OpenVikingRuntimeConfigError::InvalidEnv {
			key,
			message: message.to_string(),
		})
}

fn env_override_path_pair(canonical: &'static str, legacy: &'static str) -> Option<PathBuf> {
	env_override_path(canonical).or_else(|| env_override_path(legacy))
}

fn env_override_path(key: &'static str) -> Option<PathBuf> {
	env_override_string(key).map(PathBuf::from)
}

fn trim_optional_secret(value: Option<String>) -> Option<String> {
	value
		.map(|item| item.trim().to_string())
		.filter(|item| !item.is_empty())
}

fn invalid_config(field: &'static str, message: &str) -> OpenVikingRuntimeConfigError {
	OpenVikingRuntimeConfigError::InvalidConfig {
		field,
		message: message.to_string(),
	}
}

fn resolve_path(path: &Path) -> PathBuf {
	let expanded = expand_home(path);
	if expanded.is_absolute() {
		expanded
	} else {
		env::current_dir()
			.map(|cwd| cwd.join(&expanded))
			.unwrap_or_else(|_| PathBuf::from(".").join(&expanded))
	}
}

fn expand_home(path: &Path) -> PathBuf {
	let text = path.to_string_lossy();
	if text == "~" {
		return env::var_os("HOME")
			.map(PathBuf::from)
			.unwrap_or_else(|| PathBuf::from(path));
	}
	if let Some(suffix) = text.strip_prefix("~/")
		&& let Some(home) = env::var_os("HOME")
	{
		return PathBuf::from(home).join(suffix);
	}
	PathBuf::from(path)
}

fn write_generated_config(path: &Path, rendered: &str) -> Result<(), OpenVikingRuntimeConfigError> {
	let mut file = fs::OpenOptions::new()
		.create(true)
		.truncate(true)
		.write(true)
		.open(path)
		.map_err(
			|source| OpenVikingRuntimeConfigError::WriteGeneratedConfig {
				path: path.to_path_buf(),
				source,
			},
		)?;
	file.write_all(rendered.as_bytes())
		.and_then(|_| file.flush())
		.map_err(
			|source| OpenVikingRuntimeConfigError::WriteGeneratedConfig {
				path: path.to_path_buf(),
				source,
			},
		)?;
	#[cfg(unix)]
	{
		let permissions = fs::Permissions::from_mode(0o600);
		fs::set_permissions(path, permissions).map_err(|source| {
			OpenVikingRuntimeConfigError::WriteGeneratedConfig {
				path: path.to_path_buf(),
				source,
			}
		})?;
	}
	Ok(())
}
