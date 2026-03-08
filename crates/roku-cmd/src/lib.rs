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

//! Roku command runtime bootstrap.

mod api;
mod bot;
mod runtime;

use std::env;
use std::path::PathBuf;
use std::sync::Arc;

use roku_common_types::{ApprovalDecision, PlanningModeHint};
use roku_observability::{
	AsyncRotatingFileLogSink, FanoutLogSink, FileLogConfig, LogSink, StderrLogSink,
	install_global_log_sink,
};
use thiserror::Error;

pub use runtime::{RunMode, run_live_once_from_env, run_once, run_with_mode};

use crate::api::run_api_gateway_from_env;
use crate::bot::run_telegram_bot_from_env;
use crate::runtime::{
	ExecutionRequestOptions, decide_approval_from_env, run_live_once_with_options_from_env,
	run_with_mode_and_options, show_approval_from_env, show_task_from_env,
};

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
	#[error("failed to encode command output: {0}")]
	OutputEncoding(String),
	#[error(transparent)]
	Runtime(#[from] roku_common_types::RuntimeError),
	#[error(transparent)]
	OpenRouterBootstrap(#[from] roku_llm_adapter::OpenRouterBootstrapError),
	#[error(transparent)]
	TelegramTransport(#[from] roku_connectors_telegram::TelegramTransportError),
}

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
		Some("--help") | Some("-h") | Some("help") => Ok(Some(help_text().to_string())),
		Some(command) => Err(CommandError::Usage(format!(
			"unknown command: {command}\n\n{}",
			help_text()
		))),
	}
}

pub fn help_text() -> &'static str {
	"Usage:\n  roku-cmd once [--session-id <id>] [--planning-mode <mode>] <goal>\n  roku-cmd live-once [--session-id <id>] [--planning-mode <mode>] <goal>\n  roku-cmd telegram-bot\n  roku-cmd api-gateway\n  roku-cmd task show <task-id>\n  roku-cmd approval show <approval-id>\n  roku-cmd approval approve <approval-id> --actor <actor> [--comment <text>]\n  roku-cmd approval reject <approval-id> --actor <actor> [--comment <text>]\n\nCommands:\n  once         Run the deterministic in-process pipeline.\n  live-once    Run the OpenRouter-backed live pipeline from environment.\n  telegram-bot Start the Telegram polling bot using environment configuration.\n  api-gateway  Start the Actix HTTP gateway using environment configuration.\n  task show    Render a persisted task snapshot with its event timeline.\n  approval     Show or decide an approval ticket from persisted state.\n\nPlanning Modes:\n  react | taskdecomposition | treesearch | iterativerefinement"
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
		goal_parts.push(current.clone());
		index += 1;
	}

	Ok(ExecutionRequestOptions {
		session_id,
		goal: join_goal(&goal_parts)?,
		planning_mode_hint,
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

fn configure_logging_from_env() -> Result<(), CommandError> {
	let base_dir = env::var("ROKU_LOG_DIR")
		.ok()
		.filter(|value| !value.trim().is_empty())
		.map(PathBuf::from)
		.unwrap_or_else(|| PathBuf::from("logs"));
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
		assert!(matches!(response.status, ResponseStatus::Succeeded));
	}

	#[test]
	fn run_with_missing_evidence_fails_validation() {
		let response = run_with_mode("analyze market", RunMode::MissingEvidence)
			.expect("pipeline should execute and fail validation");
		assert!(matches!(response.status, ResponseStatus::Failed));
		assert!(response.message.contains("evidence is required"));
	}

	#[test]
	fn run_with_capability_denied_fails() {
		let response = run_with_mode("analyze market", RunMode::CapabilityDenied)
			.expect("pipeline should execute and fail with capability denial");
		assert!(matches!(response.status, ResponseStatus::Failed));
		assert!(response.message.contains("capability denied"));
	}

	#[test]
	fn execute_cli_help_renders_usage() {
		let output = execute_cli(["help"]).expect("help should succeed");
		assert!(output.is_some());
		let help = output.expect("help output should exist");
		assert!(help.contains("telegram-bot"));
		assert!(help.contains("api-gateway"));
		assert!(help.contains("task show"));
		assert!(help.contains("approval show"));
	}

	#[test]
	fn parse_request_options_supports_session_and_planning_mode_flags() {
		let options = parse_request_options(&[
			"--session-id".to_string(),
			"chat-42".to_string(),
			"--planning-mode".to_string(),
			"TreeSearch".to_string(),
			"investigate".to_string(),
			"memory".to_string(),
		])
		.expect("request options should parse");

		assert_eq!(options.session_id, "chat-42");
		assert_eq!(
			options.planning_mode_hint,
			Some(PlanningModeHint::TreeSearch)
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
}
