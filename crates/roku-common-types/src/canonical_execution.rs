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

//! Shared canonical execution contracts.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CanonicalDigest(pub String);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvocationMode {
	#[default]
	DirectExec,
	ShellWrapped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionShellContext {
	pub shell_program: String,
	#[serde(default)]
	pub shell_argv: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionActionClass {
	Read,
	Write,
	#[default]
	Exec,
	Network,
	Mixed,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionEnvPolicyMode {
	#[default]
	Clean,
	InheritSelected,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionEnvPolicy {
	#[serde(default)]
	pub mode: ExecutionEnvPolicyMode,
	#[serde(default)]
	pub allowed_keys: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionResourceScope {
	pub working_directory: String,
	#[serde(default)]
	pub resolved_targets: Vec<String>,
	#[serde(default)]
	pub effective_read_roots: Vec<String>,
	#[serde(default)]
	pub effective_write_roots: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonicalExecution {
	pub tool_name: String,
	pub program: String,
	#[serde(default)]
	pub argv: Vec<String>,
	pub invocation_mode: InvocationMode,
	#[serde(default)]
	pub shell_context: Option<ExecutionShellContext>,
	pub cwd: String,
	pub env_policy: ExecutionEnvPolicy,
	pub resource_scope: ExecutionResourceScope,
	pub action_class: ExecutionActionClass,
	pub digest: CanonicalDigest,
}
