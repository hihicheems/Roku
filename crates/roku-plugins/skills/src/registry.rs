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

use std::env;
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::blocking::Client;
use roku_observability::{LogLevel, LogRecord, emit_global_log};
use roku_plugin_catalog::{CatalogDescriptor, ResourceCost, ResourceKind, ResourceRisk};
use serde::Deserialize;
use walkdir::WalkDir;

use crate::{
	InstalledSkillRecord, SkillDescriptor, SkillInstallReport, SkillRegistryError, SkillSource,
};

const GITHUB_API_BASE: &str = "https://api.github.com";
const INSTALLER_USER_AGENT: &str = "RokuSkillInstaller/0.1";
const HARD_MAX_HTTP_TIMEOUT_SECONDS: u64 = 300;
const HARD_MAX_ARCHIVE_BYTES: u64 = 128 * 1024 * 1024;
const HARD_MAX_PROMPT_DOCUMENT_BYTES: usize = 128 * 1024;
const HARD_MAX_PROMPT_DOCUMENTS: usize = 128;

/// Effective non-secret runtime configuration for the skill registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillsRuntimeConfig {
	pub http_timeout_seconds: u64,
	pub max_archive_bytes: u64,
	pub max_prompt_document_bytes: usize,
	pub max_prompt_documents: usize,
}

/// Partial overrides for [`SkillsRuntimeConfig`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillsRuntimeConfigPatch {
	pub http_timeout_seconds: Option<u64>,
	pub max_archive_bytes: Option<u64>,
	pub max_prompt_document_bytes: Option<usize>,
	pub max_prompt_documents: Option<usize>,
}

#[derive(Debug, Default, Deserialize)]
struct ParsedSkillFrontMatter {
	name: Option<String>,
	description: Option<String>,
	license: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ParsedOpenAiAgentMetadata {
	#[serde(default)]
	interface: ParsedOpenAiInterface,
}

#[derive(Debug, Default, Deserialize)]
struct ParsedOpenAiInterface {
	display_name: Option<String>,
	short_description: Option<String>,
	default_prompt: Option<String>,
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
	config: SkillsRuntimeConfig,
}

pub trait SkillArchiveFetcher: Send + Sync {
	fn fetch(&self, source: &SkillSource) -> Result<DownloadedArchive, SkillRegistryError>;
}

#[derive(Clone, Default)]
struct DisabledSkillArchiveFetcher;

impl SkillArchiveFetcher for DisabledSkillArchiveFetcher {
	fn fetch(&self, _source: &SkillSource) -> Result<DownloadedArchive, SkillRegistryError> {
		Err(SkillRegistryError::RegistryDisabled)
	}
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
	max_archive_bytes: u64,
}

impl Default for HttpSkillArchiveFetcher {
	fn default() -> Self {
		Self::from_runtime_config(&SkillsRuntimeConfig::default())
	}
}

impl Default for SkillsRuntimeConfig {
	fn default() -> Self {
		Self {
			http_timeout_seconds: 30,
			max_archive_bytes: 32 * 1024 * 1024,
			max_prompt_document_bytes: 24 * 1024,
			max_prompt_documents: 24,
		}
	}
}

impl SkillsRuntimeConfig {
	pub fn apply_patch(&mut self, patch: SkillsRuntimeConfigPatch) {
		if let Some(value) = patch.http_timeout_seconds {
			self.http_timeout_seconds = value;
		}
		if let Some(value) = patch.max_archive_bytes {
			self.max_archive_bytes = value;
		}
		if let Some(value) = patch.max_prompt_document_bytes {
			self.max_prompt_document_bytes = value;
		}
		if let Some(value) = patch.max_prompt_documents {
			self.max_prompt_documents = value;
		}
	}

	pub fn apply_env_overrides(&mut self) -> Result<(), SkillRegistryError> {
		if let Some(value) = env_override_u64("ROKU_RUNTIME__SKILLS__HTTP_TIMEOUT_SECONDS")? {
			self.http_timeout_seconds = value;
		}
		if let Some(value) = env_override_u64("ROKU_RUNTIME__SKILLS__MAX_ARCHIVE_BYTES")? {
			self.max_archive_bytes = value;
		}
		if let Some(value) = env_override_usize("ROKU_RUNTIME__SKILLS__MAX_PROMPT_DOCUMENT_BYTES")?
		{
			self.max_prompt_document_bytes = value;
		}
		if let Some(value) = env_override_usize("ROKU_RUNTIME__SKILLS__MAX_PROMPT_DOCUMENTS")? {
			self.max_prompt_documents = value;
		}
		Ok(())
	}

	pub fn validate_and_clamp(&mut self) -> Result<(), SkillRegistryError> {
		if self.http_timeout_seconds == 0 {
			return Err(SkillRegistryError::RuntimeConfig(
				"runtime.skills.http_timeout_seconds must be greater than zero".to_string(),
			));
		}
		if self.max_archive_bytes == 0 {
			return Err(SkillRegistryError::RuntimeConfig(
				"runtime.skills.max_archive_bytes must be greater than zero".to_string(),
			));
		}
		if self.max_prompt_document_bytes == 0 {
			return Err(SkillRegistryError::RuntimeConfig(
				"runtime.skills.max_prompt_document_bytes must be greater than zero".to_string(),
			));
		}
		if self.max_prompt_documents == 0 {
			return Err(SkillRegistryError::RuntimeConfig(
				"runtime.skills.max_prompt_documents must be greater than zero".to_string(),
			));
		}
		self.http_timeout_seconds = self.http_timeout_seconds.min(HARD_MAX_HTTP_TIMEOUT_SECONDS);
		self.max_archive_bytes = self.max_archive_bytes.min(HARD_MAX_ARCHIVE_BYTES);
		self.max_prompt_document_bytes = self
			.max_prompt_document_bytes
			.min(HARD_MAX_PROMPT_DOCUMENT_BYTES);
		self.max_prompt_documents = self.max_prompt_documents.min(HARD_MAX_PROMPT_DOCUMENTS);
		Ok(())
	}
}

impl HttpSkillArchiveFetcher {
	pub fn from_runtime_config(config: &SkillsRuntimeConfig) -> Self {
		let client = Client::builder()
			.user_agent(INSTALLER_USER_AGENT)
			.timeout(Duration::from_secs(config.http_timeout_seconds))
			.build()
			.expect("skill installer client should build");
		Self {
			client,
			max_archive_bytes: config.max_archive_bytes,
		}
	}
}

fn env_override_string(key: &str) -> Option<String> {
	env::var(key)
		.ok()
		.map(|value| value.trim().to_string())
		.filter(|value| !value.is_empty())
}

fn env_override_u64(key: &'static str) -> Result<Option<u64>, SkillRegistryError> {
	match env_override_string(key) {
		Some(value) => value.parse::<u64>().map(Some).map_err(|error| {
			SkillRegistryError::RuntimeConfig(format!("{key} is invalid: {error}"))
		}),
		None => Ok(None),
	}
}

fn env_override_usize(key: &'static str) -> Result<Option<usize>, SkillRegistryError> {
	match env_override_string(key) {
		Some(value) => value.parse::<usize>().map(Some).map_err(|error| {
			SkillRegistryError::RuntimeConfig(format!("{key} is invalid: {error}"))
		}),
		None => Ok(None),
	}
}

impl SkillRegistry {
	pub fn disabled() -> Self {
		Self {
			backend: SkillRegistryBackend::Disabled,
			fetcher: Arc::new(DisabledSkillArchiveFetcher),
			config: SkillsRuntimeConfig::default(),
		}
	}

	pub fn file_backed(root: impl Into<PathBuf>) -> Self {
		Self::file_backed_with_config(root, SkillsRuntimeConfig::default())
	}

	pub fn file_backed_with_config(root: impl Into<PathBuf>, config: SkillsRuntimeConfig) -> Self {
		Self {
			backend: SkillRegistryBackend::FileBacked { root: root.into() },
			fetcher: Arc::new(HttpSkillArchiveFetcher::from_runtime_config(&config)),
			config,
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
		self.ensure_installed_from_url(source_url, activated_by)
	}

	pub fn ensure_installed_from_url(
		&self,
		source_url: &str,
		activated_by: &str,
	) -> Result<SkillInstallReport, SkillRegistryError> {
		let root = self.root_path()?;
		ensure_registry_layout(root)?;

		let source = SkillSource::parse(source_url)?;
		if let Some(existing) = self
			.list_skills()?
			.into_iter()
			.find(|record| skill_source_matches(&record.source, &source))
		{
			let install_dir = resolve_record_install_dir(root, &existing);
			let install_dir = display_path(&install_dir);
			let message = format!(
				"Skill `{}` is already installed from {} at {}. Reference `{}` in future requests to activate it.",
				existing.descriptor.name,
				source.original_url(),
				install_dir,
				existing.descriptor.name,
			);
			log_skill_event(
				"skill already installed",
				[
					("skill_name", existing.descriptor.name.clone()),
					("source_url", source.original_url().to_string()),
					("install_dir", install_dir.clone()),
				],
			);

			return Ok(SkillInstallReport {
				skill_name: existing.descriptor.name,
				version: existing.descriptor.version,
				source_url: source.original_url().to_string(),
				install_dir,
				installed_files: existing.installed_files,
				activated_by: activated_by.to_string(),
				message,
			});
		}
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

	pub fn register_local_skill(
		&self,
		skill_dir: impl AsRef<Path>,
		activated_by: &str,
	) -> Result<SkillInstallReport, SkillRegistryError> {
		let root = self.root_path()?;
		ensure_registry_layout(root)?;

		let skill_dir = skill_dir.as_ref();
		if !skill_dir.exists() || !skill_dir.is_dir() {
			return Err(SkillRegistryError::InvalidSkillRoot(
				skill_dir.to_path_buf(),
			));
		}
		let skill_dir = skill_dir.canonicalize()?;
		let descriptor = load_descriptor(&skill_dir, Some("local"))?;
		let source = SkillSource::local_path(display_path(&skill_dir));
		if let Some(existing) = self
			.read_registry_records(root)?
			.into_iter()
			.find(|record| skill_source_matches(&record.source, &source))
		{
			let install_dir = resolve_record_install_dir(root, &existing);
			let install_dir = display_path(&install_dir);
			let message = format!(
				"Skill `{}` is already registered from local path {}. Reference `{}` in future requests to activate it.",
				existing.descriptor.name, install_dir, existing.descriptor.name,
			);
			return Ok(SkillInstallReport {
				skill_name: existing.descriptor.name,
				version: existing.descriptor.version,
				source_url: source.original_url().to_string(),
				install_dir,
				installed_files: existing.installed_files,
				activated_by: activated_by.to_string(),
				message,
			});
		}

		let installed_files = list_relative_files(&skill_dir)?;
		let record = InstalledSkillRecord {
			descriptor: descriptor.clone(),
			source: source.clone(),
			installed_at_unix_ms: now_unix_ms(),
			install_dir: display_path(&skill_dir),
			installed_files: installed_files.clone(),
		};
		write_record(root, &record)?;
		let install_dir = display_path(&skill_dir);
		let message = format!(
			"Registered local skill `{}` at {}. Reference `{}` in future requests to activate it.",
			descriptor.name, install_dir, descriptor.name,
		);
		log_skill_event(
			"registered local skill",
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

	pub fn referenced_skill_names(&self, query: &str) -> Result<Vec<String>, SkillRegistryError> {
		let mut names = self
			.list_skills()?
			.into_iter()
			.filter(|record| query_mentions_skill(query, &record.descriptor.name))
			.map(|record| record.descriptor.name)
			.collect::<Vec<_>>();
		names.sort();
		names.dedup();
		Ok(names)
	}

	pub fn resolve_installed_skill_name(
		&self,
		candidate: &str,
	) -> Result<Option<String>, SkillRegistryError> {
		Ok(self
			.list_skills()?
			.into_iter()
			.find(|record| skill_name_matches(candidate, &record.descriptor.name))
			.map(|record| record.descriptor.name))
	}

	pub fn list_skills(&self) -> Result<Vec<InstalledSkillRecord>, SkillRegistryError> {
		let Some(root) = self.optional_root_path() else {
			return Ok(Vec::new());
		};
		let mut merged = std::collections::BTreeMap::<String, InstalledSkillRecord>::new();
		for record in self.discover_skill_records(root)? {
			merged.insert(normalize_skill_text(&record.descriptor.name), record);
		}
		for record in self.read_registry_records(root)? {
			merged.insert(normalize_skill_text(&record.descriptor.name), record);
		}

		let mut records = merged.into_values().collect::<Vec<_>>();
		records.sort_by(|left, right| left.descriptor.name.cmp(&right.descriptor.name));
		Ok(records)
	}

	pub fn get_skill(&self, skill_name: &str) -> Result<InstalledSkillRecord, SkillRegistryError> {
		self.list_skills()?
			.into_iter()
			.find(|record| skill_name_matches(skill_name, &record.descriptor.name))
			.ok_or_else(|| SkillRegistryError::SkillNotFound(skill_name.to_string()))
	}

	pub fn skill_dir(&self, skill_name: &str) -> Result<PathBuf, SkillRegistryError> {
		let root = self.root_path()?;
		let record = self.get_skill(skill_name)?;
		Ok(resolve_record_install_dir(root, &record))
	}

	pub fn render_prompt_context_for_skill(
		&self,
		skill_name: &str,
		max_chars: usize,
	) -> Result<String, SkillRegistryError> {
		let root = self.root_path()?;
		if max_chars == 0 {
			return Ok(String::new());
		}

		let record = self.get_skill(skill_name)?;
		render_skill_context(root, &record, max_chars, &self.config)
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
			let context =
				render_skill_context_for_query(root, &record, query, remaining, &self.config)?;
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

	pub fn catalog_descriptors(&self) -> Result<Vec<CatalogDescriptor>, SkillRegistryError> {
		let Some(root) = self.optional_root_path() else {
			return Ok(Vec::new());
		};

		self.list_skills()?
			.into_iter()
			.map(|record| build_skill_catalog_descriptor(root, &record))
			.collect()
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

	fn read_registry_records(
		&self,
		root: &Path,
	) -> Result<Vec<InstalledSkillRecord>, SkillRegistryError> {
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

	fn discover_skill_records(
		&self,
		root: &Path,
	) -> Result<Vec<InstalledSkillRecord>, SkillRegistryError> {
		let mut discovered = scan_skill_dirs(root, root, true)?;
		discovered.extend(scan_skill_dirs(root, &root.join("installed"), false)?);
		discovered.sort_by(|left, right| left.descriptor.name.cmp(&right.descriptor.name));
		Ok(discovered)
	}
}

impl SkillArchiveFetcher for HttpSkillArchiveFetcher {
	fn fetch(&self, source: &SkillSource) -> Result<DownloadedArchive, SkillRegistryError> {
		match source {
			SkillSource::LocalPath { path, .. } => Err(SkillRegistryError::InvalidSourceUrl(
				format!("local skill path cannot be fetched as an archive: {path}"),
			)),
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
			&& length > self.max_archive_bytes
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
		if size_bytes > self.max_archive_bytes {
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

fn scan_skill_dirs(
	root: &Path,
	scan_root: &Path,
	skip_reserved_names: bool,
) -> Result<Vec<InstalledSkillRecord>, SkillRegistryError> {
	if !scan_root.exists() || !scan_root.is_dir() {
		return Ok(Vec::new());
	}

	let mut records = Vec::new();
	for entry in fs::read_dir(scan_root)? {
		let entry = entry?;
		if !entry.file_type()?.is_dir() {
			continue;
		}
		let skill_dir = entry.path();
		let Some(directory_name) = skill_dir.file_name().and_then(|value| value.to_str()) else {
			continue;
		};
		if skip_reserved_names && is_reserved_skill_storage_dir(directory_name) {
			continue;
		}
		if !skill_dir.join("SKILL.md").exists() {
			continue;
		}
		records.push(discovered_skill_record(root, &skill_dir)?);
	}

	Ok(records)
}

fn discovered_skill_record(
	root: &Path,
	skill_dir: &Path,
) -> Result<InstalledSkillRecord, SkillRegistryError> {
	let descriptor = load_descriptor(skill_dir, Some("local"))?;
	let installed_files = list_relative_files(skill_dir)?;
	Ok(InstalledSkillRecord {
		descriptor,
		source: SkillSource::local_path(display_path(skill_dir)),
		installed_at_unix_ms: path_modified_unix_ms(skill_dir),
		install_dir: relative_display_path(root, skill_dir),
		installed_files,
	})
}

fn is_reserved_skill_storage_dir(name: &str) -> bool {
	matches!(name, "installed" | "registry" | "cache" | "backups")
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

fn build_skill_catalog_descriptor(
	root: &Path,
	record: &InstalledSkillRecord,
) -> Result<CatalogDescriptor, SkillRegistryError> {
	let skill_dir = resolve_record_install_dir(root, record);
	let manifest_path = skill_dir.join(&record.descriptor.entrypoint);
	let content = fs::read_to_string(manifest_path)?;
	let (_, body) = split_front_matter(&content)?;
	let ParsedOpenAiInterface {
		display_name,
		short_description,
		default_prompt,
	} = load_openai_agent_metadata(&skill_dir)?;
	let mut summary_parts = vec![skill_summary(body)];
	if let Some(short_description) = short_description.as_deref()
		&& !short_description.trim().is_empty()
	{
		summary_parts.push(short_description.trim().to_string());
	}
	let summary = summary_parts
		.into_iter()
		.filter(|value| !value.trim().is_empty())
		.collect::<Vec<_>>()
		.join(" ");
	let key_commands = extract_key_commands(body);
	let mut use_cases = extract_use_cases(body);
	if let Some(default_prompt) = default_prompt.as_deref()
		&& !default_prompt.trim().is_empty()
	{
		use_cases.push(default_prompt.trim().to_string());
	}
	let tags = extract_skill_tags(&record.descriptor, &summary, &use_cases, &key_commands);
	let mut tags = tags;
	if let Some(display_name) = display_name.as_deref() {
		tags.extend(tokenize_catalog_text(display_name));
	}
	if let Some(short_description) = short_description.as_deref() {
		tags.extend(tokenize_catalog_text(short_description));
	}
	if let Some(default_prompt) = default_prompt.as_deref() {
		tags.extend(tokenize_catalog_text(default_prompt));
	}
	if record.has_scripts() {
		tags.push("executable-skill".to_string());
		tags.push("has-scripts".to_string());
	}
	tags.sort();
	tags.dedup();
	let mut examples = key_commands.iter().take(3).cloned().collect::<Vec<_>>();
	if let Some(display_name) = display_name
		&& !display_name.trim().is_empty()
	{
		examples.push(display_name.trim().to_string());
	}
	if let Some(short_description) = short_description
		&& !short_description.trim().is_empty()
	{
		examples.push(short_description.trim().to_string());
	}
	examples.truncate(3);

	Ok(CatalogDescriptor {
		selector: roku_common_types::ResourceSelector::skill(record.descriptor.name.clone()),
		kind: ResourceKind::Skill,
		name: record.descriptor.name.clone(),
		role: None,
		discoverable: true,
		description: record.descriptor.description.clone(),
		tags,
		examples,
		input_schema: Vec::new(),
		risk: ResourceRisk::Low,
		cost: ResourceCost::default(),
		required_capabilities: Vec::new(),
		summary,
		key_commands,
		use_cases,
	})
}

fn load_openai_agent_metadata(
	skill_dir: &Path,
) -> Result<ParsedOpenAiInterface, SkillRegistryError> {
	let metadata_path = skill_dir.join("agents").join("openai.yaml");
	if !metadata_path.exists() {
		return Ok(ParsedOpenAiInterface::default());
	}

	let metadata = fs::read_to_string(metadata_path)?;
	let parsed = serde_yaml::from_str::<ParsedOpenAiAgentMetadata>(&metadata)?;
	Ok(parsed.interface)
}

fn skill_summary(body: &str) -> String {
	let mut lines = body
		.lines()
		.map(str::trim)
		.filter(|line| !line.is_empty() && !line.starts_with('#'));
	let first = lines.next().unwrap_or_default().to_string();
	let second = lines.next().unwrap_or_default().to_string();

	if second.is_empty() {
		first
	} else if first.is_empty() {
		second
	} else {
		format!("{first} {second}")
	}
}

fn extract_key_commands(body: &str) -> Vec<String> {
	let mut commands = Vec::new();
	for line in body.lines().map(str::trim).filter(|line| !line.is_empty()) {
		if line.starts_with("```") {
			continue;
		}
		if line.starts_with('-') && line.contains('`') {
			commands.extend(extract_backtick_segments(line));
		} else if line.starts_with('$') || line.starts_with("./") {
			commands.push(line.to_string());
		}
		if commands.len() >= 6 {
			break;
		}
	}
	commands.sort();
	commands.dedup();
	commands
}

fn extract_use_cases(body: &str) -> Vec<String> {
	body.lines()
		.map(str::trim)
		.filter(|line| line.starts_with("- ") || line.starts_with("* "))
		.map(|line| line.trim_start_matches(['-', '*']).trim().to_string())
		.filter(|line| !line.is_empty())
		.take(6)
		.collect()
}

fn extract_skill_tags(
	descriptor: &SkillDescriptor,
	summary: &str,
	use_cases: &[String],
	key_commands: &[String],
) -> Vec<String> {
	let mut tags = tokenize_catalog_text(&descriptor.name);
	tags.extend(tokenize_catalog_text(&descriptor.description));
	tags.extend(tokenize_catalog_text(summary));
	for use_case in use_cases {
		tags.extend(tokenize_catalog_text(use_case));
	}
	for command in key_commands {
		tags.extend(tokenize_catalog_text(command));
	}
	tags.sort();
	tags.dedup();
	tags
}

fn tokenize_catalog_text(text: &str) -> Vec<String> {
	let mut tokens = Vec::new();
	let mut current = String::new();
	for character in text.chars() {
		if character.is_ascii_alphanumeric() {
			current.push(character.to_ascii_lowercase());
		} else if !current.is_empty() {
			tokens.push(current.clone());
			current.clear();
		}
	}
	if !current.is_empty() {
		tokens.push(current);
	}
	tokens
}

fn extract_backtick_segments(line: &str) -> Vec<String> {
	let mut segments = Vec::new();
	let mut in_segment = false;
	let mut current = String::new();
	for character in line.chars() {
		if character == '`' {
			if in_segment && !current.trim().is_empty() {
				segments.push(current.trim().to_string());
				current.clear();
			}
			in_segment = !in_segment;
			continue;
		}
		if in_segment {
			current.push(character);
		}
	}
	segments
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

fn resolve_record_install_dir(root: &Path, record: &InstalledSkillRecord) -> PathBuf {
	let install_dir = PathBuf::from(&record.install_dir);
	let resolved = if install_dir.is_absolute() {
		install_dir
	} else {
		root.join(
			install_dir
				.strip_prefix("./")
				.unwrap_or(install_dir.as_path()),
		)
	};
	normalize_existing_path(resolved)
}

fn render_skill_context(
	root: &Path,
	record: &InstalledSkillRecord,
	max_chars: usize,
	config: &SkillsRuntimeConfig,
) -> Result<String, SkillRegistryError> {
	render_skill_context_with_keywords(root, record, max_chars, &[], config)
}

fn render_skill_context_for_query(
	root: &Path,
	record: &InstalledSkillRecord,
	query: &str,
	max_chars: usize,
	config: &SkillsRuntimeConfig,
) -> Result<String, SkillRegistryError> {
	let keywords = if should_focus_query_context(query) {
		query_keywords(query)
	} else {
		Vec::new()
	};
	render_skill_context_with_keywords(root, record, max_chars, &keywords, config)
}

fn render_skill_context_with_keywords(
	root: &Path,
	record: &InstalledSkillRecord,
	max_chars: usize,
	keywords: &[String],
	config: &SkillsRuntimeConfig,
) -> Result<String, SkillRegistryError> {
	let skill_dir = resolve_record_install_dir(root, record);
	if !skill_dir.exists() {
		return Err(SkillRegistryError::SkillNotFound(
			record.descriptor.name.clone(),
		));
	}

	let entry_path = skill_dir.join(&record.descriptor.entrypoint);
	let entry_markdown = fs::read_to_string(&entry_path)?;
	let supporting_paths = prompt_document_paths(&skill_dir)?;
	let supporting_budget = if supporting_paths.is_empty() {
		0
	} else {
		max_chars.min(4_096) / 4
	};

	let header = format!(
		"### skill: {}\nDescription: {}\nVersion: {}\nSource: {}\n\nEntrypoint (`{}`):\n{}",
		record.descriptor.name,
		record.descriptor.description,
		record.descriptor.version,
		record.source.original_url(),
		record.descriptor.entrypoint,
		""
	);
	let header_budget = header.chars().count();
	if header_budget >= max_chars {
		return Ok(truncate_with_notice(
			&header,
			max_chars,
			"[skill context truncated]",
		));
	}
	let entry_budget = max_chars
		.saturating_sub(header_budget)
		.saturating_sub(supporting_budget)
		.max(512);
	let entry_excerpt =
		document_excerpt(&entry_markdown, entry_budget, &entry_path, keywords, config)?;
	let mut covered_keywords = excerpt_keywords(&entry_excerpt, keywords);
	let mut rendered = format!(
		"### skill: {}\nDescription: {}\nVersion: {}\nSource: {}\n\nEntrypoint (`{}`):\n{}",
		record.descriptor.name,
		record.descriptor.description,
		record.descriptor.version,
		record.source.original_url(),
		record.descriptor.entrypoint,
		entry_excerpt.trim()
	);
	let mut documents = 0usize;
	for path in supporting_paths {
		if documents >= config.max_prompt_documents || rendered.chars().count() >= max_chars {
			break;
		}
		let content = fs::read_to_string(&path)?;
		let relative = path_to_forward_slashes(
			path.strip_prefix(&skill_dir)
				.map_err(|error| SkillRegistryError::Io(std::io::Error::other(error)))?,
		);
		let remaining = max_chars.saturating_sub(rendered.chars().count());
		if remaining < 128 {
			break;
		}
		let excerpt = document_excerpt(
			&content,
			remaining.saturating_sub(64),
			&path,
			keywords,
			config,
		)?;
		if keywords.len() >= 4 {
			let excerpt_keywords = excerpt_keywords(&excerpt, keywords);
			if !excerpt_keywords.is_empty()
				&& excerpt_keywords
					.iter()
					.all(|keyword| covered_keywords.contains(keyword))
			{
				continue;
			}
			covered_keywords.extend(excerpt_keywords);
		}
		let section = format!(
			"\n\nSupporting document (`{relative}`):\n{}",
			excerpt.trim()
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

fn excerpt_keywords(excerpt: &str, keywords: &[String]) -> std::collections::BTreeSet<String> {
	let excerpt = excerpt.to_ascii_lowercase();
	keywords
		.iter()
		.filter(|keyword| excerpt.contains(keyword.as_str()))
		.cloned()
		.collect()
}

fn document_excerpt(
	content: &str,
	max_chars: usize,
	_path: &Path,
	keywords: &[String],
	config: &SkillsRuntimeConfig,
) -> Result<String, SkillRegistryError> {
	if max_chars == 0 {
		return Ok(String::new());
	}
	if content.is_empty() {
		return Ok(String::new());
	}
	let limit = max_chars.min(config.max_prompt_document_bytes);
	if limit == 0 {
		return Ok(String::new());
	}
	if let Some(focused) = focused_excerpt(content, limit, keywords) {
		return Ok(focused);
	}
	if content.chars().count() <= limit {
		return Ok(content.to_string());
	}
	if limit < 96 {
		return Ok(truncate_with_notice(content, limit, "[truncated]"));
	}
	Ok(truncate_with_notice(
		content,
		limit,
		"[truncated for prompt budget]",
	))
}

fn truncate_with_notice(content: &str, max_chars: usize, notice: &str) -> String {
	if max_chars == 0 {
		return String::new();
	}
	let content_chars = content.chars().count();
	if content_chars <= max_chars {
		return content.to_string();
	}
	let notice = format!("\n\n{notice}");
	let notice_chars = notice.chars().count();
	if max_chars <= notice_chars {
		return content.chars().take(max_chars).collect();
	}
	let keep_chars = max_chars - notice_chars;
	let mut truncated = content.chars().take(keep_chars).collect::<String>();
	while truncated
		.chars()
		.last()
		.map(char::is_whitespace)
		.unwrap_or(false)
	{
		truncated.pop();
	}
	truncated.push_str(&notice);
	truncated
}

fn focused_excerpt(content: &str, max_chars: usize, keywords: &[String]) -> Option<String> {
	let keywords = keywords
		.iter()
		.map(|keyword| keyword.trim())
		.filter(|keyword| keyword.len() >= 3)
		.collect::<Vec<_>>();
	if keywords.is_empty() {
		return None;
	}

	let lines = content.lines().collect::<Vec<_>>();
	if lines.is_empty() {
		return None;
	}
	let lower_lines = lines
		.iter()
		.map(|line| line.to_ascii_lowercase())
		.collect::<Vec<_>>();
	let mut matches = Vec::new();
	for (index, line) in lower_lines.iter().enumerate() {
		let matched_keywords = keywords
			.iter()
			.filter(|keyword| line.contains(**keyword))
			.map(|keyword| (*keyword).to_string())
			.collect::<Vec<_>>();
		if !matched_keywords.is_empty() {
			matches.push((index, matched_keywords));
		}
	}
	if matches.is_empty() {
		return None;
	}

	let mut candidates = Vec::new();
	for (index, matched_keywords) in matches {
		let line = lines[index];
		let unique_keywords = matched_keywords
			.into_iter()
			.collect::<std::collections::BTreeSet<_>>();
		let mut score = unique_keywords.len().saturating_mul(10);
		if line.contains('`') {
			score = score.saturating_add(6);
		}
		if line.contains("/outputs/") {
			score = score.saturating_add(6);
		}
		let normalized_line = line.to_ascii_lowercase();
		if normalized_line.contains("field") {
			score = score.saturating_add(4);
		}
		if normalized_line.contains("expectation") {
			score = score.saturating_add(3);
		}
		if normalized_line.contains("baseline") {
			score = score.saturating_add(2);
		}
		if normalized_line.contains("creating a new skill") {
			score = score.saturating_add(8);
		}
		if normalized_line.contains("improving an existing skill") {
			score = score.saturating_add(8);
		}
		if normalized_line.contains("grading.json expectations array must use the fields") {
			score = score.saturating_add(12);
		}
		if line.contains("`text`") || line.contains("`passed`") || line.contains("`evidence`") {
			score = score.saturating_add(12);
		}
		if line.contains("`without_skill/outputs/`") || line.contains("`old_skill/outputs/`") {
			score = score.saturating_add(12);
		}
		candidates.push((index, score));
	}
	candidates.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));

	let mut selected = candidates
		.into_iter()
		.take(3)
		.map(|(index, _)| index)
		.collect::<Vec<_>>();
	let needs_schema_line = keywords.iter().any(|keyword| {
		matches!(
			*keyword,
			"grading" | "json" | "expectation" | "expectations" | "field" | "fields"
		)
	});
	if needs_schema_line {
		selected.extend(lines.iter().enumerate().filter_map(|(index, line)| {
			(line.contains("`text`") && line.contains("`passed`") && line.contains("`evidence`"))
				.then_some(index)
		}));
	}
	let needs_baseline_lines = keywords.iter().any(|keyword| {
		matches!(
			*keyword,
			"baseline" | "creating" | "improving" | "existing" | "new"
		)
	});
	if needs_baseline_lines {
		selected.extend(lines.iter().enumerate().filter_map(|(index, line)| {
			(line.contains("`without_skill/outputs/`") || line.contains("`old_skill/outputs/`"))
				.then_some(index)
		}));
	}
	selected.sort_unstable();
	selected.dedup();
	selected.sort_by_key(|index| excerpt_line_priority(lines[*index]));

	if selected.is_empty() {
		return None;
	}
	let snippet = selected
		.into_iter()
		.map(|line_index| compress_excerpt_line(lines[line_index]))
		.collect::<Vec<_>>()
		.join("\n");
	if snippet.is_empty() {
		None
	} else {
		Some(truncate_with_notice(
			&snippet,
			max_chars,
			"[truncated for prompt budget]",
		))
	}
}

fn excerpt_line_priority(line: &str) -> u8 {
	if line.contains("`text`")
		|| line
			.to_ascii_lowercase()
			.contains("grading.json expectations")
	{
		0
	} else if line.contains("`without_skill/outputs/`") {
		1
	} else if line.contains("`old_skill/outputs/`") {
		2
	} else {
		3
	}
}

fn compress_excerpt_line(line: &str) -> String {
	if line.contains("`text`") && line.contains("`passed`") && line.contains("`evidence`") {
		return "The grading.json expectations array must use the fields `text`, `passed`, and `evidence`.".to_string();
	}

	line.to_string()
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
	paths.sort_by_key(|path| prompt_document_priority(skill_dir, path));
	Ok(paths)
}

fn supported_prompt_document(path: &Path) -> bool {
	let Some(file_name) = path.file_name().and_then(|value| value.to_str()) else {
		return false;
	};
	let normalized_name = file_name.to_ascii_lowercase();
	if matches!(
		normalized_name.as_str(),
		"license" | "license.txt" | "license.md"
	) {
		return false;
	}

	matches!(
		path.extension().and_then(|value| value.to_str()),
		Some("md" | "txt" | "json" | "yaml" | "yml" | "toml")
	)
}

fn prompt_document_priority(skill_dir: &Path, path: &Path) -> (u8, String) {
	let relative = path
		.strip_prefix(skill_dir)
		.map(path_to_forward_slashes)
		.unwrap_or_else(|_| path_to_forward_slashes(path));
	let extension = path
		.extension()
		.and_then(|value| value.to_str())
		.unwrap_or_default();
	let bucket = if relative.starts_with("references/") {
		0
	} else if relative.starts_with("agents/") {
		1
	} else if relative.starts_with("docs/") {
		2
	} else if extension.eq_ignore_ascii_case("md") {
		3
	} else if matches!(extension, "json" | "yaml" | "yml" | "toml") {
		4
	} else {
		5
	};
	(bucket, relative)
}

fn query_mentions_skill(query: &str, skill_name: &str) -> bool {
	let normalized_query = normalize_skill_text(query);
	let normalized_name = normalize_skill_text(skill_name);
	!normalized_name.is_empty() && normalized_query.contains(&normalized_name)
}

fn skill_name_matches(candidate: &str, skill_name: &str) -> bool {
	let normalized_candidate = normalize_skill_text(candidate);
	let normalized_name = normalize_skill_text(skill_name);
	!normalized_name.is_empty() && normalized_candidate == normalized_name
}

fn skill_source_matches(left: &SkillSource, right: &SkillSource) -> bool {
	match (left, right) {
		(
			SkillSource::LocalPath {
				path: left_path, ..
			},
			SkillSource::LocalPath {
				path: right_path, ..
			},
		) => left_path == right_path,
		(
			SkillSource::GitHub {
				owner: left_owner,
				repo: left_repo,
				reference: left_reference,
				subpath: left_subpath,
				..
			},
			SkillSource::GitHub {
				owner: right_owner,
				repo: right_repo,
				reference: right_reference,
				subpath: right_subpath,
				..
			},
		) => {
			left_owner == right_owner
				&& left_repo == right_repo
				&& left_reference == right_reference
				&& left_subpath == right_subpath
		}
		(
			SkillSource::ArchiveZip {
				archive_url: left_archive_url,
				subpath: left_subpath,
				..
			},
			SkillSource::ArchiveZip {
				archive_url: right_archive_url,
				subpath: right_subpath,
				..
			},
		) => left_archive_url == right_archive_url && left_subpath == right_subpath,
		_ => false,
	}
}

fn query_keywords(query: &str) -> Vec<String> {
	normalize_skill_text(query)
		.split_whitespace()
		.filter(|word| word.len() >= 3)
		.filter(|word| !query_stopwords().contains(word))
		.map(std::string::ToString::to_string)
		.fold(Vec::new(), |mut acc, word| {
			if !acc.contains(&word) {
				acc.push(word);
			}
			acc
		})
}

fn should_focus_query_context(query: &str) -> bool {
	let normalized = query.to_ascii_lowercase();
	[
		"according to",
		"what exact",
		"which exact",
		"exact field",
		"field names",
		"schema",
		"baseline",
		"expectation",
		"directory",
		"path",
		"command",
		"根据",
		"精确",
	]
	.iter()
	.any(|marker| normalized.contains(marker) || query.contains(marker))
}

fn query_stopwords() -> &'static [&'static str] {
	&[
		"the",
		"that",
		"what",
		"how",
		"when",
		"with",
		"from",
		"into",
		"use",
		"using",
		"according",
		"skill",
		"skills",
		"creator",
		"must",
		"versus",
	]
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

fn normalize_existing_path(path: PathBuf) -> PathBuf {
	let absolute = if path.is_absolute() {
		path
	} else {
		env::current_dir()
			.map(|cwd| cwd.join(&path))
			.unwrap_or_else(|_| PathBuf::from(".").join(path))
	};
	absolute.canonicalize().unwrap_or(absolute)
}

fn display_path(path: &Path) -> String {
	path.canonicalize()
		.unwrap_or_else(|_| path.to_path_buf())
		.display()
		.to_string()
}

fn path_modified_unix_ms(path: &Path) -> u64 {
	fs::metadata(path)
		.and_then(|metadata| metadata.modified())
		.ok()
		.and_then(|timestamp| timestamp.duration_since(UNIX_EPOCH).ok())
		.and_then(|duration| u64::try_from(duration.as_millis()).ok())
		.unwrap_or_else(now_unix_ms)
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
	let mut record = LogRecord::new("roku-plugin-skills", LogLevel::Info, message);
	for (key, value) in fields {
		record = record.with_field(key, value);
	}
	let _ = emit_global_log(record);
}

#[cfg(test)]
mod tests {
	use std::collections::HashSet;
	use std::fs;
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
	fn returns_existing_install_report_when_source_is_already_installed() {
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

		let first = registry
			.install_from_url(
				"https://github.com/anthropics/skills/tree/main/skills/claude-api",
				"test-suite",
			)
			.expect("first install should succeed");
		let second = registry
			.install_from_url(
				"https://github.com/anthropics/skills/tree/main/skills/claude-api",
				"test-suite",
			)
			.expect("second install should succeed");

		assert_eq!(first.skill_name, second.skill_name);
		assert_eq!(first.install_dir, second.install_dir);
		assert!(second.message.contains("already installed"));
		assert_eq!(
			registry.list_skills().expect("list should succeed").len(),
			1
		);
	}

	#[test]
	fn returns_existing_install_report_for_equivalent_github_source_forms() {
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
			.expect("tree install should succeed");
		let second = registry
			.install_from_url(
				"https://github.com/anthropics/skills/blob/main/skills/claude-api/SKILL.md",
				"test-suite",
			)
			.expect("blob install should resolve as existing");

		assert!(second.message.contains("already installed"));
		assert_eq!(
			registry.list_skills().expect("list should succeed").len(),
			1
		);
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

	#[test]
	fn gets_skill_by_normalized_name() {
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

		let record = registry
			.get_skill("Claude API")
			.expect("normalized lookup should succeed");
		assert_eq!(record.descriptor.name, "claude-api");
	}

	#[test]
	fn renders_prompt_context_for_exact_skill_lookup() {
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
			.render_prompt_context_for_skill("claude api", 16_000)
			.expect("exact skill context should render");
		assert!(context.contains("### skill: claude-api"));
	}

	#[test]
	fn truncates_large_entrypoint_instead_of_failing() {
		let root = tempfile::tempdir().expect("temp root should exist");
		let registry = SkillRegistry::file_backed(root.path().join("skills")).with_fetcher(
			Arc::new(StaticArchiveFetcher {
				archive: DownloadedArchive {
					archive_url: "https://example.com/archive.zip".to_string(),
					bytes: large_skill_archive_bytes(),
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
			.render_prompt_context_for_skill("claude-api", 4_096)
			.expect("large skill context should still render");
		assert!(context.contains("### skill: claude-api"));
		assert!(context.contains("[truncated for prompt budget]"));
	}

	#[test]
	fn query_focused_context_prefers_exact_schema_and_baseline_lines() {
		let root = tempfile::tempdir().expect("temp root should exist");
		let registry = SkillRegistry::file_backed(root.path().join("skills")).with_fetcher(
			Arc::new(StaticArchiveFetcher {
				archive: DownloadedArchive {
					archive_url: "https://example.com/archive.zip".to_string(),
					bytes: skill_creator_focus_archive_bytes(),
					resolved_reference: Some("main".to_string()),
				},
			}),
		);
		registry
			.install_from_url(
				"https://github.com/anthropics/skills/tree/main/skills/skill-creator",
				"test-suite",
			)
			.expect("install should succeed");

		let context = registry
			.render_prompt_context_for_query(
				"Use the skill-creator skill. According to that skill, what exact field names must grading.json expectations use, and how do baseline runs differ when creating a new skill versus improving an existing skill?",
				8_000,
			)
			.expect("context should render")
			.expect("context should exist");

		assert!(context.contains("`text`, `passed`, and `evidence`"));
		assert!(context.contains("`without_skill/outputs/`"));
		assert!(context.contains("`old_skill/outputs/`"));
		assert!(!context.contains("grading_result"));
		assert!(!context.contains("is_current_best"));
	}

	#[test]
	fn summary_queries_keep_high_level_skill_overview() {
		let root = tempfile::tempdir().expect("temp root should exist");
		let registry = SkillRegistry::file_backed(root.path().join("skills")).with_fetcher(
			Arc::new(StaticArchiveFetcher {
				archive: DownloadedArchive {
					archive_url: "https://example.com/archive.zip".to_string(),
					bytes: skill_creator_summary_archive_bytes(),
					resolved_reference: Some("main".to_string()),
				},
			}),
		);
		registry
			.install_from_url(
				"https://github.com/anthropics/skills/tree/main/skills/skill-creator",
				"test-suite",
			)
			.expect("install should succeed");

		let context = registry
			.render_prompt_context_for_query(
				"Use the skill-creator skill. Summarize the core loop it recommends for creating and iterating on a skill.",
				8_000,
			)
			.expect("context should render")
			.expect("context should exist");

		assert!(
			context.contains("At a high level, the process of creating a skill goes like this")
		);
		assert!(context.contains("Write a draft of the skill"));
	}

	fn test_skill_archive_bytes() -> Vec<u8> {
		build_skill_archive_bytes(&[
			(
				"skills-main/skills/claude-api/SKILL.md",
				br#"---
name: claude-api
description: Build apps with the Claude API.
license: Apache-2.0
---

# Claude API Skill

Use this skill when the user explicitly asks for Claude API integration help.
"#
				.as_slice(),
			),
			(
				"skills-main/skills/claude-api/shared/models.md",
				b"Use claude-opus-4-6 unless the user asks otherwise.".as_slice(),
			),
		])
	}

	fn large_skill_archive_bytes() -> Vec<u8> {
		let repeated = "Use this skill to follow a very detailed workflow.\n".repeat(2_000);
		let skill_md = format!(
			"---\nname: claude-api\ndescription: Build apps with the Claude API.\n---\n\n# Claude API Skill\n\n{repeated}"
		);
		build_skill_archive_bytes(&[(
			"skills-main/skills/claude-api/SKILL.md",
			skill_md.as_bytes(),
		)])
	}

	fn skill_creator_focus_archive_bytes() -> Vec<u8> {
		build_skill_archive_bytes(&[
			(
				"skills-main/skills/skill-creator/SKILL.md",
				br#"---
name: skill-creator
description: Build and evaluate new skills.
---

# Skill Creator

Baseline run notes:
- Creating a new skill: no skill at all. Save to `without_skill/outputs/`.
- Improving an existing skill: snapshot the old version first, then save baseline outputs to `old_skill/outputs/`.

When grading each run, the grading.json expectations array must use the fields `text`, `passed`, and `evidence`.
"#,
			),
			(
				"skills-main/skills/skill-creator/references/schemas.md",
				br#"## benchmark.json

- `iterations[].grading_result`: "baseline", "won", "lost", or "tie"
- `iterations[].is_current_best`: Whether this is the current best version

## grading.json

```json
{
  "expectations": [
    {
      "text": "The output includes the name 'John Smith'",
      "passed": true,
      "evidence": "Found in transcript Step 3"
    }
  ]
}
```
"#,
			),
		])
	}

	fn skill_creator_summary_archive_bytes() -> Vec<u8> {
		build_skill_archive_bytes(&[(
			"skills-main/skills/skill-creator/SKILL.md",
			br#"---
name: skill-creator
description: Build and evaluate new skills.
---

# Skill Creator

At a high level, the process of creating a skill goes like this:

- Decide what you want the skill to do
- Write a draft of the skill
- Run a few test prompts
- Help the user evaluate the results
- Rewrite the skill based on feedback
- Repeat until you're satisfied

Baseline run notes:
- Creating a new skill: no skill at all. Save to `without_skill/outputs/`.
- Improving an existing skill: snapshot the old version first, then save baseline outputs to `old_skill/outputs/`.
"#,
		)])
	}

	fn build_skill_archive_bytes(files: &[(&str, &[u8])]) -> Vec<u8> {
		let mut cursor = Cursor::new(Vec::new());
		{
			let mut writer = zip::ZipWriter::new(&mut cursor);
			let options = zip::write::SimpleFileOptions::default();
			let mut directories = HashSet::new();
			for (path, content) in files {
				if let Some(parent) = Path::new(path).parent() {
					for ancestor in parent.ancestors().collect::<Vec<_>>().into_iter().rev() {
						if ancestor.as_os_str().is_empty() {
							continue;
						}
						let directory = format!("{}/", path_to_forward_slashes(ancestor));
						if directories.insert(directory.clone()) {
							writer
								.add_directory(directory, options)
								.expect("support dir should be added");
						}
					}
				}
				writer.start_file(path, options).expect("file should start");
				writer.write_all(content).expect("file should write");
			}
			writer.finish().expect("zip should finish");
		}
		cursor.into_inner()
	}

	#[test]
	fn register_local_skill_uses_absolute_install_dir_and_marks_scripts() {
		let registry_root = tempfile::tempdir().expect("registry root should exist");
		let local_skill_root = tempfile::tempdir().expect("local skill root should exist");
		let skill_dir = local_skill_root.path().join("python-helper");
		fs::create_dir_all(skill_dir.join("scripts")).expect("scripts dir should exist");
		fs::write(
			skill_dir.join("SKILL.md"),
			"---\nname: python-helper\ndescription: Help with Python helper tasks.\n---\n\n# Python Helper\n",
		)
		.expect("skill manifest should write");
		fs::write(skill_dir.join("scripts").join("run.py"), "print('ok')\n")
			.expect("script should write");

		let registry = SkillRegistry::file_backed(registry_root.path().join("skills"));
		let report = registry
			.register_local_skill(&skill_dir, "test-suite")
			.expect("local skill should register");
		let record = registry
			.get_skill("python-helper")
			.expect("local skill record should exist");

		assert!(Path::new(&record.install_dir).is_absolute());
		assert!(record.has_scripts());
		assert_eq!(report.skill_name, "python-helper");
		assert_eq!(
			registry
				.skill_dir("python-helper")
				.expect("skill dir should resolve"),
			skill_dir
				.canonicalize()
				.expect("skill dir should canonicalize")
		);
	}
}
