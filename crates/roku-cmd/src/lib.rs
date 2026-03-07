//! Roku command runtime bootstrap.

mod bot;
mod runtime;

use std::env;
use std::path::PathBuf;
use std::sync::Arc;

use roku_observability::{
	AsyncRotatingFileLogSink, FanoutLogSink, FileLogConfig, LogSink, StderrLogSink,
	install_global_log_sink,
};
use thiserror::Error;

pub use runtime::{RunMode, run_live_once_from_env, run_once, run_with_mode};

use crate::bot::run_telegram_bot_from_env;

#[derive(Debug, Error)]
pub enum CommandError {
	#[error("{0}")]
	Usage(String),
	#[error("invalid logging configuration: {0}")]
	LoggingConfiguration(String),
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
			let goal = join_goal(&args[1..])?;
			let response = run_once(&goal)?;
			Ok(Some(response.message))
		}
		Some("live-once") => {
			let goal = join_goal(&args[1..])?;
			let response = run_live_once_from_env(&goal)?;
			Ok(Some(response.message))
		}
		Some("telegram-bot") | Some("tg-bot") => {
			run_telegram_bot_from_env()?;
			Ok(None)
		}
		Some("--help") | Some("-h") | Some("help") => Ok(Some(help_text().to_string())),
		Some(command) => Err(CommandError::Usage(format!(
			"unknown command: {command}\n\n{}",
			help_text()
		))),
	}
}

pub fn help_text() -> &'static str {
	"Usage:\n  roku-cmd once <goal>\n  roku-cmd live-once <goal>\n  roku-cmd telegram-bot\n\nCommands:\n  once         Run the deterministic in-process pipeline.\n  live-once    Run the OpenRouter-backed live pipeline from environment.\n  telegram-bot Start the Telegram polling bot using environment configuration."
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
		assert!(
			output
				.expect("help output should exist")
				.contains("telegram-bot")
		);
	}
}
