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
use std::io::Write;
use std::path::{Path, PathBuf};
use std::str::FromStr;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use roku_observability::{LogLevel, LogRecord, emit_global_log};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const HARD_MAX_MEMORY_RECALL_TOP_K: usize = 64;
pub const HARD_MAX_MEMORY_WRITE_BATCH_SIZE: usize = 256;
pub const HARD_MAX_MEMORY_CONNECT_TIMEOUT_MS: u64 = 30_000;
pub const HARD_MAX_MEMORY_REQUEST_TIMEOUT_MS: u64 = 120_000;
pub const HARD_MAX_OPENVIKING_EMBED_MAX_CONCURRENT: usize = 64;
pub const HARD_MAX_OPENVIKING_VLM_MAX_CONCURRENT: usize = 64;

const DEFAULT_OPENVIKING_CLIENT_BASE_URL: &str = "http://127.0.0.1:1933";
const DEFAULT_OPENVIKING_EMBEDDING_API_BASE: &str = "https://openrouter.ai/api/v1";
const DEFAULT_OPENVIKING_EMBEDDING_MODEL: &str = "thenlper/gte-base";
const DEFAULT_OPENVIKING_VLM_API_BASE: &str = "https://openrouter.ai/api/v1";
const DEFAULT_OPENVIKING_VLM_MODEL: &str = "qwen/qwen3.5-flash-02-23";
const OPENROUTER_API_KEY_LEGACY_ALIAS: &str = "OPENROUTER_API_KEY";
const GENERATED_OPENVIKING_LOG_LEVEL: &str = "INFO";
const GENERATED_OPENVIKING_LOG_OUTPUT: &str = "stdout";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryRuntimeConfig {
	pub enabled: bool,
	pub backend: MemoryBackend,
	pub recall: MemoryRecallConfig,
	pub write: MemoryWriteConfig,
	pub openviking: OpenVikingRuntimeConfig,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryRuntimeConfigPatch {
	pub enabled: Option<bool>,
	pub backend: Option<MemoryBackend>,
	#[serde(default)]
	pub recall: Option<MemoryRecallConfigPatch>,
	#[serde(default)]
	pub write: Option<MemoryWriteConfigPatch>,
	#[serde(default)]
	pub openviking: Option<OpenVikingRuntimeConfigPatch>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryRecallConfig {
	pub enabled: bool,
	pub top_k: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryRecallConfigPatch {
	pub enabled: Option<bool>,
	pub top_k: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryWriteConfig {
	pub enabled: bool,
	pub max_batch_size: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryWriteConfigPatch {
	pub enabled: Option<bool>,
	pub max_batch_size: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenVikingRuntimeConfig {
	pub client: OpenVikingClientConfig,
	pub process: OpenVikingProcessConfig,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenVikingRuntimeConfigPatch {
	#[serde(default)]
	pub client: Option<OpenVikingClientConfigPatch>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryBackend {
	OpenViking,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OpenVikingStorageBackend {
	Local,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OpenVikingEmbeddingProvider {
	OpenAi,
	Volcengine,
	Jina,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OpenVikingVlmProvider {
	OpenAi,
	Volcengine,
	Litellm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OpenVikingEmbeddingInput {
	Text,
}

#[derive(Debug, Error)]
pub enum MemoryRuntimeConfigError {
	#[error("invalid environment variable {key}: {message}")]
	InvalidEnv { key: &'static str, message: String },
	#[error("invalid runtime.memory configuration for {field}: {message}")]
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

impl Default for MemoryRuntimeConfig {
	fn default() -> Self {
		Self {
			enabled: false,
			backend: MemoryBackend::default(),
			recall: MemoryRecallConfig::default(),
			write: MemoryWriteConfig::default(),
			openviking: OpenVikingRuntimeConfig::default(),
		}
	}
}

impl Default for MemoryBackend {
	fn default() -> Self {
		Self::OpenViking
	}
}

impl Default for MemoryRecallConfig {
	fn default() -> Self {
		Self {
			enabled: true,
			top_k: 8,
		}
	}
}

impl Default for MemoryWriteConfig {
	fn default() -> Self {
		Self {
			enabled: true,
			max_batch_size: 16,
		}
	}
}

impl Default for OpenVikingRuntimeConfig {
	fn default() -> Self {
		Self {
			client: OpenVikingClientConfig::default(),
			process: OpenVikingProcessConfig::default(),
		}
	}
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

impl Default for OpenVikingStorageBackend {
	fn default() -> Self {
		Self::Local
	}
}

impl Default for OpenVikingEmbeddingProvider {
	fn default() -> Self {
		Self::OpenAi
	}
}

impl Default for OpenVikingVlmProvider {
	fn default() -> Self {
		Self::OpenAi
	}
}

impl Default for OpenVikingEmbeddingInput {
	fn default() -> Self {
		Self::Text
	}
}

impl MemoryRuntimeConfig {
	pub fn apply_patch(&mut self, patch: MemoryRuntimeConfigPatch) {
		if let Some(value) = patch.enabled {
			self.enabled = value;
		}
		if let Some(value) = patch.backend {
			self.backend = value;
		}
		if let Some(value) = patch.recall {
			self.recall.apply_patch(value);
		}
		if let Some(value) = patch.write {
			self.write.apply_patch(value);
		}
		if let Some(value) = patch.openviking {
			self.openviking.apply_patch(value);
		}
	}

	pub fn apply_env_overrides(&mut self) -> Result<(), MemoryRuntimeConfigError> {
		if let Some(value) = env_override_bool("ROKU_RUNTIME__MEMORY__ENABLED")? {
			self.enabled = value;
		}
		if let Some(value) = env_override_enum("ROKU_RUNTIME__MEMORY__BACKEND")? {
			self.backend = value;
		}
		if let Some(value) = env_override_bool("ROKU_RUNTIME__MEMORY__RECALL__ENABLED")? {
			self.recall.enabled = value;
		}
		if let Some(value) = env_override_usize("ROKU_RUNTIME__MEMORY__RECALL__TOP_K")? {
			self.recall.top_k = value;
		}
		if let Some(value) = env_override_bool("ROKU_RUNTIME__MEMORY__WRITE__ENABLED")? {
			self.write.enabled = value;
		}
		if let Some(value) = env_override_usize("ROKU_RUNTIME__MEMORY__WRITE__MAX_BATCH_SIZE")? {
			self.write.max_batch_size = value;
		}
		if let Some(value) =
			env_override_string("ROKU_RUNTIME__MEMORY__OPENVIKING__CLIENT__BASE_URL")
		{
			self.openviking.client.base_url = value;
		}
		if let Some(value) =
			env_override_u64("ROKU_RUNTIME__MEMORY__OPENVIKING__CLIENT__CONNECT_TIMEOUT_MS")?
		{
			self.openviking.client.connect_timeout_ms = value;
		}
		if let Some(value) =
			env_override_u64("ROKU_RUNTIME__MEMORY__OPENVIKING__CLIENT__REQUEST_TIMEOUT_MS")?
		{
			self.openviking.client.request_timeout_ms = value;
		}
		if let Some(value) =
			env_override_secret("ROKU_RUNTIME__MEMORY__OPENVIKING__CLIENT__API_KEY")
		{
			self.openviking.client.api_key = Some(value);
		}
		if let Some(value) =
			env_override_bool("ROKU_RUNTIME__MEMORY__OPENVIKING__PROCESS__MANAGED")?
		{
			self.openviking.process.managed = value;
		}
		if let Some(value) =
			env_override_path("ROKU_RUNTIME__MEMORY__OPENVIKING__PROCESS__CONFIG_OUTPUT_PATH")
		{
			self.openviking.process.config_output_path = value;
		}
		if let Some(value) =
			env_override_path("ROKU_RUNTIME__MEMORY__OPENVIKING__PROCESS__STORAGE__WORKSPACE")
		{
			self.openviking.process.storage.workspace = value;
		}
		if let Some(value) = env_override_enum(
			"ROKU_RUNTIME__MEMORY__OPENVIKING__PROCESS__STORAGE__VECTORDB_BACKEND",
		)? {
			self.openviking.process.storage.vectordb_backend = value;
		}
		if let Some(value) =
			env_override_enum("ROKU_RUNTIME__MEMORY__OPENVIKING__PROCESS__STORAGE__AGFS_BACKEND")?
		{
			self.openviking.process.storage.agfs_backend = value;
		}
		if let Some(value) =
			env_override_string("ROKU_RUNTIME__MEMORY__OPENVIKING__PROCESS__SERVER__HOST")
		{
			self.openviking.process.server.host = value;
		}
		if let Some(value) =
			env_override_u16("ROKU_RUNTIME__MEMORY__OPENVIKING__PROCESS__SERVER__PORT")?
		{
			self.openviking.process.server.port = value;
		}
		if let Some(value) =
			env_override_secret("ROKU_RUNTIME__MEMORY__OPENVIKING__PROCESS__SERVER__ROOT_API_KEY")
		{
			self.openviking.process.server.root_api_key = Some(value);
		}
		if let Some(value) =
			env_override_enum("ROKU_RUNTIME__MEMORY__OPENVIKING__PROCESS__EMBEDDING__PROVIDER")?
		{
			self.openviking.process.embedding.provider = value;
		}
		if let Some(value) =
			env_override_string("ROKU_RUNTIME__MEMORY__OPENVIKING__PROCESS__EMBEDDING__API_BASE")
		{
			self.openviking.process.embedding.api_base = value;
		}
		if let Some(value) =
			env_override_string("ROKU_RUNTIME__MEMORY__OPENVIKING__PROCESS__EMBEDDING__MODEL")
		{
			self.openviking.process.embedding.model = value;
		}
		if let Some(value) =
			env_override_u32("ROKU_RUNTIME__MEMORY__OPENVIKING__PROCESS__EMBEDDING__DIMENSION")?
		{
			self.openviking.process.embedding.dimension = value;
		}
		if let Some(value) = env_override_usize(
			"ROKU_RUNTIME__MEMORY__OPENVIKING__PROCESS__EMBEDDING__MAX_CONCURRENT",
		)? {
			self.openviking.process.embedding.max_concurrent = value;
		}
		if let Some(value) =
			env_override_enum("ROKU_RUNTIME__MEMORY__OPENVIKING__PROCESS__EMBEDDING__INPUT")?
		{
			self.openviking.process.embedding.input = value;
		}
		if let Some(value) =
			env_override_enum("ROKU_RUNTIME__MEMORY__OPENVIKING__PROCESS__VLM__PROVIDER")?
		{
			self.openviking.process.vlm.provider = value;
		}
		if let Some(value) =
			env_override_string("ROKU_RUNTIME__MEMORY__OPENVIKING__PROCESS__VLM__API_BASE")
		{
			self.openviking.process.vlm.api_base = value;
		}
		if let Some(value) =
			env_override_string("ROKU_RUNTIME__MEMORY__OPENVIKING__PROCESS__VLM__MODEL")
		{
			self.openviking.process.vlm.model = value;
		}
		if let Some(value) =
			env_override_usize("ROKU_RUNTIME__MEMORY__OPENVIKING__PROCESS__VLM__MAX_CONCURRENT")?
		{
			self.openviking.process.vlm.max_concurrent = value;
		}
		if let Some(value) =
			env_override_bool("ROKU_RUNTIME__MEMORY__OPENVIKING__PROCESS__VLM__THINKING")?
		{
			self.openviking.process.vlm.thinking = value;
		}
		if self.enabled || self.openviking.process.managed {
			if let Some(value) = env_override_secret_with_legacy(
				"ROKU_RUNTIME__MEMORY__OPENVIKING__PROCESS__EMBEDDING__API_KEY",
				OPENROUTER_API_KEY_LEGACY_ALIAS,
			) {
				self.openviking.process.embedding.api_key = Some(value);
			}
			if let Some(value) = env_override_secret_with_legacy(
				"ROKU_RUNTIME__MEMORY__OPENVIKING__PROCESS__VLM__API_KEY",
				OPENROUTER_API_KEY_LEGACY_ALIAS,
			) {
				self.openviking.process.vlm.api_key = Some(value);
			}
		}
		Ok(())
	}

	pub fn validate_and_clamp(&mut self) -> Result<(), MemoryRuntimeConfigError> {
		if self.recall.top_k == 0 {
			return Err(invalid_config(
				"runtime.memory.recall.top_k",
				"value must be greater than zero",
			));
		}
		if self.write.max_batch_size == 0 {
			return Err(invalid_config(
				"runtime.memory.write.max_batch_size",
				"value must be greater than zero",
			));
		}
		self.recall.top_k = self.recall.top_k.min(HARD_MAX_MEMORY_RECALL_TOP_K);
		self.write.max_batch_size = self
			.write
			.max_batch_size
			.min(HARD_MAX_MEMORY_WRITE_BATCH_SIZE);

		self.openviking.client.base_url = self.openviking.client.base_url.trim().to_string();
		if self.openviking.client.base_url.is_empty() {
			return Err(invalid_config(
				"runtime.memory.openviking.client.base_url",
				"value cannot be empty",
			));
		}
		if self.openviking.client.connect_timeout_ms == 0 {
			return Err(invalid_config(
				"runtime.memory.openviking.client.connect_timeout_ms",
				"value must be greater than zero",
			));
		}
		if self.openviking.client.request_timeout_ms == 0 {
			return Err(invalid_config(
				"runtime.memory.openviking.client.request_timeout_ms",
				"value must be greater than zero",
			));
		}
		self.openviking.client.connect_timeout_ms = self
			.openviking
			.client
			.connect_timeout_ms
			.min(HARD_MAX_MEMORY_CONNECT_TIMEOUT_MS);
		self.openviking.client.request_timeout_ms = self
			.openviking
			.client
			.request_timeout_ms
			.min(HARD_MAX_MEMORY_REQUEST_TIMEOUT_MS);
		self.openviking.client.api_key =
			trim_optional_secret(self.openviking.client.api_key.take());

		if self
			.openviking
			.process
			.config_output_path
			.as_os_str()
			.is_empty()
		{
			return Err(invalid_config(
				"runtime.memory.openviking.process.config_output_path",
				"value cannot be empty",
			));
		}
		if self
			.openviking
			.process
			.storage
			.workspace
			.as_os_str()
			.is_empty()
		{
			return Err(invalid_config(
				"runtime.memory.openviking.process.storage.workspace",
				"value cannot be empty",
			));
		}

		self.openviking.process.server.host =
			self.openviking.process.server.host.trim().to_string();
		if self.openviking.process.server.host.is_empty() {
			return Err(invalid_config(
				"runtime.memory.openviking.process.server.host",
				"value cannot be empty",
			));
		}
		if self.openviking.process.server.port == 0 {
			return Err(invalid_config(
				"runtime.memory.openviking.process.server.port",
				"value must be greater than zero",
			));
		}
		self.openviking.process.server.root_api_key =
			trim_optional_secret(self.openviking.process.server.root_api_key.take());

		self.openviking.process.embedding.api_base = self
			.openviking
			.process
			.embedding
			.api_base
			.trim()
			.to_string();
		self.openviking.process.embedding.model =
			self.openviking.process.embedding.model.trim().to_string();
		if self.openviking.process.embedding.api_base.is_empty() {
			return Err(invalid_config(
				"runtime.memory.openviking.process.embedding.api_base",
				"value cannot be empty",
			));
		}
		if self.openviking.process.embedding.model.is_empty() {
			return Err(invalid_config(
				"runtime.memory.openviking.process.embedding.model",
				"value cannot be empty",
			));
		}
		if self.openviking.process.embedding.dimension == 0 {
			return Err(invalid_config(
				"runtime.memory.openviking.process.embedding.dimension",
				"value must be greater than zero",
			));
		}
		if self.openviking.process.embedding.max_concurrent == 0 {
			return Err(invalid_config(
				"runtime.memory.openviking.process.embedding.max_concurrent",
				"value must be greater than zero",
			));
		}
		self.openviking.process.embedding.max_concurrent = self
			.openviking
			.process
			.embedding
			.max_concurrent
			.min(HARD_MAX_OPENVIKING_EMBED_MAX_CONCURRENT);
		self.openviking.process.embedding.api_key =
			trim_optional_secret(self.openviking.process.embedding.api_key.take());

		self.openviking.process.vlm.api_base =
			self.openviking.process.vlm.api_base.trim().to_string();
		self.openviking.process.vlm.model = self.openviking.process.vlm.model.trim().to_string();
		if self.openviking.process.vlm.api_base.is_empty() {
			return Err(invalid_config(
				"runtime.memory.openviking.process.vlm.api_base",
				"value cannot be empty",
			));
		}
		if self.openviking.process.vlm.model.is_empty() {
			return Err(invalid_config(
				"runtime.memory.openviking.process.vlm.model",
				"value cannot be empty",
			));
		}
		if self.openviking.process.vlm.max_concurrent == 0 {
			return Err(invalid_config(
				"runtime.memory.openviking.process.vlm.max_concurrent",
				"value must be greater than zero",
			));
		}
		self.openviking.process.vlm.max_concurrent = self
			.openviking
			.process
			.vlm
			.max_concurrent
			.min(HARD_MAX_OPENVIKING_VLM_MAX_CONCURRENT);
		self.openviking.process.vlm.api_key =
			trim_optional_secret(self.openviking.process.vlm.api_key.take());

		if self.should_generate_openviking_config() {
			self.validate_managed_process_secrets()?;
		}

		Ok(())
	}

	pub fn materialize_generated_openviking_config(
		&self,
	) -> Result<Option<PathBuf>, MemoryRuntimeConfigError> {
		if !self.should_generate_openviking_config() {
			return Ok(None);
		}

		let output_path = resolve_path(&self.openviking.process.config_output_path);
		let rendered = self.render_openviking_process_config()?;
		if let Some(parent) = output_path.parent() {
			fs::create_dir_all(parent).map_err(|source| {
				MemoryRuntimeConfigError::CreateGeneratedConfigDir {
					path: parent.to_path_buf(),
					source,
				}
			})?;
		}
		write_generated_config(&output_path, &rendered)?;
		Ok(Some(output_path))
	}

	fn should_generate_openviking_config(&self) -> bool {
		self.enabled
			&& matches!(self.backend, MemoryBackend::OpenViking)
			&& self.openviking.process.managed
	}

	fn validate_managed_process_secrets(&self) -> Result<(), MemoryRuntimeConfigError> {
		if self.openviking.process.embedding.api_key.is_none() {
			return Err(invalid_config(
				"runtime.memory.openviking.process.embedding.api_key",
				"value is required when managed OpenViking process config generation is enabled",
			));
		}
		if self.openviking.process.vlm.api_key.is_none() {
			return Err(invalid_config(
				"runtime.memory.openviking.process.vlm.api_key",
				"value is required when managed OpenViking process config generation is enabled",
			));
		}
		Ok(())
	}

	fn render_openviking_process_config(&self) -> Result<String, MemoryRuntimeConfigError> {
		self.validate_managed_process_secrets()?;

		let workspace_root = resolve_path(&self.openviking.process.storage.workspace);
		let file_config = OpenVikingFileConfig {
			storage: OpenVikingFileStorageConfig {
				workspace: workspace_root,
			},
			log: OpenVikingFileLogConfig {
				level: GENERATED_OPENVIKING_LOG_LEVEL.to_string(),
				output: GENERATED_OPENVIKING_LOG_OUTPUT.to_string(),
			},
			embedding: OpenVikingFileEmbeddingSection {
				dense: OpenVikingFileEmbeddingDenseConfig {
					provider: self.openviking.process.embedding.provider,
					api_base: self.openviking.process.embedding.api_base.clone(),
					api_key: self
						.openviking
						.process
						.embedding
						.api_key
						.clone()
						.expect("embedding api key is required for managed config"),
					model: self.openviking.process.embedding.model.clone(),
					dimension: self.openviking.process.embedding.dimension,
					input: self.openviking.process.embedding.input,
				},
				max_concurrent: self.openviking.process.embedding.max_concurrent,
			},
			vlm: OpenVikingFileVlmConfig {
				provider: self.openviking.process.vlm.provider,
				api_base: self.openviking.process.vlm.api_base.clone(),
				api_key: self
					.openviking
					.process
					.vlm
					.api_key
					.clone()
					.expect("vlm api key is required for managed config"),
				model: self.openviking.process.vlm.model.clone(),
				max_concurrent: self.openviking.process.vlm.max_concurrent,
				thinking: self.openviking.process.vlm.thinking,
			},
		};
		serde_json::to_string_pretty(&file_config)
			.map_err(|error| MemoryRuntimeConfigError::SerializeGeneratedConfig(error.to_string()))
	}
}

impl MemoryRecallConfig {
	fn apply_patch(&mut self, patch: MemoryRecallConfigPatch) {
		if let Some(value) = patch.enabled {
			self.enabled = value;
		}
		if let Some(value) = patch.top_k {
			self.top_k = value;
		}
	}
}

impl MemoryWriteConfig {
	fn apply_patch(&mut self, patch: MemoryWriteConfigPatch) {
		if let Some(value) = patch.enabled {
			self.enabled = value;
		}
		if let Some(value) = patch.max_batch_size {
			self.max_batch_size = value;
		}
	}
}

impl OpenVikingRuntimeConfig {
	fn apply_patch(&mut self, patch: OpenVikingRuntimeConfigPatch) {
		if let Some(value) = patch.client {
			self.client.apply_patch(value);
		}
		if let Some(value) = patch.process {
			self.process.apply_patch(value);
		}
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

impl FromStr for MemoryBackend {
	type Err = &'static str;

	fn from_str(value: &str) -> Result<Self, Self::Err> {
		match value.trim().to_ascii_lowercase().as_str() {
			"openviking" => Ok(Self::OpenViking),
			_ => Err("expected one of: openviking"),
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

fn env_override_string(key: &'static str) -> Option<String> {
	env::var(key)
		.ok()
		.map(|value| value.trim().to_string())
		.filter(|value| !value.is_empty())
}

fn env_override_secret(key: &'static str) -> Option<String> {
	env_override_string(key)
}

fn env_override_secret_with_legacy(
	canonical: &'static str,
	legacy: &'static str,
) -> Option<String> {
	if let Some(value) = env_override_secret(canonical) {
		return Some(value);
	}
	let Some(value) = env_override_secret(legacy) else {
		return None;
	};
	let _ = emit_global_log(
		LogRecord::new(
			"roku-cmd",
			LogLevel::Warn,
			"using legacy memory secret env alias",
		)
		.with_field("legacy_env", legacy.to_string())
		.with_field("canonical_env", canonical.to_string()),
	);
	Some(value)
}

fn env_override_bool(key: &'static str) -> Result<Option<bool>, MemoryRuntimeConfigError> {
	let Some(raw) = env_override_string(key) else {
		return Ok(None);
	};
	match raw.to_ascii_lowercase().as_str() {
		"1" | "true" | "yes" | "on" => Ok(Some(true)),
		"0" | "false" | "no" | "off" => Ok(Some(false)),
		_ => Err(MemoryRuntimeConfigError::InvalidEnv {
			key,
			message: "expected boolean value".to_string(),
		}),
	}
}

fn env_override_usize(key: &'static str) -> Result<Option<usize>, MemoryRuntimeConfigError> {
	let Some(raw) = env_override_string(key) else {
		return Ok(None);
	};
	raw.parse::<usize>()
		.map(Some)
		.map_err(|error| MemoryRuntimeConfigError::InvalidEnv {
			key,
			message: error.to_string(),
		})
}

fn env_override_u64(key: &'static str) -> Result<Option<u64>, MemoryRuntimeConfigError> {
	let Some(raw) = env_override_string(key) else {
		return Ok(None);
	};
	raw.parse::<u64>()
		.map(Some)
		.map_err(|error| MemoryRuntimeConfigError::InvalidEnv {
			key,
			message: error.to_string(),
		})
}

fn env_override_u32(key: &'static str) -> Result<Option<u32>, MemoryRuntimeConfigError> {
	let Some(raw) = env_override_string(key) else {
		return Ok(None);
	};
	raw.parse::<u32>()
		.map(Some)
		.map_err(|error| MemoryRuntimeConfigError::InvalidEnv {
			key,
			message: error.to_string(),
		})
}

fn env_override_u16(key: &'static str) -> Result<Option<u16>, MemoryRuntimeConfigError> {
	let Some(raw) = env_override_string(key) else {
		return Ok(None);
	};
	raw.parse::<u16>()
		.map(Some)
		.map_err(|error| MemoryRuntimeConfigError::InvalidEnv {
			key,
			message: error.to_string(),
		})
}

fn env_override_enum<T>(key: &'static str) -> Result<Option<T>, MemoryRuntimeConfigError>
where
	T: FromStr<Err = &'static str>,
{
	let Some(raw) = env_override_string(key) else {
		return Ok(None);
	};
	T::from_str(&raw)
		.map(Some)
		.map_err(|message| MemoryRuntimeConfigError::InvalidEnv {
			key,
			message: message.to_string(),
		})
}

fn env_override_path(key: &'static str) -> Option<PathBuf> {
	env_override_string(key).map(|value| PathBuf::from(value))
}

fn trim_optional_secret(value: Option<String>) -> Option<String> {
	value
		.map(|item| item.trim().to_string())
		.filter(|item| !item.is_empty())
}

fn invalid_config(field: &'static str, message: &str) -> MemoryRuntimeConfigError {
	MemoryRuntimeConfigError::InvalidConfig {
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

fn write_generated_config(path: &Path, rendered: &str) -> Result<(), MemoryRuntimeConfigError> {
	let mut file = fs::OpenOptions::new()
		.create(true)
		.truncate(true)
		.write(true)
		.open(path)
		.map_err(|source| MemoryRuntimeConfigError::WriteGeneratedConfig {
			path: path.to_path_buf(),
			source,
		})?;
	file.write_all(rendered.as_bytes()).map_err(|source| {
		MemoryRuntimeConfigError::WriteGeneratedConfig {
			path: path.to_path_buf(),
			source,
		}
	})?;
	#[cfg(unix)]
	{
		fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|source| {
			MemoryRuntimeConfigError::WriteGeneratedConfig {
				path: path.to_path_buf(),
				source,
			}
		})?;
	}
	Ok(())
}
