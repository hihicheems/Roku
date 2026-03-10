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

use serde::{Deserialize, Serialize};

use crate::SkillSource;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillDescriptor {
	pub name: String,
	pub description: String,
	pub version: String,
	pub license: Option<String>,
	pub entrypoint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledSkillRecord {
	pub descriptor: SkillDescriptor,
	pub source: SkillSource,
	pub installed_at_unix_ms: u64,
	pub install_dir: String,
	pub installed_files: Vec<String>,
}

impl InstalledSkillRecord {
	pub fn has_scripts(&self) -> bool {
		self.installed_files.iter().any(|path| {
			path == "scripts"
				|| path.starts_with("scripts/")
				|| path == "./scripts"
				|| path.starts_with("./scripts/")
		})
	}
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillInstallReport {
	pub skill_name: String,
	pub version: String,
	pub source_url: String,
	pub install_dir: String,
	pub installed_files: Vec<String>,
	pub activated_by: String,
	pub message: String,
}
