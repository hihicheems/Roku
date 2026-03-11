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

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::ToolRuntimeError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SandboxProfile {
	#[default]
	NoIsolation,
	ReadOnlyFs,
	PythonResearch,
	ContainerRestricted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ToolSchema {
	pub required_fields: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeConstraints {
	pub timeout_ms: u64,
	pub max_retries: u8,
	pub retry_backoff_ms: u64,
	pub sandbox_profile: SandboxProfile,
	pub deterministic_hooks: bool,
	#[serde(default)]
	pub allowed_read_roots: Vec<PathBuf>,
	#[serde(default)]
	pub allowed_write_roots: Vec<PathBuf>,
}

impl RuntimeConstraints {
	pub fn max_attempts(&self) -> u8 {
		self.max_retries.saturating_add(1)
	}
}

impl Default for RuntimeConstraints {
	fn default() -> Self {
		Self {
			timeout_ms: 30_000,
			max_retries: 0,
			retry_backoff_ms: 0,
			sandbox_profile: SandboxProfile::NoIsolation,
			deterministic_hooks: true,
			allowed_read_roots: Vec::new(),
			allowed_write_roots: Vec::new(),
		}
	}
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolDescriptor {
	pub name: String,
	pub version: String,
	pub input_schema: ToolSchema,
	pub output_schema: String,
	pub required_capabilities: Vec<String>,
	pub runtime_constraints: RuntimeConstraints,
}

impl ToolDescriptor {
	pub(crate) fn validate(&self) -> Result<(), ToolRuntimeError> {
		if self.name.trim().is_empty() {
			return Err(ToolRuntimeError::InvalidDescriptor(
				"descriptor name cannot be empty".to_string(),
			));
		}
		if self.version.trim().is_empty() {
			return Err(ToolRuntimeError::InvalidDescriptor(
				"descriptor version cannot be empty".to_string(),
			));
		}
		if self.runtime_constraints.timeout_ms == 0 {
			return Err(ToolRuntimeError::InvalidDescriptor(
				"timeout_ms must be greater than zero".to_string(),
			));
		}
		Ok(())
	}
}
