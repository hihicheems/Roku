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

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::blocking::Client;
use roku_observability::{LogLevel, LogRecord, emit_global_log};
use serde::Deserialize;
use walkdir::WalkDir;

use crate::{
	InstalledSkillRecord, SkillDescriptor, SkillInstallReport, SkillRegistryError, SkillSource,
};

const GITHUB_API_BASE: &str = "https://api.github.com";
const INSTALLER_USER_AGENT: &str = "RokuSkillInstaller/0.1";
const MAX_ARCHIVE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_PROMPT_DOCUMENT_BYTES: usize = 24 * 1024;
const MAX_PROMPT_DOCUMENTS: usize = 24;

#[derive(Debug, Default, Deserialize)]
struct ParsedSkillFrontMatter {
	name: Option<String>,
	description: Option<String>,
	license: Option<String>,
}

#[derive(Clone)]
enum SkillRegistryBackend {
	Disabled,
	FileBacked { root: PathBuf },
}

#[derive(Clone)]
pub struct SkillRegistry {
	backend: SkillRegistryBackend,
	fetcher: Arc<dyn SkillArchiveFetcher>,
}

pub trait SkillArchiveFetcher: Send + Sync {
	fn fetch(&self, source: &SkillSource) -> Result<DownloadedArchive, SkillRegistryError>;
}

#[derive(Debug, Clone)]
pub struct DownloadedArchive {
	pub archive_url: String,
	pub bytes: Vec<u8>,
	pub resolved_reference: Option<String>,
}

#[derive(Clone)]
pub struct HttpSkillArchiveFetcher {
	client: Client,
}

impl Default for HttpSkillArchiveFetcher {
	fn default() -> Self {
		let client = Client::builder()
			.user_agent(INSTALLER_USER_AGENT)
			.timeout(Duration::from_secs(30))
			.build()
			.expect("skill installer client should build");
		Self { client }
	}
}

impl SkillRegistry {
	pub fn disabled() -> Self {
		Self {
			backend: SkillRegistryBackend::Disabled,
			fetcher: Arc::new(HttpSkillArchiveFetcher::default()),
		}
	}

	pub fn file_backed(root: impl Into<PathBuf>) -> Self {
		Self {
			backend: SkillRegistryBackend::FileBacked { root: root.into() },
			fetcher: Arc::new(HttpSkillArchiveFetcher::default()),
		}
	}

	pub fn with_fetcher(mut self, fetcher: Arc<dyn SkillArchiveFetcher>) -> Self {
		self.fetcher = fetcher;
		self
	}

	pub fn is_enabled(&self) -> bool {
		matches!(self.backend, SkillRegistryBackend::FileBacked { .. })
	}

	pub fn install_from_url(
		&self,
		source_url: &str,
		activated_by: &str,
	) -> Result<SkillInstallReport, SkillRegistryError> {
		let root = self.root_path()?;
		ensure_registry_layout(root)?;

		let source = SkillSource::parse(source_url)?;
		log_skill_event(
			"installing skill from source",
			[
				("source_url", source.original_url().to_string()),
				("activated_by", activated_by.to_string()),
			],
		);
		let archive = self.fetcher.fetch(&source)?;
		let cache_dir = root.join("cache");
		fs::create_dir_all(&cache_dir)?;
		let temp_dir = tempfile::Builder::new()
			.prefix("skill-install-")
			.tempdir_in(cache_dir)?;
		let extract_dir = temp_dir.path().join("extract");
		fs::create_dir_all(&extract_dir)?;
		extract_archive(&archive.bytes, &extract_dir)?;

		let archive_root = unwrapped_archive_root(&extract_dir)?;
		let source_dir = resolve_source_dir(&archive_root, source.subpath())?;
		let descriptor = load_descriptor(
			&source_dir,
			archive
				.resolved_reference
				.as_deref()
				.or(source.version_hint()),
		)?;
		let storage_key = storage_key(&descriptor.name);
		let final_dir = root.join("installed").join(&storage_key);
		let stage_dir = temp_dir.path().join("publish").join(&storage_key);
		copy_dir_recursive(&source_dir, &stage_dir)?;
		publish_skill_dir(root, &stage_dir, &final_dir)?;

		let installed_files = list_relative_files(&final_dir)?;
		let record = InstalledSkillRecord {
			descriptor: descriptor.clone(),
			source: source.clone(),
			installed_at_unix_ms: now_unix_ms(),
			install_dir: relative_display_path(root, &final_dir),
			installed_files: installed_files.clone(),
		};
		write_record(root, &record)?;

		let install_dir = display_path(&final_dir);
		let message = format!(
			"Installed skill `{}` from {} into {}. Reference `{}` in future requests to activate it.",
			descriptor.name,
			source.original_url(),
			install_dir,
			descriptor.name,
		);
		log_skill_event(
			"installed skill successfully",
			[
				("skill_name", descriptor.name.clone()),
				("source_url", source.original_url().to_string()),
				("install_dir", install_dir.clone()),
			],
		);

		Ok(SkillInstallReport {
			skill_name: descriptor.name,
			version: descriptor.version,
			source_url: source.original_url().to_string(),
			install_dir,
			installed_files,
			activated_by: activated_by.to_string(),
			message,
		})
	}

	pub fn list_skills(&self) -> Result<Vec<InstalledSkillRecord>, SkillRegistryError> {
		let Some(root) = self.optional_root_path() else {
			return Ok(Vec::new());
		};
		let registry_dir = root.join("registry");
		if !registry_dir.exists() {
			return Ok(Vec::new());
		}

		let mut records = fs::read_dir(registry_dir)?
			.filter_map(Result::ok)
			.filter(|entry| {
				entry.path().extension().and_then(|value| value.to_str()) == Some("json")
			})
			.map(|entry| {
				let content = fs::read_to_string(entry.path())?;
				serde_json::from_str::<InstalledSkillRecord>(&content)
					.map_err(SkillRegistryError::from)
			})
			.collect::<Result<Vec<_>, _>>()?;
		records.sort_by(|left, right| left.descriptor.name.cmp(&right.descriptor.name));
		Ok(records)
	}

	pub fn render_prompt_context_for_query(
		&self,
		query: &str,
		max_chars: usize,
	) -> Result<Option<String>, SkillRegistryError> {
		let Some(root) = self.optional_root_path() else {
			return Ok(None);
		};
		if max_chars == 0 {
			return Ok(None);
		}

		let matched = self
			.list_skills()?
			.into_iter()
			.filter(|record| query_mentions_skill(query, &record.descriptor.name))
			.collect::<Vec<_>>();
		if matched.is_empty() {
			return Ok(None);
		}

		let mut sections = Vec::new();
		let mut remaining = max_chars;
		for record in matched {
			if remaining == 0 {
				break;
			}
			let context = render_skill_context(root, &record, remaining)?;
			if context.is_empty() {
				continue;
			}
			remaining = remaining.saturating_sub(context.chars().count());
			sections.push(context);
		}
		if sections.is_empty() {
			Ok(None)
		} else {
			Ok(Some(format!(
				"Installed skill guidance explicitly referenced by the user:\n\n{}",
				sections.join("\n\n")
			)))
		}
	}

	fn root_path(&self) -> Result<&Path, SkillRegistryError> {
		match &self.backend {
			SkillRegistryBackend::Disabled => Err(SkillRegistryError::RegistryDisabled),
			SkillRegistryBackend::FileBacked { root } => Ok(root.as_path()),
		}
	}

	fn optional_root_path(&self) -> Option<&Path> {
		match &self.backend {
			SkillRegistryBackend::Disabled => None,
			SkillRegistryBackend::FileBacked { root } => Some(root.as_path()),
		}
	}
}

impl SkillArchiveFetcher for HttpSkillArchiveFetcher {
	fn fetch(&self, source: &SkillSource) -> Result<DownloadedArchive, SkillRegistryError> {
		match source {
			SkillSource::GitHub {
				owner,
				repo,
				reference,
				..
			} => {
				let resolved_reference = match reference {
					Some(reference) => reference.clone(),
					None => self.resolve_default_branch(owner, repo)?,
				};
				let mut last_error = None;
				for archive_url in github_archive_candidates(owner, repo, &resolved_reference) {
					match self.download_archive(&archive_url) {
						Ok(bytes) => {
							return Ok(DownloadedArchive {
								archive_url,
								bytes,
								resolved_reference: Some(resolved_reference.clone()),
							});
						}
						Err(error) => last_error = Some(error),
					}
				}
				Err(
					last_error.unwrap_or_else(|| SkillRegistryError::DownloadFailed {
						url: format!("https://github.com/{owner}/{repo}"),
						message: "no archive candidate succeeded".to_string(),
					}),
				)
			}
			SkillSource::ArchiveZip { archive_url, .. } => Ok(DownloadedArchive {
				archive_url: archive_url.clone(),
				bytes: self.download_archive(archive_url)?,
				resolved_reference: None,
			}),
		}
	}
}

impl HttpSkillArchiveFetcher {
	fn resolve_default_branch(
		&self,
		owner: &str,
		repo: &str,
	) -> Result<String, SkillRegistryError> {
		#[derive(Deserialize)]
		struct GitHubRepositoryMetadata {
			default_branch: String,
		}

		let metadata_url = format!("{GITHUB_API_BASE}/repos/{owner}/{repo}");
		let response = self.client.get(&metadata_url).send().map_err(|error| {
			SkillRegistryError::GitHubMetadataFailed {
				repo: format!("{owner}/{repo}"),
				message: error.to_string(),
			}
		})?;
		let status = response.status();
		if !status.is_success() {
			return Err(SkillRegistryError::GitHubMetadataFailed {
				repo: format!("{owner}/{repo}"),
				message: format!("http status {status}"),
			});
		}

		let metadata = response
			.json::<GitHubRepositoryMetadata>()
			.map_err(|error| SkillRegistryError::GitHubMetadataFailed {
				repo: format!("{owner}/{repo}"),
				message: error.to_string(),
			})?;
		Ok(metadata.default_branch)
	}

	fn download_archive(&self, archive_url: &str) -> Result<Vec<u8>, SkillRegistryError> {
		log_skill_event(
			"downloading skill archive",
			[("archive_url", archive_url.to_string())],
		);
		let mut response = self.client.get(archive_url).send().map_err(|error| {
			SkillRegistryError::DownloadFailed {
				url: archive_url.to_string(),
				message: error.to_string(),
			}
		})?;
		let status = response.status();
		if !status.is_success() {
			return Err(SkillRegistryError::DownloadFailed {
				url: archive_url.to_string(),
				message: format!("http status {status}"),
			});
		}
		if let Some(length) = response.content_length()
			&& length > MAX_ARCHIVE_BYTES
		{
			return Err(SkillRegistryError::ArchiveTooLarge {
				url: archive_url.to_string(),
				size_bytes: length,
			});
		}

		let mut bytes = Vec::new();
		response
			.copy_to(&mut bytes)
			.map_err(|error| SkillRegistryError::DownloadFailed {
				url: archive_url.to_string(),
				message: error.to_string(),
			})?;
		let size_bytes = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
		if size_bytes > MAX_ARCHIVE_BYTES {
			return Err(SkillRegistryError::ArchiveTooLarge {
				url: archive_url.to_string(),
				size_bytes,
			});
		}
		Ok(bytes)
	}
}

fn github_archive_candidates(owner: &str, repo: &str, reference: &str) -> Vec<String> {
	if looks_like_commit(reference) {
		return vec![format!(
			"https://github.com/{owner}/{repo}/archive/{reference}.zip"
		)];
	}

	vec![
		format!("https://github.com/{owner}/{repo}/archive/refs/heads/{reference}.zip"),
		format!("https://github.com/{owner}/{repo}/archive/refs/tags/{reference}.zip"),
	]
}

fn looks_like_commit(reference: &str) -> bool {
	reference.len() >= 7
		&& reference
			.chars()
			.all(|character| character.is_ascii_hexdigit())
}

fn ensure_registry_layout(root: &Path) -> Result<(), SkillRegistryError> {
	for directory in [
		root,
		&root.join("installed"),
		&root.join("registry"),
		&root.join("cache"),
		&root.join("backups"),
	] {
		fs::create_dir_all(directory)?;
	}
	Ok(())
}

fn extract_archive(bytes: &[u8], target_dir: &Path) -> Result<(), SkillRegistryError> {
	let reader = Cursor::new(bytes.to_vec());
	let mut archive = zip::ZipArchive::new(reader)?;
	for index in 0..archive.len() {
		let mut file = archive.by_index(index)?;
		let Some(enclosed_name) = file.enclosed_name().map(|value| value.to_path_buf()) else {
			return Err(SkillRegistryError::ArchiveExtract(format!(
				"archive entry has unsafe path: {}",
				file.name()
			)));
		};
		let output_path = target_dir.join(enclosed_name);
		if file.is_dir() {
			fs::create_dir_all(&output_path)?;
			continue;
		}
		if let Some(parent) = output_path.parent() {
			fs::create_dir_all(parent)?;
		}
		let mut output_file = fs::File::create(&output_path)?;
		std::io::copy(&mut file, &mut output_file)?;
	}
	Ok(())
}

fn unwrapped_archive_root(extract_dir: &Path) -> Result<PathBuf, SkillRegistryError> {
	let entries = fs::read_dir(extract_dir)?
		.filter_map(Result::ok)
		.collect::<Vec<_>>();
	if entries.len() == 1 {
		let first = &entries[0];
		if first.file_type()?.is_dir() {
			return Ok(first.path());
		}
	}
	Ok(extract_dir.to_path_buf())
}

fn resolve_source_dir(
	archive_root: &Path,
	subpath: Option<&str>,
) -> Result<PathBuf, SkillRegistryError> {
	let source_dir = subpath
		.map(|subpath| archive_root.join(subpath))
		.unwrap_or_else(|| archive_root.to_path_buf());
	if !source_dir.exists() || !source_dir.is_dir() {
		return Err(SkillRegistryError::InvalidSkillRoot(source_dir));
	}
	let manifest_path = source_dir.join("SKILL.md");
	if !manifest_path.exists() {
		return Err(SkillRegistryError::SkillManifestMissing(source_dir));
	}
	Ok(source_dir)
}

fn load_descriptor(
	source_dir: &Path,
	version_hint: Option<&str>,
) -> Result<SkillDescriptor, SkillRegistryError> {
	let manifest_path = source_dir.join("SKILL.md");
	let content = fs::read_to_string(&manifest_path)?;
	let (front_matter, body) = split_front_matter(&content)?;
	let title = markdown_title(body).unwrap_or_else(|| {
		source_dir
			.file_name()
			.and_then(|value| value.to_str())
			.unwrap_or("skill")
			.to_string()
	});
	let description = front_matter.description.unwrap_or_else(|| title.clone());
	let name = front_matter.name.unwrap_or_else(|| {
		infer_name_from_directory(source_dir)
			.unwrap_or_else(|| title.to_ascii_lowercase().replace(' ', "-"))
	});

	Ok(SkillDescriptor {
		name,
		description,
		version: version_hint.unwrap_or("archive").to_string(),
		license: front_matter.license,
		entrypoint: "SKILL.md".to_string(),
	})
}

fn split_front_matter(content: &str) -> Result<(ParsedSkillFrontMatter, &str), SkillRegistryError> {
	let mut lines = content.lines();
	if !matches!(lines.next(), Some("---")) {
		return Ok((ParsedSkillFrontMatter::default(), content));
	}

	let mut front_matter_lines = Vec::new();
	let mut body_offset = 4usize;
	for line in content[4..].lines() {
		body_offset = body_offset.saturating_add(line.len()).saturating_add(1);
		if line == "---" {
			let body = content.get(body_offset..).unwrap_or_default();
			let parsed =
				serde_yaml::from_str::<ParsedSkillFrontMatter>(&front_matter_lines.join("\n"))?;
			return Ok((parsed, body));
		}
		front_matter_lines.push(line);
	}

	Err(SkillRegistryError::FrontMatter(
		"opening front matter delimiter is missing a closing delimiter".to_string(),
	))
}

fn markdown_title(content: &str) -> Option<String> {
	content
		.lines()
		.find_map(|line| line.strip_prefix("# "))
		.map(|value| value.trim().to_string())
		.filter(|value| !value.is_empty())
}

fn infer_name_from_directory(source_dir: &Path) -> Option<String> {
	source_dir
		.file_name()
		.and_then(|value| value.to_str())
		.map(str::trim)
		.filter(|value| !value.is_empty())
		.map(str::to_string)
}

fn publish_skill_dir(
	root: &Path,
	stage_dir: &Path,
	final_dir: &Path,
) -> Result<(), SkillRegistryError> {
	if let Some(parent) = final_dir.parent() {
		fs::create_dir_all(parent)?;
	}
	if final_dir.exists() {
		let backup_dir = root.join("backups").join(format!(
			"{}-{}",
			final_dir
				.file_name()
				.and_then(|value| value.to_str())
				.unwrap_or("skill"),
			now_unix_ms()
		));
		fs::rename(final_dir, backup_dir)?;
	}
	fs::rename(stage_dir, final_dir)?;
	Ok(())
}

fn copy_dir_recursive(source_dir: &Path, target_dir: &Path) -> Result<(), SkillRegistryError> {
	for entry in WalkDir::new(source_dir) {
		let entry = entry.map_err(|error| SkillRegistryError::Io(std::io::Error::other(error)))?;
		let path = entry.path();
		let relative = path
			.strip_prefix(source_dir)
			.map_err(|error| SkillRegistryError::Io(std::io::Error::other(error)))?;
		let destination = target_dir.join(relative);
		if entry.file_type().is_dir() {
			fs::create_dir_all(&destination)?;
			continue;
		}
		if let Some(parent) = destination.parent() {
			fs::create_dir_all(parent)?;
		}
		fs::copy(path, destination)?;
	}
	Ok(())
}

fn list_relative_files(root: &Path) -> Result<Vec<String>, SkillRegistryError> {
	let mut files = WalkDir::new(root)
		.into_iter()
		.filter_map(Result::ok)
		.filter(|entry| entry.file_type().is_file())
		.map(|entry| {
			entry
				.path()
				.strip_prefix(root)
				.map(path_to_forward_slashes)
				.map_err(|error| SkillRegistryError::Io(std::io::Error::other(error)))
		})
		.collect::<Result<Vec<_>, _>>()?;
	files.sort();
	Ok(files)
}

fn write_record(root: &Path, record: &InstalledSkillRecord) -> Result<(), SkillRegistryError> {
	let record_path = root
		.join("registry")
		.join(format!("{}.json", storage_key(&record.descriptor.name)));
	write_text_atomically(&record_path, &serde_json::to_string_pretty(record)?)
}

fn render_skill_context(
	root: &Path,
	record: &InstalledSkillRecord,
	max_chars: usize,
) -> Result<String, SkillRegistryError> {
	let skill_dir = root.join(&record.install_dir);
	if !skill_dir.exists() {
		return Err(SkillRegistryError::SkillNotFound(
			record.descriptor.name.clone(),
		));
	}

	let entry_path = skill_dir.join(&record.descriptor.entrypoint);
	let entry_markdown = fs::read_to_string(&entry_path)?;
	if entry_markdown.len() > max_chars.max(MAX_PROMPT_DOCUMENT_BYTES) {
		return Err(SkillRegistryError::PromptDocumentTooLarge { path: entry_path });
	}

	let mut rendered = format!(
		"### skill: {}\nDescription: {}\nVersion: {}\nSource: {}\n\nEntrypoint (`{}`):\n{}",
		record.descriptor.name,
		record.descriptor.description,
		record.descriptor.version,
		record.source.original_url(),
		record.descriptor.entrypoint,
		entry_markdown.trim()
	);
	let mut documents = 0usize;
	for path in prompt_document_paths(&skill_dir)? {
		if documents >= MAX_PROMPT_DOCUMENTS || rendered.chars().count() >= max_chars {
			break;
		}
		let content = fs::read_to_string(&path)?;
		if content.len() > MAX_PROMPT_DOCUMENT_BYTES {
			continue;
		}
		let relative = path_to_forward_slashes(
			path.strip_prefix(&skill_dir)
				.map_err(|error| SkillRegistryError::Io(std::io::Error::other(error)))?,
		);
		let section = format!(
			"\n\nSupporting document (`{relative}`):\n{}",
			content.trim()
		);
		if rendered
			.chars()
			.count()
			.saturating_add(section.chars().count())
			> max_chars
		{
			break;
		}
		rendered.push_str(&section);
		documents = documents.saturating_add(1);
	}
	Ok(rendered)
}

fn prompt_document_paths(skill_dir: &Path) -> Result<Vec<PathBuf>, SkillRegistryError> {
	let mut paths = WalkDir::new(skill_dir)
		.into_iter()
		.filter_map(Result::ok)
		.filter(|entry| entry.file_type().is_file())
		.map(|entry| entry.path().to_path_buf())
		.filter(|path| {
			path.file_name().and_then(|value| value.to_str()) != Some("SKILL.md")
				&& supported_prompt_document(path)
		})
		.collect::<Vec<_>>();
	paths.sort();
	Ok(paths)
}

fn supported_prompt_document(path: &Path) -> bool {
	matches!(
		path.extension().and_then(|value| value.to_str()),
		Some("md" | "txt" | "json" | "yaml" | "yml" | "toml")
	)
}

fn query_mentions_skill(query: &str, skill_name: &str) -> bool {
	let normalized_query = normalize_skill_text(query);
	let normalized_name = normalize_skill_text(skill_name);
	!normalized_name.is_empty() && normalized_query.contains(&normalized_name)
}

fn normalize_skill_text(value: &str) -> String {
	value
		.chars()
		.map(|character| {
			if character.is_ascii_alphanumeric() {
				character.to_ascii_lowercase()
			} else {
				' '
			}
		})
		.collect::<String>()
		.split_whitespace()
		.collect::<Vec<_>>()
		.join(" ")
}

fn storage_key(name: &str) -> String {
	let normalized = normalize_skill_text(name).replace(' ', "-");
	if normalized.is_empty() {
		"skill".to_string()
	} else {
		normalized
	}
}

fn relative_display_path(root: &Path, path: &Path) -> String {
	path.strip_prefix(root)
		.map(path_to_forward_slashes)
		.unwrap_or_else(|_| display_path(path))
}

fn display_path(path: &Path) -> String {
	path.canonicalize()
		.unwrap_or_else(|_| path.to_path_buf())
		.display()
		.to_string()
}

fn path_to_forward_slashes(path: &Path) -> String {
	path.components()
		.map(|component| component.as_os_str().to_string_lossy().to_string())
		.collect::<Vec<_>>()
		.join("/")
}

fn write_text_atomically(path: &Path, content: &str) -> Result<(), SkillRegistryError> {
	if let Some(parent) = path.parent() {
		fs::create_dir_all(parent)?;
	}
	let temp_path = path.with_extension(format!(
		"{}.tmp",
		path.extension()
			.and_then(|value| value.to_str())
			.unwrap_or("json")
	));
	fs::write(&temp_path, content)?;
	fs::rename(temp_path, path)?;
	Ok(())
}

fn now_unix_ms() -> u64 {
	SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.unwrap_or_default()
		.as_millis()
		.try_into()
		.unwrap_or(u64::MAX)
}

fn log_skill_event(message: &str, fields: impl IntoIterator<Item = (&'static str, String)>) {
	let mut record = LogRecord::new("roku-skill-registry", LogLevel::Info, message);
	for (key, value) in fields {
		record = record.with_field(key, value);
	}
	let _ = emit_global_log(record);
}

#[cfg(test)]
mod tests {
	use std::io::{Cursor, Write};
	use std::sync::Arc;

	use super::*;

	#[derive(Clone)]
	struct StaticArchiveFetcher {
		archive: DownloadedArchive,
	}

	impl SkillArchiveFetcher for StaticArchiveFetcher {
		fn fetch(&self, _source: &SkillSource) -> Result<DownloadedArchive, SkillRegistryError> {
			Ok(self.archive.clone())
		}
	}

	#[test]
	fn installs_skill_from_github_subtree_archive() {
		let root = tempfile::tempdir().expect("temp root should exist");
		let registry = SkillRegistry::file_backed(root.path().join("skills")).with_fetcher(
			Arc::new(StaticArchiveFetcher {
				archive: DownloadedArchive {
					archive_url: "https://github.com/anthropics/skills/archive/refs/heads/main.zip"
						.to_string(),
					bytes: test_skill_archive_bytes(),
					resolved_reference: Some("main".to_string()),
				},
			}),
		);

		let report = registry
			.install_from_url(
				"https://github.com/anthropics/skills/tree/main/skills/claude-api",
				"test-suite",
			)
			.expect("install should succeed");
		assert_eq!(report.skill_name, "claude-api");
		assert!(report.message.contains("Reference `claude-api`"));
		assert!(report.installed_files.contains(&"SKILL.md".to_string()));

		let records = registry.list_skills().expect("list should succeed");
		assert_eq!(records.len(), 1);
		assert_eq!(records[0].descriptor.name, "claude-api");
	}

	#[test]
	fn renders_prompt_context_when_skill_name_is_referenced() {
		let root = tempfile::tempdir().expect("temp root should exist");
		let registry = SkillRegistry::file_backed(root.path().join("skills")).with_fetcher(
			Arc::new(StaticArchiveFetcher {
				archive: DownloadedArchive {
					archive_url: "https://github.com/anthropics/skills/archive/refs/heads/main.zip"
						.to_string(),
					bytes: test_skill_archive_bytes(),
					resolved_reference: Some("main".to_string()),
				},
			}),
		);
		registry
			.install_from_url(
				"https://github.com/anthropics/skills/tree/main/skills/claude-api",
				"test-suite",
			)
			.expect("install should succeed");

		let context = registry
			.render_prompt_context_for_query("Please use the claude-api skill", 16_000)
			.expect("context should render")
			.expect("context should exist");
		assert!(context.contains("### skill: claude-api"));
		assert!(context.contains("Entrypoint (`SKILL.md`)"));
		assert!(context.contains("Supporting document (`shared/models.md`)"));
	}

	#[test]
	fn does_not_match_unreferenced_skill_names() {
		let root = tempfile::tempdir().expect("temp root should exist");
		let registry = SkillRegistry::file_backed(root.path().join("skills")).with_fetcher(
			Arc::new(StaticArchiveFetcher {
				archive: DownloadedArchive {
					archive_url: "https://example.com/archive.zip".to_string(),
					bytes: test_skill_archive_bytes(),
					resolved_reference: Some("main".to_string()),
				},
			}),
		);
		registry
			.install_from_url(
				"https://github.com/anthropics/skills/tree/main/skills/claude-api",
				"test-suite",
			)
			.expect("install should succeed");

		let context = registry
			.render_prompt_context_for_query("Please explain the weather", 8_000)
			.expect("context lookup should succeed");
		assert!(context.is_none());
	}

	fn test_skill_archive_bytes() -> Vec<u8> {
		let mut cursor = Cursor::new(Vec::new());
		{
			let mut writer = zip::ZipWriter::new(&mut cursor);
			let options = zip::write::SimpleFileOptions::default();
			writer
				.add_directory("skills-main/skills/claude-api/", options)
				.expect("dir should be added");
			writer
				.add_directory("skills-main/skills/claude-api/shared/", options)
				.expect("shared dir should be added");
			writer
				.start_file("skills-main/skills/claude-api/SKILL.md", options)
				.expect("skill file should start");
			writer
				.write_all(
					br#"---
name: claude-api
description: Build apps with the Claude API.
license: Apache-2.0
---

# Claude API Skill

Use this skill when the user explicitly asks for Claude API integration help.
"#,
				)
				.expect("skill markdown should write");
			writer
				.start_file("skills-main/skills/claude-api/shared/models.md", options)
				.expect("support file should start");
			writer
				.write_all(b"Use claude-opus-4-6 unless the user asks otherwise.")
				.expect("support file should write");
			writer.finish().expect("zip should finish");
		}
		cursor.into_inner()
	}
}
