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

/// SQLite adapter config for control-plane persistence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqliteControlPlaneConfig {
	pub path: PathBuf,
}

impl SqliteControlPlaneConfig {
	pub fn new(path: impl Into<PathBuf>) -> Self {
		Self { path: path.into() }
	}
}

impl Default for SqliteControlPlaneConfig {
	fn default() -> Self {
		Self::new(".roku/state/control-plane.db")
	}
}
