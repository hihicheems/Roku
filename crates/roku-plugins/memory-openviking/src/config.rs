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

//! Typed configuration for the OpenViking long-term memory adapter.

use std::path::PathBuf;

use thiserror::Error;

/// Validated configuration for [`crate::OpenVikingLongTermMemoryBackend`].
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
	///
	/// The current adapter validates and carries this value, but [`crate::OpenVikingLongTermMemoryBackend::write`]
	/// still uses `wait = false` and does not block on indexing completion.
	pub write_wait_timeout_ms: u64,
	/// Whether OpenViking should treat ingestion warnings as strict failures.
	pub strict: bool,
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
