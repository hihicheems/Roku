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
//! semantics. Memory provider selection now flows through the Roku-owned entry registry in
//! `roku-memory`; this crate keeps only the process-local glue that feeds typed config into that
//! registry.

mod api;
mod bot;
mod chat;
mod conversation;
mod entry_registry;
mod memory_runtime_config;
mod pending_loop_substrate;
mod runtime;
mod runtime_config;
mod session_store;
mod storage;
mod telegram_session_ux_config;

#[cfg(test)]
pub(crate) mod test_support {
	use std::sync::{LazyLock, Mutex};

	use roku_agent_runtime::{
		AskUserPayload, AskUserResumeContract, AskUserResumeDirective, IntentFamily, LoopContext,
		LoopState, RouteDecision, RouteRisk, StepObservation, StepRecord, ToolObservation,
	};
	use roku_common_types::ResourceSelector;

	pub(crate) static ENV_MUTEX: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

	pub(crate) fn pending_inventory_resume_success_loop_state() -> (LoopState, String) {
		let context = LoopContext {
			request_id: "req-pending-inventory-loop-success".to_string(),
			session_id: "session-1".to_string(),
			goal: "告诉我你当前暴露的 tools 和 skills，并按我选择的主题继续".to_string(),
			workspace_root: "/workspace".to_string(),
			working_directory: "/workspace".to_string(),
			visible_tools: vec!["inventory.describe".to_string()],
			bound_resources: vec![ResourceSelector::tool("inventory.describe".to_string())],
			route_decision: RouteDecision::new(
				IntentFamily::Chat,
				0.93,
				false,
				RouteRisk::Low,
				vec!["inventory.describe".to_string()],
				Vec::new(),
				Vec::new(),
				"inventory resume success request",
			),
			last_observation: None,
		};
		let mut loop_state = LoopState::new("loop-pending-inventory-loop-success", &context);
		let observation = ToolObservation {
			ok: false,
			tool_name: "inventory.describe".to_string(),
			error_type: Some("multiple_candidates".to_string()),
			terminal: false,
			data: serde_json::json!({
				"matches": ["tools", "skills"],
			}),
			message: "I can continue with either `tools` or `skills`.".to_string(),
		};
		let interpreted =
			roku_agent_runtime::interpret_observation(&loop_state, observation.clone(), None);
		loop_state.record_step(StepRecord::tool_call(
			1,
			roku_agent_runtime::NextStepDecision {
				action: roku_agent_runtime::NextStepAction::CallTool,
				tool_name: Some("inventory.describe".to_string()),
				arguments: Some(serde_json::json!({})),
				tool_calls: None,
				reason: "Inspect the runtime inventory before answering.".to_string(),
				final_message: None,
			},
			loop_state.visible_tools.clone(),
			loop_state.bound_resources.clone(),
			serde_json::json!({
				"ok": false,
				"error_type": "multiple_candidates",
				"terminal": false,
				"message": "I can continue with either `tools` or `skills`.",
				"data": observation.data.clone(),
			}),
			StepObservation::Tool(observation),
			interpreted.clone(),
			Some(12),
			interpreted.remaining_step_budget,
			interpreted.remaining_recovery_budget,
			"/workspace",
		));
		loop_state.record_step(StepRecord::terminal(
			2,
			roku_agent_runtime::StepAction::AskUser,
			roku_agent_runtime::NextStepDecision {
				action: roku_agent_runtime::NextStepAction::AskUser,
				tool_name: None,
				arguments: None,
				tool_calls: None,
				reason: "Runtime paused for user clarification after the latest tool observation."
					.to_string(),
				final_message: Some("你想继续看 `tools` 还是 `skills`？".to_string()),
			},
			loop_state.visible_tools.clone(),
			loop_state.bound_resources.clone(),
			Some(StepObservation::AskUser {
				final_message: "你想继续看 `tools` 还是 `skills`？".to_string(),
			}),
			3,
			2,
			"/workspace",
		));
		loop_state.awaiting_user = Some(AskUserPayload {
			final_message: "你想继续看 `tools` 还是 `skills`？".to_string(),
			resume_contract: AskUserResumeContract::CandidateSelection {
				candidates: vec!["tools".to_string(), "skills".to_string()],
			},
			resume_directive: Some(AskUserResumeDirective::RepeatToolWithSelectedCandidate {
				tool_name: "inventory.describe".to_string(),
				argument_key: "topic".to_string(),
			}),
		});
		(loop_state, "tools".to_string())
	}
}

use std::env;
use std::path::PathBuf;
use std::sync::Arc;

use clap::{Args, Parser, Subcommand, ValueEnum};
use roku_agent_runtime::ToolCatalogConfigError;
use roku_common_types::ApprovalDecision;
use roku_common_types::{
	AsyncRotatingFileLogSink, FanoutLogSink, FileLogConfig, LogSink, StderrLogSink,
	install_global_log_sink,
};
use roku_memory::{
	MemoryKind, MemoryQuery, MemoryRecallReason, MemoryScope, MemoryWriteReason, MemoryWriteRequest,
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
	replay_task_from_env, resume_task_from_env, run_live_once_with_options_from_env_and_sender,
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
	#[error("failed to bootstrap control-plane bundle: {0}")]
	ControlPlaneBootstrap(String),
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

// ---------------------------------------------------------------------------
// Clap derive structures
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(name = "roku-cmd")]
struct Cli {
	#[command(subcommand)]
	command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
	/// Start an interactive REPL session. /help for in-session commands.
	#[command(after_help = concat!(
		"EXAMPLES:\n",
		"    # Interactive REPL (default, resumes last session)\n",
		"    roku-cmd chat\n\n",
		"    # Interactive REPL with named session\n",
		"    roku-cmd chat --session-id my-project\n\n",
		"    # Pipe mode: send one message, get JSON response\n",
		"    echo \"what tools do you have?\" | roku-cmd chat --pipe\n\n",
		"    # Pipe mode: continue an existing session\n",
		"    echo \"continue\" | roku-cmd chat --pipe --session-id my-project\n\n",
		"    # Pipe mode: stream events to file while reading response\n",
		"    echo \"search for X\" | roku-cmd chat --pipe 2>events.jsonl",
	))]
	Chat {
		#[arg(long, default_value = "chat-default")]
		session_id: String,

		/// Enable pipe mode: read from stdin, output JSON to stdout, events to stderr.
		#[arg(long, default_value_t = false)]
		pipe: bool,
	},

	/// Run the deterministic in-process pipeline.
	#[command(
		after_help = "Example:\n  roku-cmd once --session-id session-1 analyze market trends"
	)]
	Once(RequestArgs),

	/// Run the OpenRouter-backed live pipeline from environment.
	#[command(
		name = "live-once",
		after_help = "Example:\n  roku-cmd live-once --session-id session-1 summarize the latest logs"
	)]
	LiveOnce(RequestArgs),

	/// Run one live Telegram handler turn and print the outbound bot message.
	#[command(name = "telegram-once", aliases = ["tg-once"])]
	TelegramOnce(RequestArgs),

	/// Start the Telegram polling bot using environment configuration.
	#[command(name = "telegram-bot", aliases = ["tg-bot"])]
	TelegramBot,

	/// Start the Actix HTTP gateway using environment configuration.
	#[command(name = "api-gateway", aliases = ["http-api"])]
	ApiGateway,

	/// Task management commands.
	#[command(subcommand)]
	Task(TaskCommand),

	/// Approval management commands.
	#[command(subcommand)]
	Approval(ApprovalCommand),

	/// Artifact management commands.
	#[command(subcommand)]
	Artifact(ArtifactCommand),

	/// Experiment query commands.
	#[command(subcommand)]
	Experiment(ExperimentCommand),

	/// Memory backend commands.
	///
	/// Prepare generated config or exercise the provider-neutral memory backend.
	#[command(
		subcommand,
		after_help = "Memory Scopes:\n  session | user | project | workspace | global\n\nMemory Kinds:\n  user_preference | user_fact | project_fact | workspace_fact | historical_case | constraint | workflow_insight"
	)]
	Memory(MemoryCommand),

	/// Skill management commands.
	#[command(subcommand)]
	Skill(SkillCommand),

	/// Chat session management commands.
	#[command(subcommand)]
	Session(SessionCommand),
}

#[derive(Subcommand)]
enum SessionCommand {
	/// List all chat sessions with turn count and last activity time.
	List,
	/// Delete a chat session's conversation history.
	Delete {
		/// The session ID to delete.
		session_id: String,
	},
}

/// Shared request arguments used by once, live-once, and telegram-once.
#[derive(Args)]
struct RequestArgs {
	#[arg(long, default_value = "session-1")]
	session_id: String,

	#[arg(long)]
	generated_skill_root: Option<PathBuf>,

	/// The goal or prompt to execute (all remaining arguments are joined).
	#[arg(trailing_var_arg = true, required = true, num_args = 1..)]
	goal: Vec<String>,
}

impl RequestArgs {
	fn into_execution_options(self) -> ExecutionRequestOptions {
		ExecutionRequestOptions {
			session_id: self.session_id,
			goal: self.goal.join(" "),
			generated_skill_root: self.generated_skill_root,
		}
	}
}

#[derive(Subcommand)]
enum TaskCommand {
	/// Render a persisted task snapshot with its event timeline.
	Show { task_id: String },
	/// Rebuild a state-transition report from persisted task events.
	Replay { task_id: String },
	/// Continue a resumable persisted task using the live runtime path.
	Resume { task_id: String },
}

#[derive(Subcommand)]
enum ApprovalCommand {
	/// Show an approval ticket from persisted state.
	Show { approval_id: String },
	/// Approve a pending approval ticket.
	Approve {
		approval_id: String,
		#[arg(long, required = true)]
		actor: String,
		#[arg(long)]
		comment: Option<String>,
	},
	/// Reject a pending approval ticket.
	Reject {
		approval_id: String,
		#[arg(long, required = true)]
		actor: String,
		#[arg(long)]
		comment: Option<String>,
	},
}

#[derive(Subcommand)]
enum ArtifactCommand {
	/// List artifacts for a task.
	List { task_id: String },
	/// Print artifact content for a task artifact.
	Content {
		task_id: String,
		artifact_id: String,
	},
	/// Download an artifact payload to a file.
	Download {
		task_id: String,
		artifact_id: String,
		#[arg(long, required = true)]
		output: PathBuf,
	},
}

#[derive(Subcommand)]
enum ExperimentCommand {
	/// Render the persisted experiment run for a task.
	Show { task_id: String },
}

#[derive(Subcommand)]
enum MemoryCommand {
	/// Prepare generated memory config artifacts.
	#[command(name = "prepare-config")]
	PrepareConfig,

	/// Check the memory backend health.
	Health,

	/// Search the memory backend.
	#[command(
		after_help = "Example:\n  roku-cmd memory search --scope global --limit 10 recent user preferences"
	)]
	Search {
		#[arg(long, default_value = "session", value_enum)]
		scope: MemoryScopeArg,
		#[arg(long)]
		session_id: Option<String>,
		#[arg(long)]
		user_id: Option<String>,
		#[arg(long)]
		project_id: Option<String>,
		#[arg(long)]
		workspace_id: Option<String>,
		#[arg(long, default_value = "5")]
		limit: usize,
		/// The search query (all remaining arguments are joined).
		#[arg(trailing_var_arg = true, required = true, num_args = 1..)]
		query: Vec<String>,
	},

	/// Write a memory record.
	#[command(
		after_help = "Example:\n  roku-cmd memory write --scope global --kind workflow_insight this project uses Rust"
	)]
	Write {
		#[arg(long, default_value = "session", value_enum)]
		scope: MemoryScopeArg,
		#[arg(long, default_value = "historical_case", value_enum)]
		kind: MemoryKindArg,
		#[arg(long)]
		session_id: Option<String>,
		#[arg(long)]
		user_id: Option<String>,
		#[arg(long)]
		project_id: Option<String>,
		#[arg(long)]
		workspace_id: Option<String>,
		#[arg(long)]
		summary: Option<String>,
		#[arg(long, default_value = "operator_requested", value_enum)]
		write_reason: MemoryWriteReasonArg,
		/// The memory content (all remaining arguments are joined).
		#[arg(trailing_var_arg = true, required = true, num_args = 1..)]
		content: Vec<String>,
	},

	/// Delete a memory record by its record ID.
	Delete { record_id: String },
}

#[derive(Subcommand)]
enum SkillCommand {
	/// Install a skill package into the local file-backed registry.
	Install { source_url: String },
	/// List installed skills from the local registry.
	List,
	/// Render installed skill metadata and prompt context.
	Show { skill_name: String },
}

// ---------------------------------------------------------------------------
// ValueEnum types for memory enums
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "snake_case")]
enum MemoryScopeArg {
	Session,
	User,
	Project,
	Workspace,
	Global,
}

impl From<MemoryScopeArg> for MemoryScope {
	fn from(arg: MemoryScopeArg) -> Self {
		match arg {
			MemoryScopeArg::Session => MemoryScope::Session,
			MemoryScopeArg::User => MemoryScope::User,
			MemoryScopeArg::Project => MemoryScope::Project,
			MemoryScopeArg::Workspace => MemoryScope::Workspace,
			MemoryScopeArg::Global => MemoryScope::Global,
		}
	}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "snake_case")]
enum MemoryKindArg {
	UserPreference,
	UserFact,
	ProjectFact,
	WorkspaceFact,
	HistoricalCase,
	Constraint,
	WorkflowInsight,
}

impl From<MemoryKindArg> for MemoryKind {
	fn from(arg: MemoryKindArg) -> Self {
		match arg {
			MemoryKindArg::UserPreference => MemoryKind::UserPreference,
			MemoryKindArg::UserFact => MemoryKind::UserFact,
			MemoryKindArg::ProjectFact => MemoryKind::ProjectFact,
			MemoryKindArg::WorkspaceFact => MemoryKind::WorkspaceFact,
			MemoryKindArg::HistoricalCase => MemoryKind::HistoricalCase,
			MemoryKindArg::Constraint => MemoryKind::Constraint,
			MemoryKindArg::WorkflowInsight => MemoryKind::WorkflowInsight,
		}
	}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "snake_case")]
enum MemoryWriteReasonArg {
	TaskSucceeded,
	HighValueObservation,
	OperatorRequested,
}

impl From<MemoryWriteReasonArg> for MemoryWriteReason {
	fn from(arg: MemoryWriteReasonArg) -> Self {
		match arg {
			MemoryWriteReasonArg::TaskSucceeded => MemoryWriteReason::TaskSucceeded,
			MemoryWriteReasonArg::HighValueObservation => MemoryWriteReason::HighValueObservation,
			MemoryWriteReasonArg::OperatorRequested => MemoryWriteReason::OperatorRequested,
		}
	}
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

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

	let cli = match Cli::try_parse_from(
		std::iter::once("roku-cmd".to_string()).chain(args.into_iter().map(Into::into)),
	) {
		Ok(cli) => cli,
		Err(e)
			if e.kind() == clap::error::ErrorKind::DisplayHelp
				|| e.kind() == clap::error::ErrorKind::DisplayVersion =>
		{
			return Ok(Some(e.to_string()));
		}
		Err(e) => return Err(CommandError::Usage(e.to_string())),
	};

	let rt = tokio::runtime::Builder::new_multi_thread()
		.enable_all()
		.build()
		.map_err(|error| {
			CommandError::Runtime(roku_common_types::RuntimeError::new(error.to_string()))
		})?;

	match cli.command {
		None => {
			let response = rt
				.block_on(run_once("bootstrap request"))
				.map_err(CommandError::Runtime)?;
			Ok(Some(response.message))
		}
		Some(Commands::Chat { session_id, pipe }) => {
			chat::run_chat(&rt, chat::ChatOptions { session_id, pipe })?;
			Ok(None)
		}
		Some(Commands::Once(args)) => {
			let options = args.into_execution_options();
			let response = rt
				.block_on(run_with_mode_and_options(options, RunMode::Normal))
				.map_err(CommandError::Runtime)?;
			Ok(Some(response.message))
		}
		Some(Commands::LiveOnce(args)) => {
			let options = args.into_execution_options();
			rt.block_on(async {
				let (tx, mut rx) =
					tokio::sync::mpsc::unbounded_channel::<roku_agent_runtime::LoopEvent>();
				let render_task = tokio::spawn(async move {
					while let Some(event) = rx.recv().await {
						match event {
							roku_agent_runtime::LoopEvent::ToolStart { step, tool_name } => {
								eprintln!("[tool] step {step} starting: {tool_name}");
							}
							roku_agent_runtime::LoopEvent::ToolEnd {
								step,
								tool_name,
								elapsed_ms,
							} => {
								if let Some(ms) = elapsed_ms {
									eprintln!("[tool] step {step} done: {tool_name} ({ms}ms)");
								} else {
									eprintln!("[tool] step {step} done: {tool_name}");
								}
							}
							roku_agent_runtime::LoopEvent::CompactTriggered {
								step,
								estimated_tokens,
							} => {
								eprintln!(
									"[compact] step {step} triggered (~{estimated_tokens} tokens)"
								);
							}
							roku_agent_runtime::LoopEvent::LlmTextDelta { text, .. } => {
								eprint!("{text}");
							}
							roku_agent_runtime::LoopEvent::LlmDecisionComplete { .. } => {
								eprintln!();
							}
							roku_agent_runtime::LoopEvent::StepComplete { step } => {
								eprintln!("[step] {step} complete");
							}
						}
					}
				});
				let response =
					run_live_once_with_options_from_env_and_sender(options, Some(&tx)).await?;
				drop(tx);
				render_task.await.ok();
				Ok::<_, CommandError>(Some(response.message))
			})
		}
		Some(Commands::TelegramOnce(args)) => {
			let options = args.into_execution_options();
			Ok(Some(run_telegram_once_with_options_from_env(options)?))
		}
		Some(Commands::TelegramBot) => {
			run_telegram_bot_from_env()?;
			Ok(None)
		}
		Some(Commands::ApiGateway) => {
			run_api_gateway_from_env()?;
			Ok(None)
		}
		Some(Commands::Task(cmd)) => match cmd {
			TaskCommand::Show { task_id } => show_task_from_env(&task_id).map(Some),
			TaskCommand::Replay { task_id } => replay_task_from_env(&task_id).map(Some),
			TaskCommand::Resume { task_id } => resume_task_from_env(&task_id).map(Some),
		},
		Some(Commands::Approval(cmd)) => match cmd {
			ApprovalCommand::Show { approval_id } => show_approval_from_env(&approval_id).map(Some),
			ApprovalCommand::Approve {
				approval_id,
				actor,
				comment,
			} => decide_approval_from_env(
				&approval_id,
				ApprovalDecision {
					actor,
					approved: true,
					comment,
				},
			)
			.map(Some),
			ApprovalCommand::Reject {
				approval_id,
				actor,
				comment,
			} => decide_approval_from_env(
				&approval_id,
				ApprovalDecision {
					actor,
					approved: false,
					comment,
				},
			)
			.map(Some),
		},
		Some(Commands::Artifact(cmd)) => match cmd {
			ArtifactCommand::List { task_id } => show_artifacts_from_env(&task_id).map(Some),
			ArtifactCommand::Content {
				task_id,
				artifact_id,
			} => show_artifact_content_from_env(&task_id, &artifact_id).map(Some),
			ArtifactCommand::Download {
				task_id,
				artifact_id,
				output,
			} => download_artifact_from_env(&task_id, &artifact_id, &output).map(Some),
		},
		Some(Commands::Experiment(cmd)) => match cmd {
			ExperimentCommand::Show { task_id } => show_experiment_from_env(&task_id).map(Some),
		},
		Some(Commands::Memory(cmd)) => match cmd {
			MemoryCommand::PrepareConfig => prepare_memory_artifacts_from_env().map(Some),
			MemoryCommand::Health => show_memory_health_from_env().map(Some),
			MemoryCommand::Search {
				scope,
				session_id,
				user_id,
				project_id,
				workspace_id,
				limit,
				query,
			} => {
				let scope: MemoryScope = scope.into();
				validate_memory_scope_identity(
					scope,
					session_id.as_deref(),
					user_id.as_deref(),
					project_id.as_deref(),
					workspace_id.as_deref(),
				)?;
				let query = build_memory_query(
					scope,
					session_id,
					user_id,
					project_id,
					workspace_id,
					limit,
					query.join(" "),
				)?;
				search_memory_from_env(query).map(Some)
			}
			MemoryCommand::Write {
				scope,
				kind,
				session_id,
				user_id,
				project_id,
				workspace_id,
				summary,
				write_reason,
				content,
			} => {
				let scope: MemoryScope = scope.into();
				validate_memory_scope_identity(
					scope,
					session_id.as_deref(),
					user_id.as_deref(),
					project_id.as_deref(),
					workspace_id.as_deref(),
				)?;
				let content_str = content.join(" ");
				let summary_str = summary.unwrap_or_else(|| content_str.clone());
				let request = build_memory_write_request(
					scope,
					kind.into(),
					session_id,
					user_id,
					project_id,
					workspace_id,
					summary_str,
					write_reason.into(),
					content_str,
				)?;
				write_memory_from_env(request).map(Some)
			}
			MemoryCommand::Delete { record_id } => delete_memory_from_env(&record_id).map(Some),
		},
		Some(Commands::Skill(cmd)) => match cmd {
			SkillCommand::Install { source_url } => install_skill_from_env(&source_url).map(Some),
			SkillCommand::List => show_skills_from_env().map(Some),
			SkillCommand::Show { skill_name } => show_skill_from_env(&skill_name).map(Some),
		},
		Some(Commands::Session(cmd)) => {
			let layout = storage::LocalStorageLayout::from_env();
			let store = session_store::SessionStore::new(layout.session_history_dir);
			match cmd {
				SessionCommand::List => {
					let sessions = store
						.list()
						.map_err(|e| CommandError::Io(std::io::Error::other(e)))?;
					if sessions.is_empty() {
						Ok(Some("No chat sessions found.".to_string()))
					} else {
						let mut out = String::from("Chat sessions:\n");
						for s in &sessions {
							let ts = format_unix_ms(s.last_modified);
							out.push_str(&format!(
								"  {:<24} {:>4} turns   last active: {}\n",
								s.session_id, s.turn_count, ts
							));
						}
						Ok(Some(out))
					}
				}
				SessionCommand::Delete { session_id } => {
					let deleted = store
						.delete(&session_id)
						.map_err(|e| CommandError::Io(std::io::Error::other(e)))?;
					if deleted {
						Ok(Some(format!("Deleted session '{session_id}'.")))
					} else {
						Ok(Some(format!("Session '{session_id}' not found.")))
					}
				}
			}
		}
	}
}

fn format_unix_ms(ms: u64) -> String {
	if ms == 0 {
		return "unknown".to_string();
	}
	let secs = (ms / 1000) as i64;
	let dt =
		time::OffsetDateTime::from_unix_timestamp(secs).unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
	let format = time::format_description::parse("[year]-[month]-[day] [hour]:[minute]:[second]")
		.unwrap_or_default();
	dt.format(&format).unwrap_or_else(|_| "unknown".to_string())
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn build_memory_query(
	scope: MemoryScope,
	session_id: Option<String>,
	user_id: Option<String>,
	project_id: Option<String>,
	workspace_id: Option<String>,
	limit: usize,
	query: String,
) -> Result<MemoryQuery, CommandError> {
	let mut q = MemoryQuery::new(query, MemoryRecallReason::Manual, scope);
	q.limit = limit.max(1);
	q.session_id = session_id;
	q.user_id = user_id;
	q.project_id = project_id;
	q.workspace_id = workspace_id;
	Ok(q)
}

#[allow(clippy::too_many_arguments)]
fn build_memory_write_request(
	scope: MemoryScope,
	kind: MemoryKind,
	session_id: Option<String>,
	user_id: Option<String>,
	project_id: Option<String>,
	workspace_id: Option<String>,
	summary: String,
	write_reason: MemoryWriteReason,
	content: String,
) -> Result<MemoryWriteRequest, CommandError> {
	let mut request = MemoryWriteRequest::new(kind, scope, content, summary, write_reason);
	request.session_id = session_id;
	request.user_id = user_id;
	request.project_id = project_id;
	request.workspace_id = workspace_id;
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
		let scope_label = match scope {
			MemoryScope::Session => "session",
			MemoryScope::User => "user",
			MemoryScope::Project => "project",
			MemoryScope::Workspace => "workspace",
			MemoryScope::Global => "global",
		};
		return Err(CommandError::Usage(format!(
			"{flag} is required for memory scope `{scope_label}`"
		)));
	}
	Ok(())
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
	use std::ffi::OsString;
	use std::path::Path;

	use roku_common_types::ResponseStatus;

	use super::*;
	use crate::test_support::ENV_MUTEX;

	struct TestEnvGuard {
		key: &'static str,
		original: Option<OsString>,
	}

	impl TestEnvGuard {
		fn set_path(key: &'static str, value: &Path) -> Self {
			let original = std::env::var_os(key);
			unsafe {
				std::env::set_var(key, value);
			}
			Self { key, original }
		}
	}

	impl Drop for TestEnvGuard {
		fn drop(&mut self) {
			if let Some(value) = &self.original {
				unsafe {
					std::env::set_var(self.key, value);
				}
			} else {
				unsafe {
					std::env::remove_var(self.key);
				}
			}
		}
	}

	fn set_temp_runtime_env(tempdir: &Path) -> (TestEnvGuard, TestEnvGuard) {
		let runtime_config_path = tempdir.join("config").join("runtime.toml");
		(
			TestEnvGuard::set_path("ROKU_HOME", tempdir),
			TestEnvGuard::set_path("ROKU_RUNTIME_CONFIG_PATH", &runtime_config_path),
		)
	}

	#[tokio::test(flavor = "multi_thread")]
	#[allow(clippy::await_holding_lock)]
	async fn run_once_returns_success() {
		let _env_lock = ENV_MUTEX.lock().expect("env mutex should lock");
		let tempdir = tempfile::tempdir().expect("temp root should exist");
		let (_home_guard, _config_guard) = set_temp_runtime_env(tempdir.path());

		let response = run_once("analyze market")
			.await
			.expect("pipeline should succeed");
		assert!(matches!(response.status, ResponseStatus::Failed));
		assert!(
			response
				.message
				.contains("[runtime requested=deterministic effective=deterministic]")
		);
	}

	#[tokio::test(flavor = "multi_thread")]
	#[allow(clippy::await_holding_lock)]
	async fn run_with_missing_evidence_keeps_new_requests_on_direct_runtime() {
		let _env_lock = ENV_MUTEX.lock().expect("env mutex should lock");
		let tempdir = tempfile::tempdir().expect("temp root should exist");
		let (_home_guard, _config_guard) = set_temp_runtime_env(tempdir.path());

		let response = run_with_mode(
			"Read the first part of Cargo.toml.",
			RunMode::MissingEvidence,
		)
		.await
		.expect("pipeline should execute through the direct runtime");
		assert!(matches!(response.status, ResponseStatus::Failed));
	}

	#[tokio::test(flavor = "multi_thread")]
	#[allow(clippy::await_holding_lock)]
	async fn run_with_capability_denied_keeps_new_requests_on_direct_runtime() {
		let _env_lock = ENV_MUTEX.lock().expect("env mutex should lock");
		let tempdir = tempfile::tempdir().expect("temp root should exist");
		let (_home_guard, _config_guard) = set_temp_runtime_env(tempdir.path());

		let response = run_with_mode(
			"Read the first part of Cargo.toml.",
			RunMode::CapabilityDenied,
		)
		.await
		.expect("pipeline should execute through the direct runtime");
		assert!(matches!(response.status, ResponseStatus::Failed));
	}

	#[test]
	fn execute_cli_help_renders_usage() {
		let output = execute_cli(["--help"]).expect("help should succeed");
		assert!(output.is_some());
		let help = output.expect("help output should exist");
		assert!(help.contains("telegram-once") || help.contains("telegram"));
		assert!(help.contains("telegram-bot") || help.contains("tg-bot"));
		assert!(help.contains("api-gateway") || help.contains("http-api"));
		assert!(help.contains("task"));
		assert!(help.contains("approval"));
		assert!(help.contains("artifact"));
		assert!(help.contains("experiment"));
		assert!(help.contains("skill"));
		assert!(help.contains("memory"));
	}

	#[test]
	fn execute_skill_command_requires_expected_arguments() {
		// skill show without a name should fail
		let result = Cli::try_parse_from(["roku-cmd", "skill", "show"]);
		assert!(result.is_err(), "skill show without name should fail");

		// skill install without a source url should fail
		let result = Cli::try_parse_from(["roku-cmd", "skill", "install"]);
		assert!(result.is_err(), "skill install without url should fail");
	}

	#[test]
	fn parse_request_options_supports_session_and_skill_root_flags() {
		let cli = Cli::try_parse_from([
			"roku-cmd",
			"once",
			"--session-id",
			"chat-42",
			"--generated-skill-root",
			"/tmp/generated-skills",
			"investigate",
			"memory",
		])
		.expect("once args should parse");

		let Commands::Once(args) = cli.command.unwrap() else {
			panic!("expected Once command");
		};
		assert_eq!(args.session_id, "chat-42");
		assert_eq!(
			args.generated_skill_root,
			Some(PathBuf::from("/tmp/generated-skills"))
		);
		assert_eq!(args.goal.join(" "), "investigate memory");
	}

	#[test]
	fn parse_approval_decision_options_supports_actor_and_comment() {
		let cli = Cli::try_parse_from([
			"roku-cmd",
			"approval",
			"approve",
			"approval-123",
			"--actor",
			"reviewer",
			"--comment",
			"looks good",
		])
		.expect("approval approve should parse");

		let Commands::Approval(ApprovalCommand::Approve {
			approval_id,
			actor,
			comment,
		}) = cli.command.unwrap()
		else {
			panic!("expected Approval Approve command");
		};
		assert_eq!(approval_id, "approval-123");
		assert_eq!(actor, "reviewer");
		assert_eq!(comment.as_deref(), Some("looks good"));
	}

	#[test]
	fn parse_approval_decision_options_requires_actor() {
		// approve without --actor should fail
		let result = Cli::try_parse_from([
			"roku-cmd",
			"approval",
			"approve",
			"approval-123",
			"--comment",
			"missing actor",
		]);
		assert!(result.is_err(), "--actor should be required");
	}

	#[test]
	fn parse_download_output_path_supports_output_flag() {
		let cli = Cli::try_parse_from([
			"roku-cmd",
			"artifact",
			"download",
			"task-1",
			"artifact-1",
			"--output",
			"/tmp/artifact.txt",
		])
		.expect("artifact download should parse");

		let Commands::Artifact(ArtifactCommand::Download { output, .. }) = cli.command.unwrap()
		else {
			panic!("expected Artifact Download command");
		};
		assert_eq!(output, PathBuf::from("/tmp/artifact.txt"));
	}

	#[test]
	fn parse_download_output_path_requires_output_flag() {
		let result =
			Cli::try_parse_from(["roku-cmd", "artifact", "download", "task-1", "artifact-1"]);
		assert!(result.is_err(), "--output should be required");
	}

	#[test]
	fn parse_memory_search_options_requires_explicit_scope_identity() {
		// session scope (default) without session-id should fail at validation
		let cli = Cli::try_parse_from(["roku-cmd", "memory", "search", "recent preference"])
			.expect("memory search should parse structurally");
		let Commands::Memory(MemoryCommand::Search {
			scope,
			session_id,
			user_id,
			project_id,
			workspace_id,
			..
		}) = cli.command.unwrap()
		else {
			panic!("expected Memory Search command");
		};
		let scope: MemoryScope = scope.into();
		let result = validate_memory_scope_identity(
			scope,
			session_id.as_deref(),
			user_id.as_deref(),
			project_id.as_deref(),
			workspace_id.as_deref(),
		);
		assert!(
			result.is_err(),
			"session-scoped search should require an explicit session id"
		);
		assert!(
			result
				.unwrap_err()
				.to_string()
				.contains("--session-id is required")
		);

		// global scope should not require extra identity
		let cli = Cli::try_parse_from([
			"roku-cmd",
			"memory",
			"search",
			"--scope",
			"global",
			"recent preference",
		])
		.expect("global memory search should parse");
		let Commands::Memory(MemoryCommand::Search {
			scope,
			session_id,
			user_id,
			project_id,
			workspace_id,
			query,
			..
		}) = cli.command.unwrap()
		else {
			panic!("expected Memory Search command");
		};
		let scope: MemoryScope = scope.into();
		validate_memory_scope_identity(
			scope,
			session_id.as_deref(),
			user_id.as_deref(),
			project_id.as_deref(),
			workspace_id.as_deref(),
		)
		.expect("global search should not require extra identity");
		assert_eq!(scope, MemoryScope::Global);
		assert_eq!(query.join(" "), "recent preference");
	}

	#[test]
	fn parse_memory_write_options_requires_explicit_scope_identity() {
		// session scope (default) without session-id should fail at validation
		let cli = Cli::try_parse_from(["roku-cmd", "memory", "write", "remember this"])
			.expect("memory write should parse structurally");
		let Commands::Memory(MemoryCommand::Write {
			scope,
			session_id,
			user_id,
			project_id,
			workspace_id,
			..
		}) = cli.command.unwrap()
		else {
			panic!("expected Memory Write command");
		};
		let scope: MemoryScope = scope.into();
		let result = validate_memory_scope_identity(
			scope,
			session_id.as_deref(),
			user_id.as_deref(),
			project_id.as_deref(),
			workspace_id.as_deref(),
		);
		assert!(
			result.is_err(),
			"session-scoped write should require an explicit session id"
		);
		assert!(
			result
				.unwrap_err()
				.to_string()
				.contains("--session-id is required")
		);

		// global scope with summary and content should succeed
		let cli = Cli::try_parse_from([
			"roku-cmd",
			"memory",
			"write",
			"--scope",
			"global",
			"--summary",
			"global note",
			"remember this",
		])
		.expect("global memory write should parse");
		let Commands::Memory(MemoryCommand::Write {
			scope,
			session_id,
			user_id,
			project_id,
			workspace_id,
			summary,
			content,
			..
		}) = cli.command.unwrap()
		else {
			panic!("expected Memory Write command");
		};
		let scope: MemoryScope = scope.into();
		validate_memory_scope_identity(
			scope,
			session_id.as_deref(),
			user_id.as_deref(),
			project_id.as_deref(),
			workspace_id.as_deref(),
		)
		.expect("global write should not require extra identity");
		assert_eq!(scope, MemoryScope::Global);
		assert_eq!(summary.as_deref(), Some("global note"));
		assert_eq!(content.join(" "), "remember this");
	}
}
