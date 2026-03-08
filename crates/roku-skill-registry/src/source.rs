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
use url::Url;

use crate::SkillRegistryError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SkillSource {
	GitHub {
		owner: String,
		repo: String,
		reference: Option<String>,
		subpath: Option<String>,
		original_url: String,
	},
	ArchiveZip {
		archive_url: String,
		subpath: Option<String>,
		original_url: String,
	},
}

impl SkillSource {
	pub fn parse(source_url: &str) -> Result<Self, SkillRegistryError> {
		let parsed = Url::parse(source_url)
			.map_err(|error| SkillRegistryError::InvalidSourceUrl(error.to_string()))?;
		let host = parsed.host_str().unwrap_or_default();
		match host {
			"github.com" => parse_github_source(&parsed),
			"raw.githubusercontent.com" => parse_github_raw_source(&parsed),
			_ if parsed.path().ends_with(".zip") => Ok(Self::ArchiveZip {
				archive_url: parsed.to_string(),
				subpath: None,
				original_url: parsed.to_string(),
			}),
			_ => Err(SkillRegistryError::UnsupportedSourceUrl(parsed.to_string())),
		}
	}

	pub fn original_url(&self) -> &str {
		match self {
			Self::GitHub { original_url, .. } | Self::ArchiveZip { original_url, .. } => {
				original_url.as_str()
			}
		}
	}

	pub fn subpath(&self) -> Option<&str> {
		match self {
			Self::GitHub { subpath, .. } | Self::ArchiveZip { subpath, .. } => subpath.as_deref(),
		}
	}

	pub fn version_hint(&self) -> Option<&str> {
		match self {
			Self::GitHub { reference, .. } => reference.as_deref(),
			Self::ArchiveZip { .. } => None,
		}
	}
}

fn parse_github_source(url: &Url) -> Result<SkillSource, SkillRegistryError> {
	let segments = path_segments(url)?;
	if segments.len() < 2 {
		return Err(SkillRegistryError::UnsupportedSourceUrl(url.to_string()));
	}

	let owner = segments[0].to_string();
	let repo = segments[1].trim_end_matches(".git").to_string();
	if repo.is_empty() {
		return Err(SkillRegistryError::UnsupportedSourceUrl(url.to_string()));
	}

	if segments.len() == 2 {
		return Ok(SkillSource::GitHub {
			owner,
			repo,
			reference: None,
			subpath: None,
			original_url: url.to_string(),
		});
	}

	match segments[2] {
		"tree" if segments.len() >= 4 => Ok(SkillSource::GitHub {
			owner,
			repo,
			reference: Some(segments[3].to_string()),
			subpath: join_optional_segments(&segments[4..]),
			original_url: url.to_string(),
		}),
		"blob" if segments.len() >= 5 => Ok(SkillSource::GitHub {
			owner,
			repo,
			reference: Some(segments[3].to_string()),
			subpath: parent_directory(&segments[4..]),
			original_url: url.to_string(),
		}),
		"archive" if url.path().ends_with(".zip") => Ok(SkillSource::ArchiveZip {
			archive_url: url.to_string(),
			subpath: None,
			original_url: url.to_string(),
		}),
		_ => Err(SkillRegistryError::UnsupportedSourceUrl(url.to_string())),
	}
}

fn parse_github_raw_source(url: &Url) -> Result<SkillSource, SkillRegistryError> {
	let segments = path_segments(url)?;
	if segments.len() < 4 {
		return Err(SkillRegistryError::UnsupportedSourceUrl(url.to_string()));
	}

	Ok(SkillSource::GitHub {
		owner: segments[0].to_string(),
		repo: segments[1].trim_end_matches(".git").to_string(),
		reference: Some(segments[2].to_string()),
		subpath: parent_directory(&segments[3..]),
		original_url: url.to_string(),
	})
}

fn path_segments(url: &Url) -> Result<Vec<&str>, SkillRegistryError> {
	url.path_segments()
		.map(|segments| {
			segments
				.filter(|segment| !segment.is_empty())
				.collect::<Vec<_>>()
		})
		.ok_or_else(|| SkillRegistryError::UnsupportedSourceUrl(url.to_string()))
}

fn join_optional_segments(segments: &[&str]) -> Option<String> {
	(!segments.is_empty()).then(|| segments.join("/"))
}

fn parent_directory(segments: &[&str]) -> Option<String> {
	if segments.len() <= 1 {
		None
	} else {
		Some(segments[..segments.len() - 1].join("/"))
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parses_github_tree_url() {
		let source =
			SkillSource::parse("https://github.com/anthropics/skills/tree/main/skills/claude-api")
				.expect("tree url should parse");
		assert_eq!(
			source,
			SkillSource::GitHub {
				owner: "anthropics".to_string(),
				repo: "skills".to_string(),
				reference: Some("main".to_string()),
				subpath: Some("skills/claude-api".to_string()),
				original_url: "https://github.com/anthropics/skills/tree/main/skills/claude-api"
					.to_string(),
			}
		);
	}

	#[test]
	fn parses_github_blob_url_as_parent_directory() {
		let source = SkillSource::parse(
			"https://github.com/anthropics/skills/blob/main/skills/claude-api/SKILL.md",
		)
		.expect("blob url should parse");
		assert_eq!(source.subpath(), Some("skills/claude-api"));
	}

	#[test]
	fn parses_raw_github_url_as_parent_directory() {
		let source = SkillSource::parse(
			"https://raw.githubusercontent.com/anthropics/skills/main/skills/claude-api/SKILL.md",
		)
		.expect("raw github url should parse");
		assert_eq!(source.subpath(), Some("skills/claude-api"));
	}

	#[test]
	fn parses_direct_zip_archive_url() {
		let source = SkillSource::parse("https://example.com/skills/archive.zip")
			.expect("zip url should parse");
		assert_eq!(
			source,
			SkillSource::ArchiveZip {
				archive_url: "https://example.com/skills/archive.zip".to_string(),
				subpath: None,
				original_url: "https://example.com/skills/archive.zip".to_string(),
			}
		);
	}

	#[test]
	fn rejects_unsupported_url() {
		let error = SkillSource::parse("https://example.com/skills/archive.tar.gz")
			.expect_err("tar archive should be rejected");
		assert!(matches!(error, SkillRegistryError::UnsupportedSourceUrl(_)));
	}
}
