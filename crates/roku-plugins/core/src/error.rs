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

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PluginIdError {
	#[error("plugin id cannot be empty")]
	Empty,
	#[error("plugin id must start and end with an ASCII alphanumeric character")]
	InvalidBoundary,
	#[error("plugin id contains invalid character `{character}`")]
	InvalidCharacter { character: char },
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PluginCoreError {
	#[error("failed to parse plugin manifest: {0}")]
	ManifestParse(String),
	#[error("failed to parse plugin policy config: {0}")]
	PolicyParse(String),
	#[error("plugin registry invariant violated: {0}")]
	RegistryInvariant(String),
	#[error(transparent)]
	InvalidPluginId(#[from] PluginIdError),
}
