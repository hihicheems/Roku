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

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SkillRegistryError {
	#[error("skill registry is disabled")]
	RegistryDisabled,
	#[error("invalid skill source url: {0}")]
	InvalidSourceUrl(String),
	#[error("unsupported skill source url: {0}")]
	UnsupportedSourceUrl(String),
	#[error("failed to download skill archive from {url}: {message}")]
	DownloadFailed { url: String, message: String },
	#[error("skill archive exceeded size limit for {url}: {size_bytes} bytes")]
	ArchiveTooLarge { url: String, size_bytes: u64 },
	#[error("failed to resolve GitHub repository metadata for {repo}: {message}")]
	GitHubMetadataFailed { repo: String, message: String },
	#[error("failed to unpack skill archive: {0}")]
	ArchiveExtract(String),
	#[error("skill package is missing SKILL.md under {0}")]
	SkillManifestMissing(PathBuf),
	#[error("skill package root is missing or invalid: {0}")]
	InvalidSkillRoot(PathBuf),
	#[error("skill document is too large to load into runtime context: {path}")]
	PromptDocumentTooLarge { path: PathBuf },
	#[error("failed to parse skill front matter: {0}")]
	FrontMatter(String),
	#[error("skill not found: {0}")]
	SkillNotFound(String),
	#[error("io error: {0}")]
	Io(#[from] std::io::Error),
	#[error("json error: {0}")]
	Json(#[from] serde_json::Error),
	#[error("yaml error: {0}")]
	Yaml(#[from] serde_yaml::Error),
	#[error("http client error: {0}")]
	HttpClient(#[from] reqwest::Error),
	#[error("zip error: {0}")]
	Zip(#[from] zip::result::ZipError),
}
