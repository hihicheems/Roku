//! Roku command runtime bootstrap.

mod bot;
mod runtime;

use thiserror::Error;

pub use runtime::{RunMode, run_live_once_from_env, run_once, run_with_mode};

use crate::bot::run_telegram_bot_from_env;

#[derive(Debug, Error)]
pub enum CommandError {
	#[error("{0}")]
	Usage(String),
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
