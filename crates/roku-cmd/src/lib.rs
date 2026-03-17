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

//! Command-line bootstrap for local Roku runtimes and operator utilities.
//!
//! This crate is the process-owned edge of the workspace: it parses CLI arguments, resolves local
//! storage/config layout, and boots the requested runtime surface such as one-shot execution,
//! Telegram transport, or the HTTP gateway. It does not own task-planning semantics itself; those
//! stay inside the runtime service and agent-runtime crates.
//!
//! `roku-cmd` is a composition root, not the owner of memory subsystem contracts or registry
//! semantics. Memory provider selection now flows through the Roku-owned entry registry; this
//! crate keeps only the process-local glue that feeds typed config into that registry.

mod api;
mod bot;
mod memory_registry;
mod memory_runtime_config;
mod runtime;
mod runtime_config;
mod storage;
mod telegram_loop_bridge;

use std::env;
use std::path::PathBuf;
use std::sync::Arc;

use roku_agent_runtime::ToolCatalogConfigError;
use roku_common_types::{ApprovalDecision, PlanningModeHint};
use roku_memory::{
	MemoryKind, MemoryQuery, MemoryRecallReason, MemoryScope, MemoryWriteReason, MemoryWriteRequest,
};
use roku_observability::{
	AsyncRotatingFileLogSink, FanoutLogSink, FileLogConfig, LogSink, StderrLogSink,
	install_global_log_sink,
};
use roku_plugin_host::PluginHostError;
use roku_plugin_skills::SkillRegistryError;
use thiserror::Error;

pub use runtime::{RunMode, run_live_once_from_env, run_once, run_with_mode};

use crate::api::run_api_gateway_from_env;
use crate::bot::{run_telegram_bot_from_env, run_telegram_once_with_options_from_env};
use crate::runtime::{
	ExecutionRequestOptions, decide_approval_from_env, delete_memory_from_env,
	download_artifact_from_env, install_skill_from_env, prepare_memory_artifacts_from_env,
	replay_task_from_env, resume_task_from_env, run_live_once_with_options_from_env,
	run_with_mode_and_options, search_memory_from_env, show_approval_from_env,
	show_artifact_content_from_env, show_artifacts_from_env, show_experiment_from_env,
	show_memory_health_from_env, show_skill_from_env, show_skills_from_env, show_task_from_env,
	write_memory_from_env,
};
use crate::storage::LocalStorageLayout;

/// Top-level command error surface for CLI entrypoints.
///
/// This enum intentionally collapses lower-level bootstrap failures into command-oriented buckets
/// so the binary can report operator-facing startup errors without exposing every internal crate
/// boundary as its own CLI contract.
#[derive(Debug, Error)]
pub enum CommandError {
	#[error("{0}")]
	Usage(String),
	#[error("invalid logging configuration: {0}")]
	LoggingConfiguration(String),
	#[error("failed to bootstrap api gateway: {0}")]
	ApiGatewayBootstrap(String),
	#[error("failed to bootstrap state store: {0}")]
	StateStoreBootstrap(String),
	#[error("failed to load runtime config: {0}")]
	RuntimeConfigBootstrap(String),
	#[error("failed to load tool catalog config: {0}")]
	ToolCatalogBootstrap(String),
	#[error("memory backend failed: {0}")]
	MemoryBackend(String),
	#[error("failed to encode command output: {0}")]
	OutputEncoding(String),
	#[error("io error: {0}")]
	Io(#[from] std::io::Error),
	#[error(transparent)]
	Runtime(#[from] roku_common_types::RuntimeError),
	#[error(transparent)]
	SkillRegistry(#[from] SkillRegistryError),
	#[error(transparent)]
	OpenRouterBootstrap(#[from] roku_plugin_llm::OpenRouterBootstrapError),
	#[error(transparent)]
	ToolCatalogConfig(#[from] ToolCatalogConfigError),
	#[error(transparent)]
	PluginHost(#[from] PluginHostError),
	#[error(transparent)]
	TelegramTransport(#[from] roku_plugin_telegram::TelegramTransportError),
}

/// Parses CLI arguments, dispatches to the requested command surface, and returns printable output.
///
/// `Ok(Some(...))` means the caller should print a response payload. `Ok(None)` is reserved for
/// long-running commands that own their own stdout/stderr lifecycle after startup.
pub fn execute_cli<I, S>(args: I) -> Result<Option<String>, CommandError>
where
	I: IntoIterator<Item = S>,
	S: Into<String>,
{
	let _ = dotenvy::dotenv();
	configure_logging_from_env()?;
	let args = args.into_iter().map(Into::into).collect::<Vec<_>>();
	match args.first().map(String::as_str) {
		None => {
			let response = run_once("bootstrap request")?;
			Ok(Some(response.message))
		}
		Some("once") => {
			let options = parse_request_options(&args[1..])?;
			let response = run_with_mode_and_options(options, RunMode::Normal)?;
			Ok(Some(response.message))
		}
		Some("live-once") => {
			let options = parse_request_options(&args[1..])?;
			let response = run_live_once_with_options_from_env(options)?;
			Ok(Some(response.message))
		}
		Some("telegram-once") | Some("tg-once") => {
			let options = parse_request_options(&args[1..])?;
			Ok(Some(run_telegram_once_with_options_from_env(options)?))
		}
		Some("telegram-bot") | Some("tg-bot") => {
			run_telegram_bot_from_env()?;
			Ok(None)
		}
		Some("api-gateway") | Some("http-api") => {
			run_api_gateway_from_env()?;
			Ok(None)
		}
		Some("task") => execute_task_command(&args[1..]).map(Some),
		Some("approval") => execute_approval_command(&args[1..]).map(Some),
		Some("artifact") => execute_artifact_command(&args[1..]).map(Some),
		Some("experiment") => execute_experiment_command(&args[1..]).map(Some),
		Some("memory") => execute_memory_command(&args[1..]).map(Some),
		Some("skill") => execute_skill_command(&args[1..]).map(Some),
		Some("--help") | Some("-h") | Some("help") => Ok(Some(help_text().to_string())),
		Some(command) => Err(CommandError::Usage(format!(
			"unknown command: {command}\n\n{}",
			help_text()
		))),
	}
}

/// Returns the stable CLI help text used by usage errors and explicit help requests.
///
/// Keeping this in one place prevents subcommand parsers from drifting into slightly different
/// operator guidance.
pub fn help_text() -> &'static str {
	"Usage:\n  roku-cmd once [--session-id <id>] [--planning-mode <mode>] [--generated-skill-root <path>] <goal>\n  roku-cmd live-once [--session-id <id>] [--planning-mode <mode>] [--generated-skill-root <path>] <goal>\n  roku-cmd telegram-once [--session-id <id>] [--planning-mode <mode>] [--generated-skill-root <path>] <goal>\n  roku-cmd telegram-bot\n  roku-cmd api-gateway\n  roku-cmd task show <task-id>\n  roku-cmd task replay <task-id>\n  roku-cmd task resume <task-id>\n  roku-cmd approval show <approval-id>\n  roku-cmd approval approve <approval-id> --actor <actor> [--comment <text>]\n  roku-cmd approval reject <approval-id> --actor <actor> [--comment <text>]\n  roku-cmd artifact list <task-id>\n  roku-cmd artifact content <task-id> <artifact-id>\n  roku-cmd artifact download <task-id> <artifact-id> --output <path>\n  roku-cmd experiment show <task-id>\n  roku-cmd memory prepare-config\n  roku-cmd memory health\n  roku-cmd memory search [--scope <scope>] [--session-id <id>] [--user-id <id>] [--project-id <id>] [--workspace-id <id>] [--limit <n>] <query>\n  roku-cmd memory write [--scope <scope>] [--kind <kind>] [--session-id <id>] [--user-id <id>] [--project-id <id>] [--workspace-id <id>] [--summary <text>] [--write-reason <reason>] <content>\n  roku-cmd memory delete <record-id>\n  roku-cmd skill install <source-url>\n  roku-cmd skill list\n  roku-cmd skill show <skill-name>\n\nCommands:\n  once              Run the deterministic in-process pipeline.\n  live-once         Run the OpenRouter-backed live pipeline from environment.\n  telegram-once     Run one live Telegram handler turn and print the outbound bot message.\n  telegram-bot      Start the Telegram polling bot using environment configuration.\n  api-gateway       Start the Actix HTTP gateway using environment configuration.\n  task show         Render a persisted task snapshot with its event timeline.\n  task replay       Rebuild a state-transition report from persisted task events.\n  task resume       Continue a resumable persisted task using the live runtime path.\n  approval          Show or decide an approval ticket from persisted state.\n  artifact          List artifacts, print artifact content, or download an artifact payload.\n  experiment show   Render the persisted experiment run for a task.\n  memory            Prepare generated config or exercise the provider-neutral memory backend commands.\n  skill install     Install a skill package into the local file-backed registry.\n  skill list        List installed skills from the local registry.\n  skill show        Render installed skill metadata and prompt context.\n\nMemory Scopes:\n  session | user | project | workspace | global\n\nMemory Kinds:\n  user_preference | user_fact | project_fact | workspace_fact | historical_case | constraint | workflow_insight\n\nPlanning Modes:\n  react | taskdecomposition | treesearch | iterativerefinement\n\nNote:\n  --planning-mode is a deprecated compatibility hint. New requests stay on the direct-route runtime and produce a compatibility fallback instead of entering a planning-heavy workflow."
}

fn join_goal(parts: &[String]) -> Result<String, CommandError> {
	if parts.is_empty() {
		return Err(CommandError::Usage(format!(
			"missing goal argument\n\n{}",
			help_text()
		)));
	}
	Ok(parts.join(" "))
}

fn parse_request_options(parts: &[String]) -> Result<ExecutionRequestOptions, CommandError> {
	let mut session_id = "session-1".to_string();
	let mut planning_mode_hint = None;
	let mut generated_skill_root = None;
	let mut goal_parts = Vec::new();
	let mut index = 0usize;

	while index < parts.len() {
		let current = &parts[index];
		if let Some(value) = current.strip_prefix("--session-id=") {
			session_id = parse_non_empty_flag("--session-id", value)?;
			index += 1;
			continue;
		}
		if current == "--session-id" {
			let value = parts
				.get(index + 1)
				.ok_or_else(|| CommandError::Usage("missing value for --session-id".to_string()))?;
			session_id = parse_non_empty_flag("--session-id", value)?;
			index += 2;
			continue;
		}
		if let Some(value) = current.strip_prefix("--planning-mode=") {
			planning_mode_hint = Some(parse_planning_mode_hint(value)?);
			index += 1;
			continue;
		}
		if current == "--planning-mode" {
			let value = parts.get(index + 1).ok_or_else(|| {
				CommandError::Usage("missing value for --planning-mode".to_string())
			})?;
			planning_mode_hint = Some(parse_planning_mode_hint(value)?);
			index += 2;
			continue;
		}
		if let Some(value) = current.strip_prefix("--generated-skill-root=") {
			generated_skill_root = Some(PathBuf::from(parse_non_empty_flag(
				"--generated-skill-root",
				value,
			)?));
			index += 1;
			continue;
		}
		if current == "--generated-skill-root" {
			let value = parts.get(index + 1).ok_or_else(|| {
				CommandError::Usage("missing value for --generated-skill-root".to_string())
			})?;
			generated_skill_root = Some(PathBuf::from(parse_non_empty_flag(
				"--generated-skill-root",
				value,
			)?));
			index += 2;
			continue;
		}
		goal_parts.push(current.clone());
		index += 1;
	}

	Ok(ExecutionRequestOptions {
		session_id,
		goal: join_goal(&goal_parts)?,
		planning_mode_hint,
		generated_skill_root,
	})
}

fn parse_non_empty_flag(flag: &str, value: &str) -> Result<String, CommandError> {
	let trimmed = value.trim();
	if trimmed.is_empty() {
		return Err(CommandError::Usage(format!("{flag} cannot be empty")));
	}

	Ok(trimmed.to_string())
}

fn parse_planning_mode_hint(value: &str) -> Result<PlanningModeHint, CommandError> {
	let normalized = value
		.chars()
		.filter(|character| character.is_ascii_alphanumeric())
		.collect::<String>()
		.to_ascii_lowercase();
	match normalized.as_str() {
		"react" => Ok(PlanningModeHint::ReAct),
		"taskdecomposition" | "decomposition" => Ok(PlanningModeHint::TaskDecomposition),
		"treesearch" | "tree" => Ok(PlanningModeHint::TreeSearch),
		"iterativerefinement" | "refinement" | "refine" => {
			Ok(PlanningModeHint::IterativeRefinement)
		}
		_ => Err(CommandError::Usage(format!(
			"unknown planning mode: {value}\n\n{}",
			help_text()
		))),
	}
}

fn execute_task_command(parts: &[String]) -> Result<String, CommandError> {
	match parts {
		[command, task_id] if command.eq_ignore_ascii_case("show") => show_task_from_env(task_id),
		[command, task_id] if command.eq_ignore_ascii_case("replay") => {
			replay_task_from_env(task_id)
		}
		[command, task_id] if command.eq_ignore_ascii_case("resume") => {
			resume_task_from_env(task_id)
		}
		_ => Err(CommandError::Usage(format!(
			"invalid task command\n\n{}",
			help_text()
		))),
	}
}

fn execute_approval_command(parts: &[String]) -> Result<String, CommandError> {
	match parts.first().map(String::as_str) {
		Some(command) if command.eq_ignore_ascii_case("show") => {
			let approval_id = parts.get(1).ok_or_else(|| {
				CommandError::Usage(format!(
					"missing approval id for approval show\n\n{}",
					help_text()
				))
			})?;
			show_approval_from_env(approval_id)
		}
		Some(command)
			if command.eq_ignore_ascii_case("approve")
				|| command.eq_ignore_ascii_case("reject") =>
		{
			let approval_id = parts.get(1).ok_or_else(|| {
				CommandError::Usage(format!(
					"missing approval id for approval decision\n\n{}",
					help_text()
				))
			})?;
			let options = parse_approval_decision_options(&parts[2..])?;
			decide_approval_from_env(
				approval_id,
				ApprovalDecision {
					actor: options.actor,
					approved: command.eq_ignore_ascii_case("approve"),
					comment: options.comment,
				},
			)
		}
		_ => Err(CommandError::Usage(format!(
			"invalid approval command\n\n{}",
			help_text()
		))),
	}
}

fn execute_artifact_command(parts: &[String]) -> Result<String, CommandError> {
	match parts.first().map(String::as_str) {
		Some(command) if command.eq_ignore_ascii_case("list") => {
			let task_id = parts.get(1).ok_or_else(|| {
				CommandError::Usage(format!(
					"missing task id for artifact list\n\n{}",
					help_text()
				))
			})?;
			show_artifacts_from_env(task_id)
		}
		Some(command) if command.eq_ignore_ascii_case("content") => {
			let task_id = parts.get(1).ok_or_else(|| {
				CommandError::Usage(format!(
					"missing task id for artifact content\n\n{}",
					help_text()
				))
			})?;
			let artifact_id = parts.get(2).ok_or_else(|| {
				CommandError::Usage(format!(
					"missing artifact id for artifact content\n\n{}",
					help_text()
				))
			})?;
			show_artifact_content_from_env(task_id, artifact_id)
		}
		Some(command) if command.eq_ignore_ascii_case("download") => {
			let task_id = parts.get(1).ok_or_else(|| {
				CommandError::Usage(format!(
					"missing task id for artifact download\n\n{}",
					help_text()
				))
			})?;
			let artifact_id = parts.get(2).ok_or_else(|| {
				CommandError::Usage(format!(
					"missing artifact id for artifact download\n\n{}",
					help_text()
				))
			})?;
			let output_path = parse_download_output_path(&parts[3..])?;
			download_artifact_from_env(task_id, artifact_id, &output_path)
		}
		_ => Err(CommandError::Usage(format!(
			"invalid artifact command\n\n{}",
			help_text()
		))),
	}
}

fn execute_experiment_command(parts: &[String]) -> Result<String, CommandError> {
	match parts {
		[command, task_id] if command.eq_ignore_ascii_case("show") => {
			show_experiment_from_env(task_id)
		}
		_ => Err(CommandError::Usage(format!(
			"invalid experiment command\n\n{}",
			help_text()
		))),
	}
}

fn execute_memory_command(parts: &[String]) -> Result<String, CommandError> {
	match parts.first().map(String::as_str) {
		Some(command) if command.eq_ignore_ascii_case("prepare-config") => {
			prepare_memory_artifacts_from_env()
		}
		Some(command) if command.eq_ignore_ascii_case("health") => show_memory_health_from_env(),
		Some(command) if command.eq_ignore_ascii_case("search") => {
			let options = parse_memory_search_options(&parts[1..])?;
			search_memory_from_env(build_memory_query(options)?)
		}
		Some(command) if command.eq_ignore_ascii_case("write") => {
			let options = parse_memory_write_options(&parts[1..])?;
			write_memory_from_env(build_memory_write_request(options)?)
		}
		Some(command) if command.eq_ignore_ascii_case("delete") => {
			let record_id = parts.get(1).ok_or_else(|| {
				CommandError::Usage(format!(
					"missing record id for memory delete\n\n{}",
					help_text()
				))
			})?;
			delete_memory_from_env(record_id)
		}
		_ => Err(CommandError::Usage(format!(
			"invalid memory command\n\n{}",
			help_text()
		))),
	}
}

fn execute_skill_command(parts: &[String]) -> Result<String, CommandError> {
	match parts.first().map(String::as_str) {
		Some(command) if command.eq_ignore_ascii_case("install") => {
			let source_url = parts.get(1).ok_or_else(|| {
				CommandError::Usage(format!(
					"missing source url for skill install\n\n{}",
					help_text()
				))
			})?;
			install_skill_from_env(source_url)
		}
		Some(command) if command.eq_ignore_ascii_case("list") => show_skills_from_env(),
		Some(command) if command.eq_ignore_ascii_case("show") => {
			let skill_name = parts.get(1).ok_or_else(|| {
				CommandError::Usage(format!(
					"missing skill name for skill show\n\n{}",
					help_text()
				))
			})?;
			show_skill_from_env(skill_name)
		}
		_ => Err(CommandError::Usage(format!(
			"invalid skill command\n\n{}",
			help_text()
		))),
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MemorySearchOptions {
	scope: MemoryScope,
	session_id: Option<String>,
	user_id: Option<String>,
	project_id: Option<String>,
	workspace_id: Option<String>,
	limit: usize,
	query: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MemoryWriteOptions {
	scope: MemoryScope,
	kind: MemoryKind,
	session_id: Option<String>,
	user_id: Option<String>,
	project_id: Option<String>,
	workspace_id: Option<String>,
	summary: String,
	write_reason: MemoryWriteReason,
	content: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ApprovalDecisionOptions {
	actor: String,
	comment: Option<String>,
}

fn parse_approval_decision_options(
	parts: &[String],
) -> Result<ApprovalDecisionOptions, CommandError> {
	let mut actor = None;
	let mut comment = None;
	let mut index = 0usize;

	while index < parts.len() {
		let current = &parts[index];
		if let Some(value) = current.strip_prefix("--actor=") {
			actor = Some(parse_non_empty_flag("--actor", value)?);
			index += 1;
			continue;
		}
		if current == "--actor" {
			let value = parts
				.get(index + 1)
				.ok_or_else(|| CommandError::Usage("missing value for --actor".to_string()))?;
			actor = Some(parse_non_empty_flag("--actor", value)?);
			index += 2;
			continue;
		}
		if let Some(value) = current.strip_prefix("--comment=") {
			comment = Some(parse_non_empty_flag("--comment", value)?);
			index += 1;
			continue;
		}
		if current == "--comment" {
			let value = parts
				.get(index + 1)
				.ok_or_else(|| CommandError::Usage("missing value for --comment".to_string()))?;
			comment = Some(parse_non_empty_flag("--comment", value)?);
			index += 2;
			continue;
		}

		return Err(CommandError::Usage(format!(
			"unknown approval decision flag: {current}\n\n{}",
			help_text()
		)));
	}

	Ok(ApprovalDecisionOptions {
		actor: actor.ok_or_else(|| {
			CommandError::Usage(format!(
				"missing required --actor for approval decision\n\n{}",
				help_text()
			))
		})?,
		comment,
	})
}

fn parse_download_output_path(parts: &[String]) -> Result<PathBuf, CommandError> {
	let Some(current) = parts.first() else {
		return Err(CommandError::Usage(format!(
			"missing required --output for artifact download\n\n{}",
			help_text()
		)));
	};

	if let Some(value) = current.strip_prefix("--output=") {
		return Ok(PathBuf::from(parse_non_empty_flag("--output", value)?));
	}

	if current == "--output" {
		let value = parts
			.get(1)
			.ok_or_else(|| CommandError::Usage("missing value for --output".to_string()))?;
		return Ok(PathBuf::from(parse_non_empty_flag("--output", value)?));
	}

	Err(CommandError::Usage(format!(
		"unknown artifact download flag: {current}\n\n{}",
		help_text()
	)))
}

fn parse_memory_search_options(parts: &[String]) -> Result<MemorySearchOptions, CommandError> {
	let mut options = MemorySearchOptions {
		scope: MemoryScope::Session,
		session_id: None,
		user_id: None,
		project_id: None,
		workspace_id: None,
		limit: 5,
		query: String::new(),
	};
	let mut query_parts = Vec::new();
	let mut index = 0usize;

	while index < parts.len() {
		let current = &parts[index];
		if let Some(value) = current.strip_prefix("--scope=") {
			options.scope = parse_memory_scope(value)?;
			index += 1;
			continue;
		}
		if current == "--scope" {
			let value = parts
				.get(index + 1)
				.ok_or_else(|| CommandError::Usage("missing value for --scope".to_string()))?;
			options.scope = parse_memory_scope(value)?;
			index += 2;
			continue;
		}
		if let Some(value) = current.strip_prefix("--session-id=") {
			options.session_id = Some(parse_non_empty_flag("--session-id", value)?);
			index += 1;
			continue;
		}
		if current == "--session-id" {
			let value = parts
				.get(index + 1)
				.ok_or_else(|| CommandError::Usage("missing value for --session-id".to_string()))?;
			options.session_id = Some(parse_non_empty_flag("--session-id", value)?);
			index += 2;
			continue;
		}
		if let Some(value) = current.strip_prefix("--user-id=") {
			options.user_id = Some(parse_non_empty_flag("--user-id", value)?);
			index += 1;
			continue;
		}
		if current == "--user-id" {
			let value = parts
				.get(index + 1)
				.ok_or_else(|| CommandError::Usage("missing value for --user-id".to_string()))?;
			options.user_id = Some(parse_non_empty_flag("--user-id", value)?);
			index += 2;
			continue;
		}
		if let Some(value) = current.strip_prefix("--project-id=") {
			options.project_id = Some(parse_non_empty_flag("--project-id", value)?);
			index += 1;
			continue;
		}
		if current == "--project-id" {
			let value = parts
				.get(index + 1)
				.ok_or_else(|| CommandError::Usage("missing value for --project-id".to_string()))?;
			options.project_id = Some(parse_non_empty_flag("--project-id", value)?);
			index += 2;
			continue;
		}
		if let Some(value) = current.strip_prefix("--workspace-id=") {
			options.workspace_id = Some(parse_non_empty_flag("--workspace-id", value)?);
			index += 1;
			continue;
		}
		if current == "--workspace-id" {
			let value = parts.get(index + 1).ok_or_else(|| {
				CommandError::Usage("missing value for --workspace-id".to_string())
			})?;
			options.workspace_id = Some(parse_non_empty_flag("--workspace-id", value)?);
			index += 2;
			continue;
		}
		if let Some(value) = current.strip_prefix("--limit=") {
			options.limit = parse_usize_flag("--limit", value)?;
			index += 1;
			continue;
		}
		if current == "--limit" {
			let value = parts
				.get(index + 1)
				.ok_or_else(|| CommandError::Usage("missing value for --limit".to_string()))?;
			options.limit = parse_usize_flag("--limit", value)?;
			index += 2;
			continue;
		}
		query_parts.push(current.clone());
		index += 1;
	}

	options.query = join_goal(&query_parts)?;
	validate_memory_scope_identity(
		options.scope,
		options.session_id.as_deref(),
		options.user_id.as_deref(),
		options.project_id.as_deref(),
		options.workspace_id.as_deref(),
	)?;
	Ok(options)
}

fn parse_memory_write_options(parts: &[String]) -> Result<MemoryWriteOptions, CommandError> {
	let mut options = MemoryWriteOptions {
		scope: MemoryScope::Session,
		kind: MemoryKind::HistoricalCase,
		session_id: None,
		user_id: None,
		project_id: None,
		workspace_id: None,
		summary: String::new(),
		write_reason: MemoryWriteReason::OperatorRequested,
		content: String::new(),
	};
	let mut content_parts = Vec::new();
	let mut index = 0usize;

	while index < parts.len() {
		let current = &parts[index];
		if let Some(value) = current.strip_prefix("--scope=") {
			options.scope = parse_memory_scope(value)?;
			index += 1;
			continue;
		}
		if current == "--scope" {
			let value = parts
				.get(index + 1)
				.ok_or_else(|| CommandError::Usage("missing value for --scope".to_string()))?;
			options.scope = parse_memory_scope(value)?;
			index += 2;
			continue;
		}
		if let Some(value) = current.strip_prefix("--kind=") {
			options.kind = parse_memory_kind(value)?;
			index += 1;
			continue;
		}
		if current == "--kind" {
			let value = parts
				.get(index + 1)
				.ok_or_else(|| CommandError::Usage("missing value for --kind".to_string()))?;
			options.kind = parse_memory_kind(value)?;
			index += 2;
			continue;
		}
		if let Some(value) = current.strip_prefix("--session-id=") {
			options.session_id = Some(parse_non_empty_flag("--session-id", value)?);
			index += 1;
			continue;
		}
		if current == "--session-id" {
			let value = parts
				.get(index + 1)
				.ok_or_else(|| CommandError::Usage("missing value for --session-id".to_string()))?;
			options.session_id = Some(parse_non_empty_flag("--session-id", value)?);
			index += 2;
			continue;
		}
		if let Some(value) = current.strip_prefix("--user-id=") {
			options.user_id = Some(parse_non_empty_flag("--user-id", value)?);
			index += 1;
			continue;
		}
		if current == "--user-id" {
			let value = parts
				.get(index + 1)
				.ok_or_else(|| CommandError::Usage("missing value for --user-id".to_string()))?;
			options.user_id = Some(parse_non_empty_flag("--user-id", value)?);
			index += 2;
			continue;
		}
		if let Some(value) = current.strip_prefix("--project-id=") {
			options.project_id = Some(parse_non_empty_flag("--project-id", value)?);
			index += 1;
			continue;
		}
		if current == "--project-id" {
			let value = parts
				.get(index + 1)
				.ok_or_else(|| CommandError::Usage("missing value for --project-id".to_string()))?;
			options.project_id = Some(parse_non_empty_flag("--project-id", value)?);
			index += 2;
			continue;
		}
		if let Some(value) = current.strip_prefix("--workspace-id=") {
			options.workspace_id = Some(parse_non_empty_flag("--workspace-id", value)?);
			index += 1;
			continue;
		}
		if current == "--workspace-id" {
			let value = parts.get(index + 1).ok_or_else(|| {
				CommandError::Usage("missing value for --workspace-id".to_string())
			})?;
			options.workspace_id = Some(parse_non_empty_flag("--workspace-id", value)?);
			index += 2;
			continue;
		}
		if let Some(value) = current.strip_prefix("--summary=") {
			options.summary = parse_non_empty_flag("--summary", value)?;
			index += 1;
			continue;
		}
		if current == "--summary" {
			let value = parts
				.get(index + 1)
				.ok_or_else(|| CommandError::Usage("missing value for --summary".to_string()))?;
			options.summary = parse_non_empty_flag("--summary", value)?;
			index += 2;
			continue;
		}
		if let Some(value) = current.strip_prefix("--write-reason=") {
			options.write_reason = parse_memory_write_reason(value)?;
			index += 1;
			continue;
		}
		if current == "--write-reason" {
			let value = parts.get(index + 1).ok_or_else(|| {
				CommandError::Usage("missing value for --write-reason".to_string())
			})?;
			options.write_reason = parse_memory_write_reason(value)?;
			index += 2;
			continue;
		}
		content_parts.push(current.clone());
		index += 1;
	}

	options.content = join_goal(&content_parts)?;
	if options.summary.is_empty() {
		options.summary = options.content.clone();
	}
	validate_memory_scope_identity(
		options.scope,
		options.session_id.as_deref(),
		options.user_id.as_deref(),
		options.project_id.as_deref(),
		options.workspace_id.as_deref(),
	)?;
	Ok(options)
}

fn build_memory_query(options: MemorySearchOptions) -> Result<MemoryQuery, CommandError> {
	let mut query = MemoryQuery::new(options.query, MemoryRecallReason::Manual, options.scope);
	query.limit = options.limit.max(1);
	query.session_id = options.session_id;
	query.user_id = options.user_id;
	query.project_id = options.project_id;
	query.workspace_id = options.workspace_id;
	Ok(query)
}

fn build_memory_write_request(
	options: MemoryWriteOptions,
) -> Result<MemoryWriteRequest, CommandError> {
	let mut request = MemoryWriteRequest::new(
		options.kind,
		options.scope,
		options.content,
		options.summary,
		options.write_reason,
	);
	request.session_id = options.session_id;
	request.user_id = options.user_id;
	request.project_id = options.project_id;
	request.workspace_id = options.workspace_id;
	Ok(request)
}

fn validate_memory_scope_identity(
	scope: MemoryScope,
	session_id: Option<&str>,
	user_id: Option<&str>,
	project_id: Option<&str>,
	workspace_id: Option<&str>,
) -> Result<(), CommandError> {
	let missing = match scope {
		MemoryScope::Session if session_id.is_none() => Some("--session-id"),
		MemoryScope::User if user_id.is_none() => Some("--user-id"),
		MemoryScope::Project if project_id.is_none() => Some("--project-id"),
		MemoryScope::Workspace if workspace_id.is_none() => Some("--workspace-id"),
		_ => None,
	};
	if let Some(flag) = missing {
		return Err(CommandError::Usage(format!(
			"{flag} is required for memory scope `{}`\n\n{}",
			memory_scope_label(scope),
			help_text()
		)));
	}
	Ok(())
}

fn parse_memory_scope(value: &str) -> Result<MemoryScope, CommandError> {
	match value.trim().to_ascii_lowercase().as_str() {
		"session" => Ok(MemoryScope::Session),
		"user" => Ok(MemoryScope::User),
		"project" => Ok(MemoryScope::Project),
		"workspace" => Ok(MemoryScope::Workspace),
		"global" => Ok(MemoryScope::Global),
		_ => Err(CommandError::Usage(format!(
			"unknown memory scope: {value}\n\n{}",
			help_text()
		))),
	}
}

fn parse_memory_kind(value: &str) -> Result<MemoryKind, CommandError> {
	match value.trim().to_ascii_lowercase().as_str() {
		"user_preference" => Ok(MemoryKind::UserPreference),
		"user_fact" => Ok(MemoryKind::UserFact),
		"project_fact" => Ok(MemoryKind::ProjectFact),
		"workspace_fact" => Ok(MemoryKind::WorkspaceFact),
		"historical_case" => Ok(MemoryKind::HistoricalCase),
		"constraint" => Ok(MemoryKind::Constraint),
		"workflow_insight" => Ok(MemoryKind::WorkflowInsight),
		_ => Err(CommandError::Usage(format!(
			"unknown memory kind: {value}\n\n{}",
			help_text()
		))),
	}
}

fn parse_memory_write_reason(value: &str) -> Result<MemoryWriteReason, CommandError> {
	match value.trim().to_ascii_lowercase().as_str() {
		"task_succeeded" => Ok(MemoryWriteReason::TaskSucceeded),
		"high_value_observation" => Ok(MemoryWriteReason::HighValueObservation),
		"operator_requested" => Ok(MemoryWriteReason::OperatorRequested),
		_ => Err(CommandError::Usage(format!(
			"unknown memory write reason: {value}\n\n{}",
			help_text()
		))),
	}
}

fn parse_usize_flag(flag: &str, value: &str) -> Result<usize, CommandError> {
	value
		.trim()
		.parse::<usize>()
		.map_err(|error| CommandError::Usage(format!("{flag}: {error}")))
}

fn memory_scope_label(scope: MemoryScope) -> &'static str {
	match scope {
		MemoryScope::Session => "session",
		MemoryScope::User => "user",
		MemoryScope::Project => "project",
		MemoryScope::Workspace => "workspace",
		MemoryScope::Global => "global",
	}
}

fn configure_logging_from_env() -> Result<(), CommandError> {
	let layout = LocalStorageLayout::from_env();
	layout.ensure_dirs()?;
	let base_dir = layout.log_dir;
	let max_file_bytes = env_var_u64("ROKU_LOG_MAX_FILE_BYTES")?.unwrap_or(8_u64 * 1024 * 1024);
	let max_backup_files = env_var_usize("ROKU_LOG_MAX_BACKUP_FILES")?.unwrap_or(5);
	let stderr_enabled = env_var_bool("ROKU_LOG_STDERR")?.unwrap_or(true);

	let mut sinks: Vec<Arc<dyn LogSink>> =
		vec![Arc::new(AsyncRotatingFileLogSink::new(FileLogConfig {
			base_dir,
			max_file_bytes,
			max_backup_files,
		}))];
	if stderr_enabled {
		sinks.push(Arc::new(StderrLogSink));
	}
	install_global_log_sink(Arc::new(FanoutLogSink::new(sinks)));
	Ok(())
}

fn env_var_u64(key: &'static str) -> Result<Option<u64>, CommandError> {
	match env::var(key) {
		Ok(value) if !value.trim().is_empty() => value
			.parse::<u64>()
			.map(Some)
			.map_err(|error| CommandError::LoggingConfiguration(format!("{key}: {error}"))),
		Ok(_) | Err(env::VarError::NotPresent) => Ok(None),
		Err(error) => Err(CommandError::LoggingConfiguration(format!(
			"{key}: {error}"
		))),
	}
}

fn env_var_usize(key: &'static str) -> Result<Option<usize>, CommandError> {
	match env::var(key) {
		Ok(value) if !value.trim().is_empty() => value
			.parse::<usize>()
			.map(Some)
			.map_err(|error| CommandError::LoggingConfiguration(format!("{key}: {error}"))),
		Ok(_) | Err(env::VarError::NotPresent) => Ok(None),
		Err(error) => Err(CommandError::LoggingConfiguration(format!(
			"{key}: {error}"
		))),
	}
}

fn env_var_bool(key: &'static str) -> Result<Option<bool>, CommandError> {
	match env::var(key) {
		Ok(value) if !value.trim().is_empty() => match value.to_ascii_lowercase().as_str() {
			"1" | "true" | "yes" | "on" => Ok(Some(true)),
			"0" | "false" | "no" | "off" => Ok(Some(false)),
			_ => Err(CommandError::LoggingConfiguration(format!(
				"{key}: expected one of true/false/1/0/yes/no/on/off"
			))),
		},
		Ok(_) | Err(env::VarError::NotPresent) => Ok(None),
		Err(error) => Err(CommandError::LoggingConfiguration(format!(
			"{key}: {error}"
		))),
	}
}

#[cfg(test)]
mod tests {
	use roku_common_types::ResponseStatus;

	use super::*;

	#[test]
	fn run_once_returns_success() {
		let response = run_once("analyze market").expect("pipeline should succeed");
		assert!(matches!(response.status, ResponseStatus::Failed));
		assert!(
			response
				.message
				.contains("[runtime requested=deterministic effective=deterministic]")
		);
	}

	#[test]
	fn run_with_missing_evidence_keeps_new_requests_on_direct_runtime() {
		let response = run_with_mode(
			"Read the first part of Cargo.toml.",
			RunMode::MissingEvidence,
		)
		.expect("pipeline should execute through the direct runtime");
		assert!(matches!(response.status, ResponseStatus::Failed));
	}

	#[test]
	fn run_with_capability_denied_keeps_new_requests_on_direct_runtime() {
		let response = run_with_mode(
			"Read the first part of Cargo.toml.",
			RunMode::CapabilityDenied,
		)
		.expect("pipeline should execute through the direct runtime");
		assert!(matches!(response.status, ResponseStatus::Failed));
	}

	#[test]
	fn execute_cli_help_renders_usage() {
		let output = execute_cli(["help"]).expect("help should succeed");
		assert!(output.is_some());
		let help = output.expect("help output should exist");
		assert!(help.contains("telegram-once"));
		assert!(help.contains("telegram-bot"));
		assert!(help.contains("api-gateway"));
		assert!(help.contains("task show"));
		assert!(help.contains("task replay"));
		assert!(help.contains("task resume"));
		assert!(help.contains("approval show"));
		assert!(help.contains("artifact list"));
		assert!(help.contains("experiment show"));
		assert!(help.contains("skill install"));
		assert!(help.contains("skill list"));
		assert!(help.contains("skill show"));
	}

	#[test]
	fn execute_skill_command_requires_expected_arguments() {
		let error =
			execute_skill_command(&["show".to_string()]).expect_err("skill show should fail");
		assert!(error.to_string().contains("missing skill name"));

		let error = execute_skill_command(&["install".to_string()])
			.expect_err("skill install should require a source url");
		assert!(error.to_string().contains("missing source url"));
	}

	#[test]
	fn parse_request_options_supports_session_and_planning_mode_flags() {
		let options = parse_request_options(&[
			"--session-id".to_string(),
			"chat-42".to_string(),
			"--planning-mode".to_string(),
			"TreeSearch".to_string(),
			"--generated-skill-root".to_string(),
			"/tmp/generated-skills".to_string(),
			"investigate".to_string(),
			"memory".to_string(),
		])
		.expect("request options should parse");

		assert_eq!(options.session_id, "chat-42");
		assert_eq!(
			options.planning_mode_hint,
			Some(PlanningModeHint::TreeSearch)
		);
		assert_eq!(
			options.generated_skill_root,
			Some(PathBuf::from("/tmp/generated-skills"))
		);
		assert_eq!(options.goal, "investigate memory");
	}

	#[test]
	fn parse_request_options_rejects_unknown_planning_mode() {
		let error = parse_request_options(&[
			"--planning-mode".to_string(),
			"unknown".to_string(),
			"hello".to_string(),
		])
		.expect_err("unknown mode should fail");

		assert!(error.to_string().contains("unknown planning mode"));
	}

	#[test]
	fn parse_approval_decision_options_supports_actor_and_comment() {
		let options = parse_approval_decision_options(&[
			"--actor".to_string(),
			"reviewer".to_string(),
			"--comment=looks good".to_string(),
		])
		.expect("approval options should parse");

		assert_eq!(options.actor, "reviewer");
		assert_eq!(options.comment.as_deref(), Some("looks good"));
	}

	#[test]
	fn parse_approval_decision_options_requires_actor() {
		let error = parse_approval_decision_options(&[
			"--comment".to_string(),
			"missing actor".to_string(),
		])
		.expect_err("actor should be required");

		assert!(error.to_string().contains("missing required --actor"));
	}

	#[test]
	fn parse_download_output_path_supports_output_flag() {
		let path =
			parse_download_output_path(&["--output".to_string(), "/tmp/artifact.txt".to_string()])
				.expect("download output path should parse");

		assert_eq!(path, PathBuf::from("/tmp/artifact.txt"));
	}

	#[test]
	fn parse_download_output_path_requires_output_flag() {
		let error = parse_download_output_path(&[]).expect_err("output flag should be required");
		assert!(error.to_string().contains("missing required --output"));
	}

	#[test]
	fn parse_memory_search_options_requires_explicit_scope_identity() {
		let error = parse_memory_search_options(&["recent preference".to_string()])
			.expect_err("session-scoped search should require an explicit session id");
		assert!(error.to_string().contains("--session-id is required"));

		let options = parse_memory_search_options(&[
			"--scope".to_string(),
			"global".to_string(),
			"recent preference".to_string(),
		])
		.expect("global search should not require extra identity");
		assert_eq!(options.scope, MemoryScope::Global);
		assert_eq!(options.query, "recent preference");
	}

	#[test]
	fn parse_memory_write_options_requires_explicit_scope_identity() {
		let error = parse_memory_write_options(&["remember this".to_string()])
			.expect_err("session-scoped write should require an explicit session id");
		assert!(error.to_string().contains("--session-id is required"));

		let options = parse_memory_write_options(&[
			"--scope".to_string(),
			"global".to_string(),
			"--summary".to_string(),
			"global note".to_string(),
			"remember this".to_string(),
		])
		.expect("global write should not require extra identity");
		assert_eq!(options.scope, MemoryScope::Global);
		assert_eq!(options.summary, "global note");
		assert_eq!(options.content, "remember this");
	}
}
