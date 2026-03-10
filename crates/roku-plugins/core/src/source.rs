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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginSourceKind {
	ConfigPath,
	EnvRoot,
	WorkspaceRoot,
	UserRoot,
	Bundled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginSource {
	pub kind: PluginSourceKind,
	pub root: Option<PathBuf>,
	pub manifest_path: Option<PathBuf>,
	pub precedence: u8,
}

impl PluginSource {
	pub fn new(
		kind: PluginSourceKind,
		root: Option<PathBuf>,
		manifest_path: Option<PathBuf>,
		precedence: u8,
	) -> Self {
		Self {
			kind,
			root,
			manifest_path,
			precedence,
		}
	}

	pub fn is_bundled(&self) -> bool {
		self.kind == PluginSourceKind::Bundled
	}
}
