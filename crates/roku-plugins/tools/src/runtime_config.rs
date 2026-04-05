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

use serde::Deserialize;
use thiserror::Error;

/// Effective runtime tunables for builtin tools and tool-backed workers.
///
/// Values in this struct are already merged from typed defaults, TOML patches,
/// and environment overrides. Call [`Self::validate_and_clamp`] before using
/// externally sourced values so hard limits remain the final guardrail.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ToolsRuntimeConfig {
	pub fs: FsToolRuntimeConfig,
	pub command: CommandToolRuntimeConfig,
	pub python: PythonToolRuntimeConfig,
	pub table: TableToolRuntimeConfig,
	pub web: WebToolRuntimeConfig,
	pub workers: ToolWorkerRuntimeConfig,
}

/// Partial runtime overrides for [`ToolsRuntimeConfig`].
///
/// This mirrors the `[runtime.tools.*]` section in `config/runtime.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolsRuntimeConfigPatch {
	#[serde(default)]
	pub fs: Option<FsToolRuntimeConfigPatch>,
	#[serde(default)]
	pub command: Option<CommandToolRuntimeConfigPatch>,
	#[serde(default)]
	pub python: Option<PythonToolRuntimeConfigPatch>,
	#[serde(default)]
	pub table: Option<TableToolRuntimeConfigPatch>,
	#[serde(default)]
	pub web: Option<WebToolRuntimeConfigPatch>,
	#[serde(default)]
	pub workers: Option<ToolWorkerRuntimeConfigPatch>,
}

/// Effective runtime settings for filesystem tools.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsToolRuntimeConfig {
	pub default_max_bytes: usize,
	pub max_dir_entries: usize,
	pub max_glob_matches: usize,
	pub max_descendant_scan_entries: usize,
	pub max_grep_results: usize,
}

/// Partial overrides for [`FsToolRuntimeConfig`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FsToolRuntimeConfigPatch {
	pub default_max_bytes: Option<usize>,
	pub max_dir_entries: Option<usize>,
	pub max_glob_matches: Option<usize>,
	pub max_descendant_scan_entries: Option<usize>,
	pub max_grep_results: Option<usize>,
}

/// Effective runtime settings for the bounded Python executor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PythonToolRuntimeConfig {
	pub default_timeout_ms: u64,
	pub max_output_bytes: usize,
}

/// Effective runtime settings for the constrained command executor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandToolRuntimeConfig {
	pub default_timeout_ms: u64,
	pub max_output_bytes: usize,
}

/// Partial overrides for [`CommandToolRuntimeConfig`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandToolRuntimeConfigPatch {
	pub default_timeout_ms: Option<u64>,
	pub max_output_bytes: Option<usize>,
}

/// Partial overrides for [`PythonToolRuntimeConfig`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PythonToolRuntimeConfigPatch {
	pub default_timeout_ms: Option<u64>,
	pub max_output_bytes: Option<usize>,
}

/// Effective runtime settings for table helpers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableToolRuntimeConfig {
	pub default_preview_rows: usize,
}

/// Partial overrides for [`TableToolRuntimeConfig`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TableToolRuntimeConfigPatch {
	pub default_preview_rows: Option<usize>,
}

/// Effective runtime settings for web search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebToolRuntimeConfig {
	pub endpoint: Option<String>,
	pub default_top_k: usize,
	pub max_fetch_bytes: usize,
	pub fetch_timeout_ms: u64,
}

/// Partial overrides for [`WebToolRuntimeConfig`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebToolRuntimeConfigPatch {
	pub endpoint: Option<String>,
	pub default_top_k: Option<usize>,
	pub max_fetch_bytes: Option<usize>,
	pub fetch_timeout_ms: Option<u64>,
}

/// Effective runtime settings shared by builtin worker-backed tools.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolWorkerRuntimeConfig {
	pub llm_tool_timeout_ms: u64,
	pub max_skill_prompt_context_chars: usize,
	pub max_skill_execution_output_chars: usize,
}

/// Partial overrides for [`ToolWorkerRuntimeConfig`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolWorkerRuntimeConfigPatch {
	pub llm_tool_timeout_ms: Option<u64>,
	pub max_skill_prompt_context_chars: Option<usize>,
	pub max_skill_execution_output_chars: Option<usize>,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ToolsRuntimeConfigError {
	#[error("runtime.tools.fs.default_max_bytes must be greater than zero")]
	InvalidFsDefaultMaxBytes,
	#[error("runtime.tools.fs.max_dir_entries must be greater than zero")]
	InvalidFsMaxDirEntries,
	#[error("runtime.tools.fs.max_glob_matches must be greater than zero")]
	InvalidFsMaxGlobMatches,
	#[error("runtime.tools.fs.max_descendant_scan_entries must be greater than zero")]
	InvalidFsMaxDescendantScanEntries,
	#[error("runtime.tools.fs.max_grep_results must be greater than zero")]
	InvalidFsMaxGrepResults,
	#[error("runtime.tools.command.default_timeout_ms must be greater than zero")]
	InvalidCommandDefaultTimeoutMs,
	#[error("runtime.tools.command.max_output_bytes must be greater than zero")]
	InvalidCommandMaxOutputBytes,
	#[error("runtime.tools.python.default_timeout_ms must be greater than zero")]
	InvalidPythonDefaultTimeoutMs,
	#[error("runtime.tools.python.max_output_bytes must be greater than zero")]
	InvalidPythonMaxOutputBytes,
	#[error("runtime.tools.table.default_preview_rows must be greater than zero")]
	InvalidTableDefaultPreviewRows,
	#[error("runtime.tools.web.default_top_k must be greater than zero")]
	InvalidWebDefaultTopK,
	#[error("runtime.tools.web.endpoint cannot be empty")]
	InvalidWebEndpoint,
	#[error("runtime.tools.web.max_fetch_bytes must be greater than zero")]
	InvalidWebMaxFetchBytes,
	#[error("runtime.tools.web.fetch_timeout_ms must be greater than zero")]
	InvalidWebFetchTimeoutMs,
	#[error("runtime.tools.workers.llm_tool_timeout_ms must be greater than zero")]
	InvalidWorkerTimeoutMs,
	#[error("runtime.tools.workers.max_skill_prompt_context_chars must be greater than zero")]
	InvalidWorkerPromptContextChars,
	#[error("runtime.tools.workers.max_skill_execution_output_chars must be greater than zero")]
	InvalidWorkerExecutionOutputChars,
}

pub const HARD_MAX_READ_BYTES: usize = 256 * 1024;
pub const HARD_MAX_DIR_ENTRIES: usize = 2_000;
pub const HARD_MAX_GLOB_MATCHES: usize = 2_000;
pub const HARD_MAX_DESCENDANT_SCAN_ENTRIES: usize = 50_000;
pub const HARD_MAX_GREP_RESULTS: usize = 2_000;
pub const HARD_MAX_TIMEOUT_MS: u64 = 120_000;
pub const HARD_MAX_OUTPUT_BYTES: usize = 128 * 1024;
pub const HARD_MAX_PREVIEW_ROWS: usize = 50;
pub const HARD_MAX_WEB_TOP_K: usize = 20;
pub const HARD_MAX_FETCH_BYTES: usize = 512 * 1024;
pub const HARD_MAX_FETCH_TIMEOUT_MS: u64 = 30_000;
pub const HARD_MAX_LLM_TOOL_TIMEOUT_MS: u64 = 180_000;
pub const HARD_MAX_SKILL_PROMPT_CONTEXT_CHARS: usize = 64_000;
pub const HARD_MAX_SKILL_EXECUTION_OUTPUT_CHARS: usize = 16_000;

impl Default for FsToolRuntimeConfig {
	fn default() -> Self {
		Self {
			default_max_bytes: 4_096,
			max_dir_entries: 200,
			max_glob_matches: 200,
			max_descendant_scan_entries: 8_000,
			max_grep_results: 200,
		}
	}
}

impl Default for PythonToolRuntimeConfig {
	fn default() -> Self {
		Self {
			default_timeout_ms: 60_000,
			max_output_bytes: 8_192,
		}
	}
}

impl Default for CommandToolRuntimeConfig {
	fn default() -> Self {
		Self {
			default_timeout_ms: 1_500,
			max_output_bytes: 8_192,
		}
	}
}

impl Default for TableToolRuntimeConfig {
	fn default() -> Self {
		Self {
			default_preview_rows: 5,
		}
	}
}

impl Default for WebToolRuntimeConfig {
	fn default() -> Self {
		Self {
			endpoint: None,
			default_top_k: 5,
			max_fetch_bytes: 102_400,
			fetch_timeout_ms: 10_000,
		}
	}
}

impl Default for ToolWorkerRuntimeConfig {
	fn default() -> Self {
		Self {
			llm_tool_timeout_ms: 45_000,
			max_skill_prompt_context_chars: 16_000,
			max_skill_execution_output_chars: 4_000,
		}
	}
}

impl ToolsRuntimeConfig {
	pub fn apply_patch(&mut self, patch: ToolsRuntimeConfigPatch) {
		if let Some(fs) = patch.fs {
			self.fs.apply_patch(fs);
		}
		if let Some(command) = patch.command {
			self.command.apply_patch(command);
		}
		if let Some(python) = patch.python {
			self.python.apply_patch(python);
		}
		if let Some(table) = patch.table {
			self.table.apply_patch(table);
		}
		if let Some(web) = patch.web {
			self.web.apply_patch(web);
		}
		if let Some(workers) = patch.workers {
			self.workers.apply_patch(workers);
		}
	}

	pub fn validate_and_clamp(&mut self) -> Result<(), ToolsRuntimeConfigError> {
		self.fs.validate_and_clamp()?;
		self.command.validate_and_clamp()?;
		self.python.validate_and_clamp()?;
		self.table.validate_and_clamp()?;
		self.web.validate_and_clamp()?;
		self.workers.validate_and_clamp()?;
		Ok(())
	}

	pub fn apply_env_overrides(&mut self) -> Result<(), ToolsRuntimeConfigError> {
		self.fs.apply_env_overrides()?;
		self.command.apply_env_overrides()?;
		self.python.apply_env_overrides()?;
		self.table.apply_env_overrides()?;
		self.web.apply_env_overrides()?;
		self.workers.apply_env_overrides()?;
		Ok(())
	}
}

impl FsToolRuntimeConfig {
	pub fn apply_patch(&mut self, patch: FsToolRuntimeConfigPatch) {
		if let Some(value) = patch.default_max_bytes {
			self.default_max_bytes = value;
		}
		if let Some(value) = patch.max_dir_entries {
			self.max_dir_entries = value;
		}
		if let Some(value) = patch.max_glob_matches {
			self.max_glob_matches = value;
		}
		if let Some(value) = patch.max_descendant_scan_entries {
			self.max_descendant_scan_entries = value;
		}
		if let Some(value) = patch.max_grep_results {
			self.max_grep_results = value;
		}
	}

	pub fn validate_and_clamp(&mut self) -> Result<(), ToolsRuntimeConfigError> {
		if self.default_max_bytes == 0 {
			return Err(ToolsRuntimeConfigError::InvalidFsDefaultMaxBytes);
		}
		if self.max_dir_entries == 0 {
			return Err(ToolsRuntimeConfigError::InvalidFsMaxDirEntries);
		}
		if self.max_glob_matches == 0 {
			return Err(ToolsRuntimeConfigError::InvalidFsMaxGlobMatches);
		}
		if self.max_descendant_scan_entries == 0 {
			return Err(ToolsRuntimeConfigError::InvalidFsMaxDescendantScanEntries);
		}
		if self.max_grep_results == 0 {
			return Err(ToolsRuntimeConfigError::InvalidFsMaxGrepResults);
		}
		self.default_max_bytes = self.default_max_bytes.min(HARD_MAX_READ_BYTES);
		self.max_dir_entries = self.max_dir_entries.min(HARD_MAX_DIR_ENTRIES);
		self.max_glob_matches = self.max_glob_matches.min(HARD_MAX_GLOB_MATCHES);
		self.max_descendant_scan_entries = self
			.max_descendant_scan_entries
			.min(HARD_MAX_DESCENDANT_SCAN_ENTRIES);
		self.max_grep_results = self.max_grep_results.min(HARD_MAX_GREP_RESULTS);
		Ok(())
	}

	pub fn apply_env_overrides(&mut self) -> Result<(), ToolsRuntimeConfigError> {
		if let Some(value) = env_override_usize("ROKU_RUNTIME__TOOLS__FS__DEFAULT_MAX_BYTES") {
			self.default_max_bytes = value?;
		}
		if let Some(value) = env_override_usize("ROKU_RUNTIME__TOOLS__FS__MAX_DIR_ENTRIES") {
			self.max_dir_entries = value?;
		}
		if let Some(value) = env_override_usize("ROKU_RUNTIME__TOOLS__FS__MAX_GLOB_MATCHES") {
			self.max_glob_matches = value?;
		}
		if let Some(value) =
			env_override_usize("ROKU_RUNTIME__TOOLS__FS__MAX_DESCENDANT_SCAN_ENTRIES")
		{
			self.max_descendant_scan_entries = value?;
		}
		if let Some(value) = env_override_usize("ROKU_RUNTIME__TOOLS__FS__MAX_GREP_RESULTS") {
			self.max_grep_results = value?;
		}
		Ok(())
	}
}

impl PythonToolRuntimeConfig {
	pub fn apply_patch(&mut self, patch: PythonToolRuntimeConfigPatch) {
		if let Some(value) = patch.default_timeout_ms {
			self.default_timeout_ms = value;
		}
		if let Some(value) = patch.max_output_bytes {
			self.max_output_bytes = value;
		}
	}

	pub fn validate_and_clamp(&mut self) -> Result<(), ToolsRuntimeConfigError> {
		if self.default_timeout_ms == 0 {
			return Err(ToolsRuntimeConfigError::InvalidPythonDefaultTimeoutMs);
		}
		if self.max_output_bytes == 0 {
			return Err(ToolsRuntimeConfigError::InvalidPythonMaxOutputBytes);
		}
		self.default_timeout_ms = self.default_timeout_ms.min(HARD_MAX_TIMEOUT_MS);
		self.max_output_bytes = self.max_output_bytes.min(HARD_MAX_OUTPUT_BYTES);
		Ok(())
	}

	pub fn apply_env_overrides(&mut self) -> Result<(), ToolsRuntimeConfigError> {
		if let Some(value) = env_override_u64("ROKU_RUNTIME__TOOLS__PYTHON__DEFAULT_TIMEOUT_MS") {
			self.default_timeout_ms = value?;
		}
		if let Some(value) = env_override_usize("ROKU_RUNTIME__TOOLS__PYTHON__MAX_OUTPUT_BYTES") {
			self.max_output_bytes = value?;
		}
		Ok(())
	}
}

impl CommandToolRuntimeConfig {
	pub fn apply_patch(&mut self, patch: CommandToolRuntimeConfigPatch) {
		if let Some(value) = patch.default_timeout_ms {
			self.default_timeout_ms = value;
		}
		if let Some(value) = patch.max_output_bytes {
			self.max_output_bytes = value;
		}
	}

	pub fn validate_and_clamp(&mut self) -> Result<(), ToolsRuntimeConfigError> {
		if self.default_timeout_ms == 0 {
			return Err(ToolsRuntimeConfigError::InvalidCommandDefaultTimeoutMs);
		}
		if self.max_output_bytes == 0 {
			return Err(ToolsRuntimeConfigError::InvalidCommandMaxOutputBytes);
		}
		self.default_timeout_ms = self.default_timeout_ms.min(HARD_MAX_TIMEOUT_MS);
		self.max_output_bytes = self.max_output_bytes.min(HARD_MAX_OUTPUT_BYTES);
		Ok(())
	}

	pub fn apply_env_overrides(&mut self) -> Result<(), ToolsRuntimeConfigError> {
		if let Some(value) = env_override_u64("ROKU_RUNTIME__TOOLS__COMMAND__DEFAULT_TIMEOUT_MS") {
			self.default_timeout_ms = value?;
		}
		if let Some(value) = env_override_usize("ROKU_RUNTIME__TOOLS__COMMAND__MAX_OUTPUT_BYTES") {
			self.max_output_bytes = value?;
		}
		Ok(())
	}
}

impl TableToolRuntimeConfig {
	pub fn apply_patch(&mut self, patch: TableToolRuntimeConfigPatch) {
		if let Some(value) = patch.default_preview_rows {
			self.default_preview_rows = value;
		}
	}

	pub fn validate_and_clamp(&mut self) -> Result<(), ToolsRuntimeConfigError> {
		if self.default_preview_rows == 0 {
			return Err(ToolsRuntimeConfigError::InvalidTableDefaultPreviewRows);
		}
		self.default_preview_rows = self.default_preview_rows.min(HARD_MAX_PREVIEW_ROWS);
		Ok(())
	}

	pub fn apply_env_overrides(&mut self) -> Result<(), ToolsRuntimeConfigError> {
		if let Some(value) = env_override_usize("ROKU_RUNTIME__TOOLS__TABLE__DEFAULT_PREVIEW_ROWS")
		{
			self.default_preview_rows = value?;
		}
		Ok(())
	}
}

impl WebToolRuntimeConfig {
	pub fn apply_patch(&mut self, patch: WebToolRuntimeConfigPatch) {
		if let Some(value) = patch.endpoint {
			self.endpoint = Some(value);
		}
		if let Some(value) = patch.default_top_k {
			self.default_top_k = value;
		}
		if let Some(value) = patch.max_fetch_bytes {
			self.max_fetch_bytes = value;
		}
		if let Some(value) = patch.fetch_timeout_ms {
			self.fetch_timeout_ms = value;
		}
	}

	pub fn validate_and_clamp(&mut self) -> Result<(), ToolsRuntimeConfigError> {
		if self.default_top_k == 0 {
			return Err(ToolsRuntimeConfigError::InvalidWebDefaultTopK);
		}
		if matches!(self.endpoint.as_deref(), Some(value) if value.trim().is_empty()) {
			return Err(ToolsRuntimeConfigError::InvalidWebEndpoint);
		}
		if self.max_fetch_bytes == 0 {
			return Err(ToolsRuntimeConfigError::InvalidWebMaxFetchBytes);
		}
		if self.fetch_timeout_ms == 0 {
			return Err(ToolsRuntimeConfigError::InvalidWebFetchTimeoutMs);
		}
		self.default_top_k = self.default_top_k.min(HARD_MAX_WEB_TOP_K);
		self.max_fetch_bytes = self.max_fetch_bytes.min(HARD_MAX_FETCH_BYTES);
		self.fetch_timeout_ms = self.fetch_timeout_ms.min(HARD_MAX_FETCH_TIMEOUT_MS);
		self.endpoint = self
			.endpoint
			.take()
			.map(|value| value.trim().to_string())
			.filter(|value| !value.is_empty());
		Ok(())
	}

	pub fn apply_env_overrides(&mut self) -> Result<(), ToolsRuntimeConfigError> {
		if let Some(value) = env_override_string("ROKU_WEB_SEARCH_URL") {
			self.endpoint = Some(value);
		}
		if let Some(value) = env_override_usize("ROKU_RUNTIME__TOOLS__WEB__DEFAULT_TOP_K") {
			self.default_top_k = value?;
		}
		if let Some(value) = env_override_string("ROKU_RUNTIME__TOOLS__WEB__ENDPOINT") {
			self.endpoint = Some(value);
		}
		if let Some(value) = env_override_usize("ROKU_RUNTIME__TOOLS__WEB__MAX_FETCH_BYTES") {
			self.max_fetch_bytes = value?;
		}
		if let Some(value) = env_override_u64("ROKU_RUNTIME__TOOLS__WEB__FETCH_TIMEOUT_MS") {
			self.fetch_timeout_ms = value?;
		}
		Ok(())
	}
}

impl ToolWorkerRuntimeConfig {
	pub fn apply_patch(&mut self, patch: ToolWorkerRuntimeConfigPatch) {
		if let Some(value) = patch.llm_tool_timeout_ms {
			self.llm_tool_timeout_ms = value;
		}
		if let Some(value) = patch.max_skill_prompt_context_chars {
			self.max_skill_prompt_context_chars = value;
		}
		if let Some(value) = patch.max_skill_execution_output_chars {
			self.max_skill_execution_output_chars = value;
		}
	}

	pub fn validate_and_clamp(&mut self) -> Result<(), ToolsRuntimeConfigError> {
		if self.llm_tool_timeout_ms == 0 {
			return Err(ToolsRuntimeConfigError::InvalidWorkerTimeoutMs);
		}
		if self.max_skill_prompt_context_chars == 0 {
			return Err(ToolsRuntimeConfigError::InvalidWorkerPromptContextChars);
		}
		if self.max_skill_execution_output_chars == 0 {
			return Err(ToolsRuntimeConfigError::InvalidWorkerExecutionOutputChars);
		}
		self.llm_tool_timeout_ms = self.llm_tool_timeout_ms.min(HARD_MAX_LLM_TOOL_TIMEOUT_MS);
		self.max_skill_prompt_context_chars = self
			.max_skill_prompt_context_chars
			.min(HARD_MAX_SKILL_PROMPT_CONTEXT_CHARS);
		self.max_skill_execution_output_chars = self
			.max_skill_execution_output_chars
			.min(HARD_MAX_SKILL_EXECUTION_OUTPUT_CHARS);
		Ok(())
	}

	pub fn apply_env_overrides(&mut self) -> Result<(), ToolsRuntimeConfigError> {
		if let Some(value) = env_override_u64("ROKU_RUNTIME__TOOLS__WORKERS__LLM_TOOL_TIMEOUT_MS") {
			self.llm_tool_timeout_ms = value?;
		}
		if let Some(value) =
			env_override_usize("ROKU_RUNTIME__TOOLS__WORKERS__MAX_SKILL_PROMPT_CONTEXT_CHARS")
		{
			self.max_skill_prompt_context_chars = value?;
		}
		if let Some(value) =
			env_override_usize("ROKU_RUNTIME__TOOLS__WORKERS__MAX_SKILL_EXECUTION_OUTPUT_CHARS")
		{
			self.max_skill_execution_output_chars = value?;
		}
		Ok(())
	}
}

fn env_override_string(key: &str) -> Option<String> {
	env::var(key)
		.ok()
		.map(|value| value.trim().to_string())
		.filter(|value| !value.is_empty())
}

fn env_override_usize(key: &'static str) -> Option<Result<usize, ToolsRuntimeConfigError>> {
	env_override_string(key).map(|value| value.parse::<usize>().map_err(|_| invalid_env_key(key)))
}

fn env_override_u64(key: &'static str) -> Option<Result<u64, ToolsRuntimeConfigError>> {
	env_override_string(key).map(|value| value.parse::<u64>().map_err(|_| invalid_env_key(key)))
}

fn invalid_env_key(key: &'static str) -> ToolsRuntimeConfigError {
	match key {
		"ROKU_RUNTIME__TOOLS__FS__DEFAULT_MAX_BYTES" => {
			ToolsRuntimeConfigError::InvalidFsDefaultMaxBytes
		}
		"ROKU_RUNTIME__TOOLS__FS__MAX_DIR_ENTRIES" => {
			ToolsRuntimeConfigError::InvalidFsMaxDirEntries
		}
		"ROKU_RUNTIME__TOOLS__FS__MAX_GLOB_MATCHES" => {
			ToolsRuntimeConfigError::InvalidFsMaxGlobMatches
		}
		"ROKU_RUNTIME__TOOLS__FS__MAX_DESCENDANT_SCAN_ENTRIES" => {
			ToolsRuntimeConfigError::InvalidFsMaxDescendantScanEntries
		}
		"ROKU_RUNTIME__TOOLS__FS__MAX_GREP_RESULTS" => {
			ToolsRuntimeConfigError::InvalidFsMaxGrepResults
		}
		"ROKU_RUNTIME__TOOLS__COMMAND__DEFAULT_TIMEOUT_MS" => {
			ToolsRuntimeConfigError::InvalidCommandDefaultTimeoutMs
		}
		"ROKU_RUNTIME__TOOLS__COMMAND__MAX_OUTPUT_BYTES" => {
			ToolsRuntimeConfigError::InvalidCommandMaxOutputBytes
		}
		"ROKU_RUNTIME__TOOLS__PYTHON__DEFAULT_TIMEOUT_MS" => {
			ToolsRuntimeConfigError::InvalidPythonDefaultTimeoutMs
		}
		"ROKU_RUNTIME__TOOLS__PYTHON__MAX_OUTPUT_BYTES" => {
			ToolsRuntimeConfigError::InvalidPythonMaxOutputBytes
		}
		"ROKU_RUNTIME__TOOLS__TABLE__DEFAULT_PREVIEW_ROWS" => {
			ToolsRuntimeConfigError::InvalidTableDefaultPreviewRows
		}
		"ROKU_RUNTIME__TOOLS__WEB__DEFAULT_TOP_K" => ToolsRuntimeConfigError::InvalidWebDefaultTopK,
		"ROKU_RUNTIME__TOOLS__WEB__MAX_FETCH_BYTES" => {
			ToolsRuntimeConfigError::InvalidWebMaxFetchBytes
		}
		"ROKU_RUNTIME__TOOLS__WEB__FETCH_TIMEOUT_MS" => {
			ToolsRuntimeConfigError::InvalidWebFetchTimeoutMs
		}
		"ROKU_RUNTIME__TOOLS__WORKERS__LLM_TOOL_TIMEOUT_MS" => {
			ToolsRuntimeConfigError::InvalidWorkerTimeoutMs
		}
		"ROKU_RUNTIME__TOOLS__WORKERS__MAX_SKILL_PROMPT_CONTEXT_CHARS" => {
			ToolsRuntimeConfigError::InvalidWorkerPromptContextChars
		}
		"ROKU_RUNTIME__TOOLS__WORKERS__MAX_SKILL_EXECUTION_OUTPUT_CHARS" => {
			ToolsRuntimeConfigError::InvalidWorkerExecutionOutputChars
		}
		_ => ToolsRuntimeConfigError::InvalidWebEndpoint,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn clamps_tool_runtime_values_to_hard_limits() {
		let mut config = ToolsRuntimeConfig::default();
		config.apply_patch(ToolsRuntimeConfigPatch {
			fs: Some(FsToolRuntimeConfigPatch {
				default_max_bytes: Some(HARD_MAX_READ_BYTES * 2),
				max_dir_entries: Some(HARD_MAX_DIR_ENTRIES * 2),
				max_glob_matches: Some(HARD_MAX_GLOB_MATCHES * 2),
				max_descendant_scan_entries: Some(HARD_MAX_DESCENDANT_SCAN_ENTRIES * 2),
				max_grep_results: Some(HARD_MAX_GREP_RESULTS * 2),
			}),
			command: Some(CommandToolRuntimeConfigPatch {
				default_timeout_ms: Some(HARD_MAX_TIMEOUT_MS * 2),
				max_output_bytes: Some(HARD_MAX_OUTPUT_BYTES * 2),
			}),
			python: Some(PythonToolRuntimeConfigPatch {
				default_timeout_ms: Some(HARD_MAX_TIMEOUT_MS * 2),
				max_output_bytes: Some(HARD_MAX_OUTPUT_BYTES * 2),
			}),
			table: Some(TableToolRuntimeConfigPatch {
				default_preview_rows: Some(HARD_MAX_PREVIEW_ROWS * 2),
			}),
			web: Some(WebToolRuntimeConfigPatch {
				endpoint: Some(" https://example.test/search ".to_string()),
				default_top_k: Some(HARD_MAX_WEB_TOP_K * 2),
				max_fetch_bytes: Some(HARD_MAX_FETCH_BYTES * 2),
				fetch_timeout_ms: Some(HARD_MAX_FETCH_TIMEOUT_MS * 2),
			}),
			workers: Some(ToolWorkerRuntimeConfigPatch {
				llm_tool_timeout_ms: Some(HARD_MAX_LLM_TOOL_TIMEOUT_MS * 2),
				max_skill_prompt_context_chars: Some(HARD_MAX_SKILL_PROMPT_CONTEXT_CHARS * 2),
				max_skill_execution_output_chars: Some(HARD_MAX_SKILL_EXECUTION_OUTPUT_CHARS * 2),
			}),
		});

		config.validate_and_clamp().expect("config should clamp");

		assert_eq!(config.fs.default_max_bytes, HARD_MAX_READ_BYTES);
		assert_eq!(config.fs.max_dir_entries, HARD_MAX_DIR_ENTRIES);
		assert_eq!(config.fs.max_glob_matches, HARD_MAX_GLOB_MATCHES);
		assert_eq!(
			config.fs.max_descendant_scan_entries,
			HARD_MAX_DESCENDANT_SCAN_ENTRIES
		);
		assert_eq!(config.fs.max_grep_results, HARD_MAX_GREP_RESULTS);
		assert_eq!(config.command.default_timeout_ms, HARD_MAX_TIMEOUT_MS);
		assert_eq!(config.command.max_output_bytes, HARD_MAX_OUTPUT_BYTES);
		assert_eq!(config.python.default_timeout_ms, HARD_MAX_TIMEOUT_MS);
		assert_eq!(config.python.max_output_bytes, HARD_MAX_OUTPUT_BYTES);
		assert_eq!(config.table.default_preview_rows, HARD_MAX_PREVIEW_ROWS);
		assert_eq!(config.web.default_top_k, HARD_MAX_WEB_TOP_K);
		assert_eq!(config.web.max_fetch_bytes, HARD_MAX_FETCH_BYTES);
		assert_eq!(config.web.fetch_timeout_ms, HARD_MAX_FETCH_TIMEOUT_MS);
		assert_eq!(
			config.web.endpoint.as_deref(),
			Some("https://example.test/search")
		);
		assert_eq!(
			config.workers.llm_tool_timeout_ms,
			HARD_MAX_LLM_TOOL_TIMEOUT_MS
		);
		assert_eq!(
			config.workers.max_skill_prompt_context_chars,
			HARD_MAX_SKILL_PROMPT_CONTEXT_CHARS
		);
		assert_eq!(
			config.workers.max_skill_execution_output_chars,
			HARD_MAX_SKILL_EXECUTION_OUTPUT_CHARS
		);
	}

	#[test]
	fn rejects_zero_values() {
		let mut config = ToolsRuntimeConfig::default();
		config.fs.default_max_bytes = 0;
		assert_eq!(
			config.validate_and_clamp(),
			Err(ToolsRuntimeConfigError::InvalidFsDefaultMaxBytes)
		);
	}
}
