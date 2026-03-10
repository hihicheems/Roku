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

use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolFailure {
	pub message: String,
	pub retriable: bool,
}

impl ToolFailure {
	pub fn retriable(message: impl Into<String>) -> Self {
		Self {
			message: message.into(),
			retriable: true,
		}
	}

	pub fn terminal(message: impl Into<String>) -> Self {
		Self {
			message: message.into(),
			retriable: false,
		}
	}
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ToolRuntimeError {
	#[error("tool not found: {0}")]
	ToolNotFound(String),
	#[error("tool already registered: {0}")]
	ToolAlreadyRegistered(String),
	#[error("invalid descriptor: {0}")]
	InvalidDescriptor(String),
	#[error("input schema violation for {tool}: missing required fields {missing_fields:?}")]
	InputSchemaViolation {
		tool: String,
		missing_fields: Vec<String>,
	},
	#[error("capability denied for {tool}: missing {missing_capabilities:?}")]
	CapabilityDenied {
		tool: String,
		missing_capabilities: Vec<String>,
	},
	#[error("tool timed out: {tool} exceeded {timeout_ms}ms (elapsed {elapsed_ms}ms)")]
	Timeout {
		tool: String,
		timeout_ms: u64,
		elapsed_ms: u128,
	},
	#[error("tool execution failed: {tool} after {attempts} attempts: {message}")]
	ExecutionFailed {
		tool: String,
		attempts: u8,
		message: String,
		retriable: bool,
	},
}
