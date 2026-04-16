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

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use crate::result::{policy_rejection_result, tool_failure_result, tool_success_result};
use crate::router::DirectRouteExecutionResult;
use crate::runtime_config::AgentRuntimeConfig;
use crate::runtime_loop::{
	AskUserPayload, AskUserResumeContract, LoopContext, LoopState, LoopStatus, StepAction,
	StepObservation, StepRecord, ToolObservation, attachments_for_tool, build_loop_context,
	build_tool_definitions, effective_ask_user_payload, intake_request, interpret_observation,
	next_working_directory_from_observation, runtime_loop_trace,
};
use crate::sub_agent::{SubAgentConfig, execute_sub_agent};
use crate::task_store::{TaskStatus, TaskStore};
use crate::tool_config::{ToolCatalogConfig, ToolsRuntimeConfig};
use crate::tools::{
	LoopMode, ToolRegistry,
	build_builtin_tool_runtime_with_plugin_snapshot_and_runtime_capabilities_and_runtime_config,
	build_llm_tool_runtime_with_plugin_snapshot_and_runtime_config,
	build_resource_catalog_with_plugin_snapshot_and_runtime_capabilities_and_runtime_config,
	execution_elapsed_ms, observation_from_execution, raw_tool_output_from_result,
	register_catalog_tools, tool_result_cap, tool_selector_by_name, truncate_raw_tool_output,
	truncate_tool_result_for_message,
};
use crate::workers::{skill_execute_worker_with_config, skill_worker_with_config};
use roku_common_types::{
	AgentContext, AggregationMode, CanonicalExecution, ConversationRole, ConversationTurn,
	EvidenceItem, JoinPolicy, NodeBudgetSnapshot, NodeId, PolicyBindings, RequestEnvelope,
	RerunPolicy, ResourceSelector, ResultStatus, RetryPolicy, RuntimeMemorySections, Task, TaskId,
	TaskNodeDispatchPolicy, TaskNodeKind,
};
use roku_common_types::{AgentInstanceSpec, ResultEnvelope, TaskNode};
use roku_plugin_host::{
	PluginRegistrySnapshot, ToolExecutionResult, ToolInvocation, ToolRuntime, ToolRuntimeError,
};
use roku_plugin_llm::{
	GenerationRequest, LlmAdapterError, LlmRouter, Message, RiskTier, StreamChunk, ThinkingEffort,
	ToolCallBlock,
};
use roku_plugin_skills::SkillRegistry;
use roku_plugin_tools::{
	PSEUDO_AGENT, PSEUDO_TASK_CREATE, PSEUDO_TASK_GET, PSEUDO_TASK_LIST, PSEUDO_TASK_UPDATE,
	PSEUDO_TOOL_SEARCH, ResourceCatalog,
};
use roku_plugin_tools::{
	RuntimeVisibleToolAvailabilitySnapshot, build_runtime_visible_tool_availability_snapshot,
	canonical_execution_for_builtin_tool_input,
};
use serde::Deserialize;
use serde_json::{Value, json};

pub trait AgentWorker {
	fn execute(&self, spec: &AgentInstanceSpec, node: &TaskNode) -> ResultEnvelope;
}

pub trait RuntimeWorker: Send + Sync {
	fn worker_id(&self) -> &'static str;
	fn supports(&self, capabilities: &[String]) -> bool;
	fn execute(&self, spec: &AgentInstanceSpec, node: &TaskNode) -> ResultEnvelope;
}

struct WorkerRegistryEntry {
	priority: u8,
	worker: Arc<dyn RuntimeWorker>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AwaitingUserResumeAssessment {
	pub should_resume: bool,
	pub reason: String,
}

#[derive(Debug, Deserialize)]
struct AwaitingUserResumeDecision {
	resume_existing_loop: bool,
	reason: String,
}

pub struct GenericAgentRuntime {
	workers: Vec<WorkerRegistryEntry>,
	tool_runtime: Arc<ToolRuntime>,
	resource_catalog: ResourceCatalog,
	runtime_visible_tool_availability_snapshot: RuntimeVisibleToolAvailabilitySnapshot,
	tool_config: ToolCatalogConfig,
	plugin_snapshot: PluginRegistrySnapshot,
	route_router: Option<Arc<LlmRouter>>,
	execution_router: Option<Arc<LlmRouter>>,
	agent_runtime_config: AgentRuntimeConfig,
	/// Tool registry for mode-aware tool filtering and definition building.
	tool_registry: ToolRegistry,
	/// Current operational mode (Normal or Plan).
	loop_mode: LoopMode,
	/// Sub-agent execution configuration.
	sub_agent_config: SubAgentConfig,
	/// Session-scoped task store for structured progress tracking.
	task_store: std::sync::Mutex<TaskStore>,
	/// Keepalive for the MCP bootstrap tokio runtime. The rmcp serve loop tasks
	/// are spawned on this runtime during MCP server connection. Dropping it
	/// would kill those tasks and break all MCP tool calls. Never accessed
	/// directly — its sole purpose is preventing the drop.
	_mcp_runtime: Option<Arc<tokio::runtime::Runtime>>,
}

impl GenericAgentRuntime {
	pub fn with_tool_runtime(
		tool_runtime: ToolRuntime,
		resource_catalog: ResourceCatalog,
		tool_config: ToolCatalogConfig,
	) -> Self {
		Self::with_tool_runtime_and_plugin_snapshot(
			tool_runtime,
			resource_catalog,
			tool_config,
			PluginRegistrySnapshot::permissive(),
		)
	}

	pub fn with_tool_runtime_and_plugin_snapshot(
		tool_runtime: ToolRuntime,
		resource_catalog: ResourceCatalog,
		tool_config: ToolCatalogConfig,
		plugin_snapshot: PluginRegistrySnapshot,
	) -> Self {
		Self::with_tool_runtime_and_plugin_snapshot_and_runtime_config(
			tool_runtime,
			resource_catalog,
			tool_config,
			plugin_snapshot,
			AgentRuntimeConfig::default(),
		)
	}

	pub fn with_tool_runtime_and_plugin_snapshot_and_runtime_config(
		tool_runtime: ToolRuntime,
		resource_catalog: ResourceCatalog,
		tool_config: ToolCatalogConfig,
		plugin_snapshot: PluginRegistrySnapshot,
		agent_runtime_config: AgentRuntimeConfig,
	) -> Self {
		let runtime_visible_tool_availability_snapshot =
			build_runtime_visible_tool_availability_snapshot(
				&resource_catalog,
				&safe_baseline_tool_pool(&agent_runtime_config.r#loop),
			);
		let mut tool_registry = ToolRegistry::new();
		register_catalog_tools(&mut tool_registry, &resource_catalog);
		let shared_tool_runtime = Arc::new(tool_runtime);
		let mut runtime = Self {
			workers: Vec::new(),
			tool_runtime: Arc::clone(&shared_tool_runtime),
			resource_catalog,
			runtime_visible_tool_availability_snapshot,
			tool_config: tool_config.clone(),
			plugin_snapshot,
			route_router: None,
			execution_router: None,
			agent_runtime_config,
			tool_registry,
			loop_mode: LoopMode::Normal,
			sub_agent_config: SubAgentConfig::default(),
			task_store: std::sync::Mutex::new(TaskStore::new()),
			_mcp_runtime: None,
		};
		runtime.register_worker(
			96,
			skill_execute_worker_with_config(Arc::clone(&shared_tool_runtime), &tool_config),
		);
		runtime.register_worker(
			95,
			skill_worker_with_config(Arc::clone(&shared_tool_runtime), &tool_config),
		);
		let _ = shared_tool_runtime;
		runtime
	}

	pub fn with_skill_registry(skill_registry: SkillRegistry) -> Self {
		Self::with_skill_registry_and_tool_config(skill_registry, ToolCatalogConfig::default())
	}

	pub fn with_skill_registry_and_tool_config(
		skill_registry: SkillRegistry,
		tool_config: ToolCatalogConfig,
	) -> Self {
		Self::with_skill_registry_tool_config_and_plugin_snapshot(
			skill_registry,
			tool_config,
			PluginRegistrySnapshot::permissive(),
			ToolsRuntimeConfig::default(),
		)
	}

	pub fn with_skill_registry_tool_config_and_plugin_snapshot(
		skill_registry: SkillRegistry,
		tool_config: ToolCatalogConfig,
		plugin_snapshot: PluginRegistrySnapshot,
		tools_runtime_config: ToolsRuntimeConfig,
	) -> Self {
		Self::with_skill_registry_tool_config_and_plugin_snapshot_and_runtime_config(
			skill_registry,
			tool_config,
			plugin_snapshot,
			tools_runtime_config,
			AgentRuntimeConfig::default(),
		)
	}

	pub fn with_skill_registry_tool_config_and_plugin_snapshot_and_runtime_config(
		skill_registry: SkillRegistry,
		tool_config: ToolCatalogConfig,
		plugin_snapshot: PluginRegistrySnapshot,
		tools_runtime_config: ToolsRuntimeConfig,
		agent_runtime_config: AgentRuntimeConfig,
	) -> Self {
		let resource_catalog =
			build_resource_catalog_with_plugin_snapshot_and_runtime_capabilities_and_runtime_config(
				&skill_registry,
				&tool_config,
				&plugin_snapshot,
				&tools_runtime_config,
				false,
			);
		Self::with_tool_runtime_and_plugin_snapshot_and_runtime_config(
			build_builtin_tool_runtime_with_plugin_snapshot_and_runtime_capabilities_and_runtime_config(
				skill_registry,
				&tool_config,
				&plugin_snapshot,
				&tools_runtime_config,
				false,
			),
			resource_catalog,
			tool_config,
			plugin_snapshot,
			agent_runtime_config,
		)
	}

	pub fn with_llm_router(router: LlmRouter) -> Self {
		Self::with_llm_router_and_skill_registry(router, SkillRegistry::disabled())
	}

	pub fn with_llm_router_and_skill_registry(
		router: LlmRouter,
		skill_registry: SkillRegistry,
	) -> Self {
		Self::with_llm_router_skill_registry_and_tool_config(
			router,
			skill_registry,
			ToolCatalogConfig::default(),
		)
	}

	pub fn with_llm_router_skill_registry_and_tool_config(
		router: LlmRouter,
		skill_registry: SkillRegistry,
		tool_config: ToolCatalogConfig,
	) -> Self {
		Self::with_llm_router_skill_registry_tool_config_and_plugin_snapshot(
			router,
			skill_registry,
			tool_config,
			PluginRegistrySnapshot::permissive(),
			ToolsRuntimeConfig::default(),
		)
	}

	pub fn with_llm_router_skill_registry_tool_config_and_plugin_snapshot(
		router: LlmRouter,
		skill_registry: SkillRegistry,
		tool_config: ToolCatalogConfig,
		plugin_snapshot: PluginRegistrySnapshot,
		tools_runtime_config: ToolsRuntimeConfig,
	) -> Self {
		Self::with_llm_router_skill_registry_tool_config_and_plugin_snapshot_and_runtime_config(
			router,
			skill_registry,
			tool_config,
			plugin_snapshot,
			tools_runtime_config,
			AgentRuntimeConfig::default(),
		)
	}

	pub fn with_llm_router_skill_registry_tool_config_and_plugin_snapshot_and_runtime_config(
		router: LlmRouter,
		skill_registry: SkillRegistry,
		tool_config: ToolCatalogConfig,
		plugin_snapshot: PluginRegistrySnapshot,
		tools_runtime_config: ToolsRuntimeConfig,
		agent_runtime_config: AgentRuntimeConfig,
	) -> Self {
		let shared_router = Arc::new(router);
		Self::with_llm_execution_and_route_routers(
			Arc::clone(&shared_router),
			shared_router,
			skill_registry,
			tool_config,
			plugin_snapshot,
			tools_runtime_config,
			agent_runtime_config,
		)
	}

	pub fn with_route_and_execution_routers_skill_registry_tool_config_and_plugin_snapshot(
		route_router: LlmRouter,
		execution_router: LlmRouter,
		skill_registry: SkillRegistry,
		tool_config: ToolCatalogConfig,
		plugin_snapshot: PluginRegistrySnapshot,
		tools_runtime_config: ToolsRuntimeConfig,
	) -> Self {
		Self::with_route_and_execution_routers_skill_registry_tool_config_and_plugin_snapshot_and_runtime_config(
			route_router,
			execution_router,
			skill_registry,
			tool_config,
			plugin_snapshot,
			tools_runtime_config,
			AgentRuntimeConfig::default(),
		)
	}

	pub fn with_route_and_execution_routers_skill_registry_tool_config_and_plugin_snapshot_and_runtime_config(
		route_router: LlmRouter,
		execution_router: LlmRouter,
		skill_registry: SkillRegistry,
		tool_config: ToolCatalogConfig,
		plugin_snapshot: PluginRegistrySnapshot,
		tools_runtime_config: ToolsRuntimeConfig,
		agent_runtime_config: AgentRuntimeConfig,
	) -> Self {
		Self::with_llm_execution_and_route_routers(
			Arc::new(execution_router),
			Arc::new(route_router),
			skill_registry,
			tool_config,
			plugin_snapshot,
			tools_runtime_config,
			agent_runtime_config,
		)
	}

	fn with_llm_execution_and_route_routers(
		execution_router: Arc<LlmRouter>,
		route_router: Arc<LlmRouter>,
		skill_registry: SkillRegistry,
		tool_config: ToolCatalogConfig,
		plugin_snapshot: PluginRegistrySnapshot,
		tools_runtime_config: ToolsRuntimeConfig,
		agent_runtime_config: AgentRuntimeConfig,
	) -> Self {
		let resource_catalog =
			build_resource_catalog_with_plugin_snapshot_and_runtime_capabilities_and_runtime_config(
				&skill_registry,
				&tool_config,
				&plugin_snapshot,
				&tools_runtime_config,
				true,
			);
		Self::with_tool_runtime_and_plugin_snapshot_and_runtime_config(
			build_llm_tool_runtime_with_plugin_snapshot_and_runtime_config(
				Arc::clone(&execution_router),
				skill_registry,
				&tool_config,
				&resource_catalog,
				&plugin_snapshot,
				&tools_runtime_config,
			),
			resource_catalog,
			tool_config,
			plugin_snapshot,
			agent_runtime_config,
		)
		.with_execution_router(Arc::clone(&execution_router))
		.with_route_router(route_router)
	}

	/// Like `with_route_and_execution_routers_...` but also integrates external MCP tools.
	///
	/// MCP catalog entries and tools are merged into the standard catalog/runtime after
	/// deduplication by name. Entries whose name collides with a builtin are skipped with
	/// a warning. This constructor is intended for CLI bootstrap where MCP connections have
	/// already been established.
	pub fn with_routers_and_mcp(
		route_router: LlmRouter,
		execution_router: LlmRouter,
		skill_registry: SkillRegistry,
		tool_config: ToolCatalogConfig,
		plugin_snapshot: PluginRegistrySnapshot,
		tools_runtime_config: ToolsRuntimeConfig,
		agent_runtime_config: AgentRuntimeConfig,
		mcp_catalog_entries: Vec<roku_plugin_tools::CatalogDescriptor>,
		mcp_tools: Vec<Box<dyn roku_plugin_host::Tool>>,
		mcp_runtime: Option<Arc<tokio::runtime::Runtime>>,
	) -> Self {
		use roku_common_types::{LogLevel, LogRecord, emit_global_log};

		let mut resource_catalog =
			build_resource_catalog_with_plugin_snapshot_and_runtime_capabilities_and_runtime_config(
				&skill_registry,
				&tool_config,
				&plugin_snapshot,
				&tools_runtime_config,
				true,
			);

		let execution_router = Arc::new(execution_router);
		let mut tool_runtime = build_llm_tool_runtime_with_plugin_snapshot_and_runtime_config(
			Arc::clone(&execution_router),
			skill_registry,
			&tool_config,
			&resource_catalog,
			&plugin_snapshot,
			&tools_runtime_config,
		);

		// Dedup: skip MCP entries whose name collides with existing catalog entries.
		let existing_names: std::collections::HashSet<&str> = resource_catalog
			.entries()
			.iter()
			.map(|e| e.name.as_str())
			.collect();

		let filtered_entries: Vec<_> = mcp_catalog_entries
			.into_iter()
			.filter(|e| {
				if existing_names.contains(e.name.as_str()) {
					let _ = emit_global_log(LogRecord::new(
						"roku-agent-runtime",
						LogLevel::Warn,
						format!("MCP tool '{}' skipped: name collision with builtin", e.name),
					));
					false
				} else {
					true
				}
			})
			.collect();

		resource_catalog.extend(filtered_entries);

		for tool in mcp_tools {
			if let Err(e) = tool_runtime.register_tool_boxed(tool) {
				let _ = emit_global_log(LogRecord::new(
					"roku-agent-runtime",
					LogLevel::Warn,
					format!("failed to register MCP tool: {}", e),
				));
			}
		}

		let mut runtime = Self::with_tool_runtime_and_plugin_snapshot_and_runtime_config(
			tool_runtime,
			resource_catalog,
			tool_config,
			plugin_snapshot,
			agent_runtime_config,
		)
		.with_execution_router(execution_router)
		.with_route_router(Arc::new(route_router));
		runtime._mcp_runtime = mcp_runtime;
		runtime
	}

	pub fn resource_catalog(&self) -> &ResourceCatalog {
		&self.resource_catalog
	}

	pub(crate) fn agent_runtime_config(&self) -> &AgentRuntimeConfig {
		&self.agent_runtime_config
	}

	/// Access the tool registry for mode-aware queries.
	pub fn tool_registry(&self) -> &ToolRegistry {
		&self.tool_registry
	}

	/// Get the current loop mode.
	pub fn loop_mode(&self) -> LoopMode {
		self.loop_mode
	}

	/// Set the loop mode (Normal or Plan).
	pub fn set_loop_mode(&mut self, mode: LoopMode) {
		self.loop_mode = mode;
	}

	/// Returns the list of model IDs available to the execution router.
	pub fn available_models(&self) -> Vec<String> {
		self.execution_router
			.as_ref()
			.map(|r| r.available_models())
			.unwrap_or_default()
	}

	pub fn tool_config(&self) -> &ToolCatalogConfig {
		&self.tool_config
	}

	pub fn plugin_snapshot(&self) -> &PluginRegistrySnapshot {
		&self.plugin_snapshot
	}

	async fn assess_missing_required_input_resume(
		&self,
		loop_state: &LoopState,
		payload: &AskUserPayload,
		user_input: &str,
		fields: &[String],
	) -> AwaitingUserResumeAssessment {
		let Some(router) = self.route_router.as_deref() else {
			return AwaitingUserResumeAssessment {
				should_resume: false,
				reason:
					"missing-input clarification requires a live router to distinguish resume from a fresh request"
						.to_string(),
			};
		};
		let response = match router
			.generate_json_value(&GenerationRequest {
				system_prompt: Some(
					"You are Roku's paused-loop resume gate. Return only valid JSON.".to_string(),
				),
				prompt: awaiting_user_resume_prompt(
					loop_state,
					&payload.final_message,
					fields,
					user_input,
				),
				messages: None,
				expected_output_tokens: 96,
				risk_tier: RiskTier::Low,
				preferred_provider: None,
				budget_tokens_remaining: self.agent_runtime_config.router.budget_tokens_remaining,
				budget_cost_remaining_usd: self
					.agent_runtime_config
					.router
					.budget_cost_remaining_usd,
				tools: None,
				model_override: None,
				thinking_effort: None,
				system_prompt_sections: None,
			})
			.await
		{
			Ok(response) => response,
			Err(error) => {
				return AwaitingUserResumeAssessment {
					should_resume: false,
					reason: format!("resume gate could not produce a structured decision: {error}"),
				};
			}
		};
		let decision = match serde_json::from_value::<AwaitingUserResumeDecision>(response.value) {
			Ok(decision) => decision,
			Err(error) => {
				return AwaitingUserResumeAssessment {
					should_resume: false,
					reason: format!("resume gate returned an invalid decision payload: {error}"),
				};
			}
		};
		AwaitingUserResumeAssessment {
			should_resume: decision.resume_existing_loop,
			reason: decision.reason,
		}
	}

	pub fn build_loop_context(
		&self,
		request: &RequestEnvelope,
		_session_id: &str,
		route_decision: &crate::router::RouteDecision,
		bound_resources: Vec<ResourceSelector>,
	) -> LoopContext {
		let loop_request = intake_request(request);
		build_loop_context(
			&loop_request,
			route_decision,
			self.visible_tools_for_decision(route_decision),
			bound_resources,
		)
	}

	pub fn initialize_runtime_loop(
		&self,
		request: &RequestEnvelope,
		session_id: &str,
		route_decision: &crate::router::RouteDecision,
		bound_resources: Vec<ResourceSelector>,
	) -> LoopState {
		let context = self.build_loop_context(request, session_id, route_decision, bound_resources);
		LoopState::with_budgets(
			format!("loop-{}", request.request_id.0),
			&context,
			self.agent_runtime_config.r#loop.initial_step_budget,
			self.agent_runtime_config.r#loop.initial_recovery_budget,
		)
	}

	pub async fn assess_awaiting_user_resume(
		&self,
		loop_state: &LoopState,
		user_input: &str,
	) -> AwaitingUserResumeAssessment {
		let trimmed = user_input.trim();
		if loop_state.status != LoopStatus::AwaitingUser {
			return AwaitingUserResumeAssessment {
				should_resume: false,
				reason: "loop is not currently awaiting user input".to_string(),
			};
		}
		if trimmed.is_empty() {
			return AwaitingUserResumeAssessment {
				should_resume: false,
				reason: "empty replies cannot resume a paused loop".to_string(),
			};
		}
		let Some(payload) = loop_state.awaiting_user.as_ref() else {
			return AwaitingUserResumeAssessment {
				should_resume: false,
				reason: "paused loop is missing an awaiting-user payload".to_string(),
			};
		};
		match &payload.resume_contract {
			AskUserResumeContract::CandidateSelection { .. } => {
				let should_resume = payload.can_resume(trimmed);
				AwaitingUserResumeAssessment {
					should_resume,
					reason: if should_resume {
						"user reply selected one of the grounded candidates for the paused loop"
							.to_string()
					} else {
						"user reply did not explicitly select one of the grounded candidates"
							.to_string()
					},
				}
			}
			AskUserResumeContract::NoAutomaticResume => AwaitingUserResumeAssessment {
				should_resume: false,
				reason:
					"freeform clarification pauses do not auto-resume; treat the next message as a fresh intake"
						.to_string(),
			},
			AskUserResumeContract::MissingRequiredInput { fields } => {
				self.assess_missing_required_input_resume(loop_state, payload, trimmed, fields)
					.await
			}
		}
	}

	pub fn record_terminal_step(
		&self,
		loop_state: &mut LoopState,
		action: StepAction,
		reason: impl Into<String>,
		final_message: Option<String>,
	) -> StepRecord {
		let awaiting_user = if action == StepAction::AskUser {
			final_message.clone().map(AskUserPayload::freeform)
		} else {
			None
		};
		let observation = final_message.map(|message| match action {
			StepAction::AskUser => StepObservation::AskUser {
				final_message: message,
			},
			StepAction::FinalAnswer
			| StepAction::Fail
			| StepAction::CallTool
			| StepAction::Stop
			| StepAction::CompactBoundary => StepObservation::FinalMessage {
				final_message: message,
			},
		});
		let reason = reason.into();
		let step = StepRecord::terminal(
			loop_state.step_index + 1,
			action,
			terminal_decision(action, &reason, observation.as_ref()),
			loop_state.visible_tools.clone(),
			loop_state.bound_resources.clone(),
			observation,
			match action {
				StepAction::AskUser => loop_state.remaining_step_budget,
				StepAction::FinalAnswer | StepAction::Fail | StepAction::CallTool => {
					loop_state.remaining_step_budget.saturating_sub(1)
				}
				StepAction::Stop | StepAction::CompactBoundary => loop_state.remaining_step_budget,
			},
			loop_state.remaining_recovery_budget,
			loop_state.working_directory.clone(),
		);
		loop_state.record_step(step.clone());
		loop_state.awaiting_user = awaiting_user;
		loop_state.status = match action {
			StepAction::FinalAnswer => crate::runtime_loop::LoopStatus::Succeeded,
			StepAction::Fail => crate::runtime_loop::LoopStatus::Failed,
			StepAction::AskUser => crate::runtime_loop::LoopStatus::AwaitingUser,
			StepAction::CallTool | StepAction::CompactBoundary => {
				crate::runtime_loop::LoopStatus::LoopRunning
			}
			StepAction::Stop => crate::runtime_loop::LoopStatus::Stopped,
		};
		step
	}

	pub fn record_ask_user_step(
		&self,
		loop_state: &mut LoopState,
		reason: impl Into<String>,
		payload: AskUserPayload,
	) -> StepRecord {
		let reason = reason.into();
		let step = StepRecord::terminal(
			loop_state.step_index + 1,
			StepAction::AskUser,
			terminal_decision(
				StepAction::AskUser,
				&reason,
				Some(&StepObservation::AskUser {
					final_message: payload.final_message.clone(),
				}),
			),
			loop_state.visible_tools.clone(),
			loop_state.bound_resources.clone(),
			Some(StepObservation::AskUser {
				final_message: payload.final_message.clone(),
			}),
			loop_state.remaining_step_budget,
			loop_state.remaining_recovery_budget,
			loop_state.working_directory.clone(),
		);
		loop_state.record_step(step.clone());
		loop_state.awaiting_user = Some(payload);
		loop_state.status = crate::runtime_loop::LoopStatus::AwaitingUser;
		step
	}

	pub fn tool_result_to_observation(&self, execution: &ToolExecutionResult) -> ToolObservation {
		ToolObservation::from_execution_result(execution)
	}

	pub fn tool_error_to_observation(
		&self,
		tool_name: &str,
		error: &ToolRuntimeError,
	) -> ToolObservation {
		ToolObservation::from_runtime_error(tool_name, error, &self.resource_catalog)
	}

	/// Returns `(prompt_tokens, output_tokens)` consumed by compaction LLM calls.
	async fn maybe_compact(
		&self,
		loop_state: &mut LoopState,
		messages: &mut Vec<roku_plugin_llm::Message>,
		current_step_index: u32,
		event_sender: Option<&crate::runtime_loop::LoopEventSender>,
		system_prompt: &str,
	) -> (u64, u64) {
		// Truncate oversized tool results in messages.
		let tool_result_max_chars = self.agent_runtime_config.r#loop.working_summary_max_chars;
		crate::runtime_loop::truncate_large_tool_results(messages, tool_result_max_chars);

		let threshold = self.agent_runtime_config.r#loop.compact_threshold_tokens();
		let estimated = crate::runtime_loop::estimate_prompt_pressure(
			messages,
			Some(system_prompt),
			&loop_state.estimator_calibration,
		);
		if estimated > threshold {
			let _ = roku_common_types::emit_global_log(roku_common_types::LogRecord::new(
				"roku-runtime-service",
				roku_common_types::LogLevel::Info,
				format!(
					"context compact triggered: estimated {estimated} tokens exceeds threshold {threshold}"
				),
			));
			if let Some(sender) = event_sender {
				let _ = sender.send(crate::runtime_loop::LoopEvent::CompactTriggered {
					step: current_step_index,
					estimated_tokens: estimated,
				});
			}
			return self
				.run_compaction(loop_state, messages, current_step_index, event_sender)
				.await;
		}
		(0, 0)
	}

	/// Reactive compaction triggered by a provider `context_window_exceeded` error.
	///
	/// Bypasses the threshold check used by [`Self::maybe_compact`] — the
	/// provider has already told us the prompt is too large, so we always
	/// compact and emit a [`crate::runtime_loop::LoopEvent::ReactiveCompactTriggered`]
	/// event before doing the work. Returns the `(prompt_tokens, output_tokens)`
	/// consumed by any LLM-assisted summarization step so the caller can fold
	/// the cost into the per-turn accumulators.
	async fn reactive_compact(
		&self,
		loop_state: &mut LoopState,
		messages: &mut Vec<roku_plugin_llm::Message>,
		current_step_index: u32,
		event_sender: Option<&crate::runtime_loop::LoopEventSender>,
		detail: &str,
	) -> (u64, u64) {
		let tool_result_max_chars = self.agent_runtime_config.r#loop.working_summary_max_chars;
		crate::runtime_loop::truncate_large_tool_results(messages, tool_result_max_chars);

		let _ = roku_common_types::emit_global_log(roku_common_types::LogRecord::new(
			"roku-runtime-service",
			roku_common_types::LogLevel::Warn,
			format!(
				"reactive compact triggered: provider reported context_window_exceeded: {detail}"
			),
		));
		if let Some(sender) = event_sender {
			let _ = sender.send(crate::runtime_loop::LoopEvent::ReactiveCompactTriggered {
				step: current_step_index,
				detail: detail.to_string(),
			});
		}
		self.run_compaction(loop_state, messages, current_step_index, event_sender)
			.await
	}

	/// Shared body used by both threshold-gated [`Self::maybe_compact`] and
	/// reactive [`Self::reactive_compact`]. Performs LLM-assisted message and
	/// history compaction (or mechanical fallback when no router is available)
	/// and emits a `CompactComplete` event when finished.
	async fn run_compaction(
		&self,
		loop_state: &mut LoopState,
		messages: &mut Vec<roku_plugin_llm::Message>,
		current_step_index: u32,
		event_sender: Option<&crate::runtime_loop::LoopEventSender>,
	) -> (u64, u64) {
		let compact_config = crate::runtime_loop::CompactConfig {
			retain_tail_steps: self.agent_runtime_config.r#loop.retain_tail_steps,
			working_summary_max_chars: self.agent_runtime_config.r#loop.working_summary_max_chars,
			..Default::default()
		};
		let compact_start = std::time::Instant::now();
		// Compact conversation messages (preserve recent 5).
		let retain_messages = 5_usize.max(compact_config.retain_tail_steps);
		let breaker_tripped = loop_state.autocompact_circuit_breaker_tripped();
		let llm_succeeded;
		let compact_tokens;
		match (self.route_router.as_deref(), breaker_tripped) {
			(Some(router), false) => {
				let outcome = crate::runtime_loop::compact_messages_with_structured_summary(
					messages,
					retain_messages,
					router,
					&compact_config,
				)
				.await;
				if let Some(sender) = event_sender {
					let _ = sender.send(
						crate::runtime_loop::LoopEvent::AutoCompactSummarizerCalled {
							step: current_step_index,
							prompt_tokens: outcome.prompt_tokens,
							output_tokens: outcome.output_tokens,
							succeeded: outcome.succeeded,
							drop_oldest_retries: outcome.drop_oldest_retries,
						},
					);
				}
				if outcome.succeeded {
					let history_ok = crate::runtime_loop::compact_history_with_llm(
						loop_state,
						&compact_config,
						router,
					)
					.await;
					llm_succeeded = history_ok;
					loop_state.note_autocompact_success();
				} else {
					crate::runtime_loop::compact_history(loop_state, &compact_config);
					llm_succeeded = false;
					// Only drive the breaker on real LLM failure paths, not on
					// `None` (nothing-to-compact, which returns succeeded=false
					// with no error).
					if outcome.error.is_some() {
						let just_tripped = loop_state.note_autocompact_failure();
						if just_tripped && let Some(sender) = event_sender {
							let _ = sender.send(
								crate::runtime_loop::LoopEvent::AutoCompactCircuitBreakerTripped {
									step: current_step_index,
									consecutive_failures: loop_state
										.consecutive_autocompact_failures,
								},
							);
						}
					}
				}
				compact_tokens = (outcome.prompt_tokens, outcome.output_tokens);
			}
			_ => {
				// No router, or breaker tripped — mechanical fallback only.
				crate::runtime_loop::compact_messages(messages, retain_messages);
				crate::runtime_loop::compact_history(loop_state, &compact_config);
				llm_succeeded = false;
				compact_tokens = (0, 0);
			}
		}
		if let Some(sender) = event_sender {
			let _ = sender.send(crate::runtime_loop::LoopEvent::CompactComplete {
				step: current_step_index,
				llm_succeeded,
				elapsed_ms: compact_start.elapsed().as_millis() as u64,
			});
		}
		compact_tokens
	}

	pub async fn execute_tool_loop(
		&self,
		task_id: &TaskId,
		request: &RequestEnvelope,
		loop_state: &mut LoopState,
		runtime_memory_sections: &RuntimeMemorySections,
		user_reply: Option<&str>,
		event_sender: Option<&crate::runtime_loop::LoopEventSender>,
		approval_gate: Option<&dyn crate::runtime_loop::approval::ToolApprovalGate>,
	) -> DirectRouteExecutionResult {
		// Without an LLM router the message-based loop cannot make decisions.
		// Return a graceful completion — the runtime still functions for
		// orchestration, approval, and memory paths.
		let Some(router) = self.route_router.as_deref() else {
			let message = user_reply.unwrap_or(&loop_state.goal).to_string();
			self.record_terminal_step(
				loop_state,
				StepAction::FinalAnswer,
				"No LLM router available; echoing user input as final answer.",
				Some(message.clone()),
			);
			return self.synthetic_loop_terminal_result(
				task_id,
				"tool",
				message,
				StepAction::FinalAnswer,
				ResultStatus::Ok,
				Some(loop_state),
			);
		};

		let grounding_input = user_reply.unwrap_or(&loop_state.goal).to_string();
		loop_state.note_grounding_input(&grounding_input);

		// Build the system prompt once (environment context uses working_directory
		// which may change across steps, but we read it fresh each turn below).
		// Build initial conversation history from prior turns in the request.
		let mut messages: Vec<Message> = request
			.conversation_history
			.iter()
			.filter_map(|turn| match turn.role {
				ConversationRole::User => Some(Message::User {
					content: turn.content.clone(),
				}),
				ConversationRole::Assistant => Some(Message::Assistant {
					text: turn.content.clone(),
					tool_calls: Vec::new(),
				}),
				ConversationRole::System => None,
			})
			.collect();

		// Append the current user message (either a resume reply or the original goal).
		let initial_user_content = user_reply.unwrap_or(&loop_state.goal).to_string();
		messages.push(Message::User {
			content: initial_user_content,
		});

		// Load project instructions once from the initial working directory.
		let project_instruction = crate::runtime_loop::system_prompt::load_project_instructions(
			&loop_state.working_directory,
		);

		// Per-turn token accumulators: summed across all LLM calls in this loop execution.
		let mut total_prompt_tokens: u64 = 0;
		let mut total_output_tokens: u64 = 0;
		// Per-tier cache accumulators — additive counters mirroring the
		// provider's `usage.cache_*_input_tokens` (Anthropic) and
		// `usage.*_tokens_details.cached_tokens` (OpenAI). Stay at `0`
		// through every turn until 09 / 10 land the cache markers /
		// prompt_cache_key; after that they track the real cache activity
		// emitted alongside `prompt_tokens` / `output_tokens`.
		let mut total_cache_creation_input_tokens: u64 = 0;
		let mut total_cache_read_input_tokens: u64 = 0;
		// Track the model that served this request (updated on each successful LLM call).
		let mut last_model_id: Option<String> = None;

		// Cost constants: rough estimate for Claude Sonnet tier.
		const COST_PER_M_INPUT_TOKENS_USD: f64 = 3.0;
		const COST_PER_M_OUTPUT_TOKENS_USD: f64 = 15.0;

		loop {
			// Refresh visible tools at the start of each turn. This also
			// detects plan-mode transitions and `disallowed_tools` pushes,
			// marking `loop_state.tool_schema_dirty` on the turn the
			// transition occurs.
			self.refresh_tool_loop_visible_tools(loop_state);
			let fresh_tool_definitions = build_tool_definitions(
				&loop_state.visible_tools,
				Some(&self.resource_catalog),
				&loop_state.disallowed_tools,
			);
			// Freeze on first build; on subsequent clean turns return the
			// cached `Vec<ToolDefinition>` so the provider adapter sees
			// byte-identical tool bytes and cache markers stay valid.
			let tool_definitions = loop_state.freeze_or_reuse_tool_schema(fresh_tool_definitions);
			// Apply threshold-based deferred schema loading: when the total
			// estimated schema token cost exceeds the configured fraction of
			// the context window, non-core tool schemas are withheld from the
			// LLM and a `tool_search` pseudo-tool is injected so the model can
			// load them on demand.
			let tool_definitions = crate::runtime_loop::apply_deferred_mode(
				tool_definitions,
				loop_state,
				self.agent_runtime_config.r#loop.context_window_tokens,
			);

			// Check step budget before calling the LLM.
			if loop_state.remaining_step_budget == 0 {
				let message = format!(
					"Step budget exhausted after {} steps; goal: {}",
					loop_state.step_index, loop_state.goal,
				);
				self.record_terminal_step(
					loop_state,
					StepAction::Fail,
					"Step budget exhausted.",
					Some(message.clone()),
				);
				emit_token_usage(
					event_sender,
					loop_state.step_index.saturating_add(1),
					total_prompt_tokens,
					total_output_tokens,
					COST_PER_M_INPUT_TOKENS_USD,
					COST_PER_M_OUTPUT_TOKENS_USD,
					last_model_id.as_deref(),
					total_cache_creation_input_tokens,
					total_cache_read_input_tokens,
				);
				return self.synthetic_loop_terminal_result(
					task_id,
					"tool",
					message,
					StepAction::Fail,
					ResultStatus::Error,
					Some(loop_state),
				);
			}

			// Build the modular system prompt with environment + project instructions.
			// Environment is re-probed each turn; project instruction is stable (loaded once above).
			// The structured `sections` form carries the static/dynamic split that
			// prompt-cache adapters will consume; `system_prompt` keeps the single
			// String shape for adapters that haven't migrated yet.
			let env_snapshot = crate::runtime_loop::environment::probe_environment();
			let system_prompt_sections =
				crate::runtime_loop::system_prompt::build_system_prompt_sections(
					env_snapshot,
					&loop_state.working_directory,
					project_instruction.as_deref(),
					Some(runtime_memory_sections),
					self.loop_mode == LoopMode::Plan,
				);
			let system_prompt = system_prompt_sections.flatten();

			// Cache break detector: snapshot the prompt prefix components
			// (static system blocks + tool schema + model) so the post-call
			// check can identify which component diverged when a break fires.
			let model_for_fingerprint = request.model_override.as_deref().unwrap_or("");
			loop_state.cache_break_detector.record_prompt_state(
				&system_prompt_sections.static_blocks,
				&tool_definitions,
				model_for_fingerprint,
			);

			let config = &self.agent_runtime_config.next_step;
			let thinking_effort = request.thinking_effort.as_deref().and_then(|s| match s {
				"low" => Some(ThinkingEffort::Low),
				"medium" => Some(ThinkingEffort::Medium),
				"high" => Some(ThinkingEffort::High),
				"none" => Some(ThinkingEffort::None),
				_ => None,
			});

			let current_step_index = loop_state.step_index + 1;

			// Inner reactive-retry loop: the LLM call may fail with
			// `ContextWindowExceeded` because our local byte-based estimator
			// undershoots the provider's tokenizer. When that happens, we run a
			// single reactive compaction on the current message buffer and try
			// the call again. After at most one retry per turn we surface the
			// failure so the loop does not spin indefinitely.
			let mut reactive_compact_used = false;
			let (accumulated_text, accumulated_tool_calls) = loop {
				// Layer 0 pre-flight microcompaction: unconditionally clear
				// historical tool result content so the prompt only carries the
				// most recent observations. Pure mechanical mutation, no LLM
				// call, no threshold — runs on every attempt (including after
				// reactive compaction) so `pre_call_estimate` reflects the
				// post-microcompact state.
				let microcompact_freed = crate::runtime_loop::microcompact_old_tool_results(
					&mut messages,
					crate::runtime_loop::MICROCOMPACT_RETAIN_RECENT,
					&loop_state.estimator_calibration,
				);
				// Notify the cache break detector that message content changed.
				if microcompact_freed > 0 {
					loop_state.cache_break_detector.notify_compaction();
				}
				// Emit only when the pre-flight pass actually freed tokens. On
				// retry iterations after reactive compaction the buffer is
				// already lean; suppressing zero-freed events keeps trace
				// consumers from attributing a no-op to this attempt.
				if microcompact_freed > 0
					&& let Some(sender) = event_sender
				{
					let _ = sender.send(crate::runtime_loop::LoopEvent::MicrocompactRan {
						step: current_step_index,
						freed_tokens: microcompact_freed,
					});
				}

				// Mid-tier pre-flight (Layer 1 / Layer 2): runs between Layer 0
				// microcompact and the Layer 3 high-water check. Only fires when
				// pressure is above the mid-water threshold and reactive compaction
				// has not already been used this attempt (to avoid double-firing
				// two compaction layers in the same turn).
				if !reactive_compact_used {
					let mid_estimate = crate::runtime_loop::estimate_prompt_pressure(
						&messages,
						Some(&system_prompt),
						&loop_state.estimator_calibration,
					);
					let mid_threshold = (self.agent_runtime_config.r#loop.context_window_tokens
						as f64 * crate::runtime_loop::MID_WATER_TRIGGER_RATIO)
						as u64;
					if mid_estimate > mid_threshold {
						let outcome =
							crate::runtime_loop::mid_compact_messages(&mut messages, None);
						if !matches!(outcome, crate::runtime_loop::MidCompactOutcome::Noop) {
							loop_state.cache_break_detector.notify_compaction();
						}
						if let Some(sender) = event_sender {
							match &outcome {
								crate::runtime_loop::MidCompactOutcome::Layer2 {
									messages_replaced,
								} => {
									let _ = sender.send(
										crate::runtime_loop::LoopEvent::MidCompactLayer2Ran {
											step: current_step_index,
											messages_replaced: *messages_replaced,
										},
									);
								}
								crate::runtime_loop::MidCompactOutcome::Layer1 {
									messages_collapsed,
								} => {
									let _ = sender.send(
										crate::runtime_loop::LoopEvent::MidCompactLayer1Ran {
											step: current_step_index,
											messages_collapsed: *messages_collapsed,
										},
									);
								}
								crate::runtime_loop::MidCompactOutcome::Noop => {}
							}
						}
					}
				}

				// Pre-call: snapshot the calibrated byte-based prompt estimate so
				// we can fold the provider's reported `usage.prompt_tokens` back
				// into the calibration after the call returns. Recomputed each
				// attempt because mid-tier or reactive compaction may have mutated
				// `messages`.
				let pre_call_estimate = crate::runtime_loop::estimate_prompt_tokens_calibrated(
					&messages,
					Some(&system_prompt),
					&loop_state.estimator_calibration,
				);
				let gen_request = GenerationRequest {
					system_prompt: Some(system_prompt.clone()),
					prompt: String::new(),
					messages: Some(messages.clone()),
					expected_output_tokens: config.expected_output_tokens,
					risk_tier: RiskTier::Low,
					preferred_provider: None,
					budget_tokens_remaining: config.budget_tokens_remaining,
					budget_cost_remaining_usd: config.budget_cost_remaining_usd,
					tools: if tool_definitions.is_empty() {
						None
					} else {
						Some(tool_definitions.clone())
					},
					model_override: request.model_override.clone(),
					thinking_effort,
					system_prompt_sections: Some(system_prompt_sections.clone()),
				};

				// Stream the LLM response, accumulating text and tool_calls.
				let attempt_result: Result<(String, Vec<ToolCallBlock>), LlmAdapterError> =
					if let Some(sender) = event_sender {
						let step = current_step_index;
						let (tx, mut rx) = tokio::sync::mpsc::channel::<StreamChunk>(64);
						let event_tx = sender.clone();
						let accumulator = tokio::spawn(async move {
							let mut text = String::new();
							let mut tool_calls: Vec<ToolCallBlock> = Vec::new();
							let mut pending_by_id: HashMap<String, (String, String)> =
								HashMap::new();
							while let Some(chunk) = rx.recv().await {
								match chunk {
									StreamChunk::TextDelta { text: delta } => {
										text.push_str(&delta);
										let _ = event_tx.send(
											crate::runtime_loop::LoopEvent::LlmTextDelta {
												step,
												text: delta,
												agent_id: None,
											},
										);
									}
									StreamChunk::ToolCallStart { id, name } => {
										pending_by_id.insert(id, (name, String::new()));
									}
									StreamChunk::ToolCallDelta {
										id,
										arguments_chunk,
									} => {
										if let Some((_, args)) = pending_by_id.get_mut(&id) {
											args.push_str(&arguments_chunk);
										}
									}
									StreamChunk::ToolCallDone { id } => {
										if let Some((name, args_str)) = pending_by_id.remove(&id) {
											let arguments = serde_json::from_str(&args_str)
												.unwrap_or(Value::Null);
											tool_calls.push(ToolCallBlock {
												id,
												name,
												arguments,
											});
										}
									}
									StreamChunk::Done { .. } => {}
								}
							}
							(text, tool_calls)
						});
						let llm_result = router.generate_streaming(&gen_request, tx).await;
						let (mut text, tool_calls) = accumulator.await.unwrap_or_default();

						match llm_result {
							Ok(resp) => {
								// Fallback: if the provider returned text but no streaming
								// deltas reached the accumulator (e.g. SSE delivered text
								// only in `response.completed`), emit a synthetic delta so
								// the render task can display the response.
								if text.is_empty() && !resp.output.is_empty() {
									text.clone_from(&resp.output);
									let _ =
										sender.send(crate::runtime_loop::LoopEvent::LlmTextDelta {
											step: current_step_index,
											text: resp.output.clone(),
											agent_id: None,
										});
								}
								let _ = sender.send(
									crate::runtime_loop::LoopEvent::LlmDecisionComplete {
										step: current_step_index,
									},
								);
								total_prompt_tokens =
									total_prompt_tokens.saturating_add(resp.prompt_tokens);
								total_output_tokens =
									total_output_tokens.saturating_add(resp.output_tokens);
								total_cache_creation_input_tokens =
									total_cache_creation_input_tokens
										.saturating_add(resp.cache_creation_input_tokens);
								total_cache_read_input_tokens = total_cache_read_input_tokens
									.saturating_add(resp.cache_read_input_tokens);
								last_model_id = Some(resp.model_id.clone());
								// Fold the real `usage.prompt_tokens` back into the
								// estimator calibration so the next turn's pressure
								// check is closer to ground truth.
								loop_state
									.estimator_calibration
									.update(pre_call_estimate.raw_total_tokens, resp.prompt_tokens);
								let _ = sender.send(
									crate::runtime_loop::LoopEvent::EstimatorCalibrated {
										step: current_step_index,
										estimated_prompt_tokens: pre_call_estimate.total_tokens,
										prompt_tokens: resp.prompt_tokens,
										scale: loop_state.estimator_calibration.scale(),
									},
								);
								// Cache break detection: compare this turn's
								// cache_read against the session baseline.
								if let Some(report) =
									loop_state.cache_break_detector.check_response(
										resp.cache_read_input_tokens,
										&resp.model_id,
										&resp.provider,
									) {
									let diag_path =
										crate::runtime_loop::cache_break::write_cache_break_diagnostic(&report)
											.ok()
											.map(|p| p.display().to_string());
									let _ = sender.send(
										crate::runtime_loop::LoopEvent::CacheBreakDetected {
											step: current_step_index,
											reason: report.reason,
											tokens_lost: report.tokens_lost,
											component_changed: report.component_changed,
											diagnostic_path: diag_path,
										},
									);
								}
								Ok((text, tool_calls))
							}
							Err(err) => {
								let _ = sender.send(
									crate::runtime_loop::LoopEvent::LlmDecisionComplete {
										step: current_step_index,
									},
								);
								Err(err)
							}
						}
					} else {
						// Non-streaming path.
						match router.generate(&gen_request).await {
							Ok(resp) => {
								total_prompt_tokens =
									total_prompt_tokens.saturating_add(resp.prompt_tokens);
								total_output_tokens =
									total_output_tokens.saturating_add(resp.output_tokens);
								total_cache_creation_input_tokens =
									total_cache_creation_input_tokens
										.saturating_add(resp.cache_creation_input_tokens);
								total_cache_read_input_tokens = total_cache_read_input_tokens
									.saturating_add(resp.cache_read_input_tokens);
								last_model_id = Some(resp.model_id.clone());
								loop_state
									.estimator_calibration
									.update(pre_call_estimate.raw_total_tokens, resp.prompt_tokens);
								if let Some(sender) = event_sender {
									let _ = sender.send(
										crate::runtime_loop::LoopEvent::EstimatorCalibrated {
											step: current_step_index,
											estimated_prompt_tokens: pre_call_estimate.total_tokens,
											prompt_tokens: resp.prompt_tokens,
											scale: loop_state.estimator_calibration.scale(),
										},
									);
								}
								// Cache break detection (non-streaming path).
								if let Some(report) =
									loop_state.cache_break_detector.check_response(
										resp.cache_read_input_tokens,
										&resp.model_id,
										&resp.provider,
									) {
									let diag_path =
										crate::runtime_loop::cache_break::write_cache_break_diagnostic(&report)
											.ok()
											.map(|p| p.display().to_string());
									if let Some(sender) = event_sender {
										let _ = sender.send(
											crate::runtime_loop::LoopEvent::CacheBreakDetected {
												step: current_step_index,
												reason: report.reason,
												tokens_lost: report.tokens_lost,
												component_changed: report.component_changed,
												diagnostic_path: diag_path,
											},
										);
									}
								}
								let tool_calls = resp.tool_calls.unwrap_or_default();
								Ok((resp.output, tool_calls))
							}
							Err(err) => Err(err),
						}
					};

				match attempt_result {
					Ok(pair) => break pair,
					Err(LlmAdapterError::ContextWindowExceeded { detail, .. })
						if !reactive_compact_used =>
					{
						reactive_compact_used = true;
						loop_state.cache_break_detector.notify_compaction();
						let (compact_pt, compact_ot) = self
							.reactive_compact(
								loop_state,
								&mut messages,
								current_step_index,
								event_sender,
								&detail,
							)
							.await;
						total_prompt_tokens = total_prompt_tokens.saturating_add(compact_pt);
						total_output_tokens = total_output_tokens.saturating_add(compact_ot);
						// Retry the LLM call with the compacted message buffer.
						continue;
					}
					Err(err) => {
						let message = match &err {
							LlmAdapterError::ContextWindowExceeded { detail, .. } => format!(
								"LLM call failed: context window still exceeded after reactive compact ({detail}); goal: {}",
								loop_state.goal
							),
							_ => format!("LLM call failed for goal: {}", loop_state.goal),
						};
						let trace_msg = match &err {
							LlmAdapterError::ContextWindowExceeded { .. } => {
								"LLM call failed: context window still exceeded after reactive compact."
							}
							_ => "LLM call failed.",
						};
						self.record_terminal_step(
							loop_state,
							StepAction::Fail,
							trace_msg,
							Some(message.clone()),
						);
						emit_token_usage(
							event_sender,
							current_step_index,
							total_prompt_tokens,
							total_output_tokens,
							COST_PER_M_INPUT_TOKENS_USD,
							COST_PER_M_OUTPUT_TOKENS_USD,
							last_model_id.as_deref(),
							total_cache_creation_input_tokens,
							total_cache_read_input_tokens,
						);
						return self.synthetic_loop_terminal_result(
							task_id,
							"tool",
							message,
							StepAction::Fail,
							ResultStatus::Error,
							Some(loop_state),
						);
					}
				}
			};

			// Push the assistant turn (text + any tool_calls) to the conversation.
			messages.push(Message::Assistant {
				text: accumulated_text.clone(),
				tool_calls: accumulated_tool_calls.clone(),
			});

			// If the LLM produced no tool_calls, treat the text as the final answer.
			if accumulated_tool_calls.is_empty() {
				let message = if accumulated_text.is_empty() {
					"Runtime loop completed.".to_string()
				} else {
					accumulated_text.clone()
				};
				self.record_terminal_step(
					loop_state,
					StepAction::FinalAnswer,
					"LLM produced a text response with no tool calls.",
					Some(message.clone()),
				);
				emit_token_usage(
					event_sender,
					current_step_index,
					total_prompt_tokens,
					total_output_tokens,
					COST_PER_M_INPUT_TOKENS_USD,
					COST_PER_M_OUTPUT_TOKENS_USD,
					last_model_id.as_deref(),
					total_cache_creation_input_tokens,
					total_cache_read_input_tokens,
				);
				return self.synthetic_loop_terminal_result(
					task_id,
					"tool",
					message,
					StepAction::FinalAnswer,
					ResultStatus::Ok,
					Some(loop_state),
				);
			}

			// Check for special pseudo-tool calls first.
			for tc in &accumulated_tool_calls {
				match tc.name.as_str() {
					"final_answer" => {
						let message = tc
							.arguments
							.get("message")
							.and_then(Value::as_str)
							.unwrap_or("")
							.to_string();
						self.record_terminal_step(
							loop_state,
							StepAction::FinalAnswer,
							"LLM called final_answer.",
							Some(message.clone()),
						);
						emit_token_usage(
							event_sender,
							current_step_index,
							total_prompt_tokens,
							total_output_tokens,
							COST_PER_M_INPUT_TOKENS_USD,
							COST_PER_M_OUTPUT_TOKENS_USD,
							last_model_id.as_deref(),
							total_cache_creation_input_tokens,
							total_cache_read_input_tokens,
						);
						return self.synthetic_loop_terminal_result(
							task_id,
							"tool",
							message,
							StepAction::FinalAnswer,
							ResultStatus::Ok,
							Some(loop_state),
						);
					}
					"ask_user" => {
						let question = tc
							.arguments
							.get("question")
							.and_then(Value::as_str)
							.unwrap_or("")
							.to_string();
						let payload = effective_ask_user_payload(
							&loop_state.goal,
							loop_state.last_observation.as_ref(),
							Some(AskUserPayload::freeform(question)),
						);
						let message = payload.final_message.clone();
						self.record_ask_user_step(loop_state, "LLM called ask_user.", payload);
						emit_token_usage(
							event_sender,
							current_step_index,
							total_prompt_tokens,
							total_output_tokens,
							COST_PER_M_INPUT_TOKENS_USD,
							COST_PER_M_OUTPUT_TOKENS_USD,
							last_model_id.as_deref(),
							total_cache_creation_input_tokens,
							total_cache_read_input_tokens,
						);
						return self.synthetic_loop_terminal_result(
							task_id,
							"tool",
							message,
							StepAction::AskUser,
							ResultStatus::Ok,
							Some(loop_state),
						);
					}
					"fail" => {
						let reason = tc
							.arguments
							.get("reason")
							.and_then(Value::as_str)
							.unwrap_or("LLM reported failure.")
							.to_string();
						self.record_terminal_step(
							loop_state,
							StepAction::Fail,
							"LLM called fail.",
							Some(reason.clone()),
						);
						emit_token_usage(
							event_sender,
							current_step_index,
							total_prompt_tokens,
							total_output_tokens,
							COST_PER_M_INPUT_TOKENS_USD,
							COST_PER_M_OUTPUT_TOKENS_USD,
							last_model_id.as_deref(),
							total_cache_creation_input_tokens,
							total_cache_read_input_tokens,
						);
						return self.synthetic_loop_terminal_result(
							task_id,
							"tool",
							reason,
							StepAction::Fail,
							ResultStatus::Error,
							Some(loop_state),
						);
					}
					_ => {}
				}
			}

			// Execute regular tool calls and collect ToolResult messages.
			let mut turn_tool_ids: Vec<String> = Vec::new();
			for tc in &accumulated_tool_calls {
				let tool_name = &tc.name;
				let arguments = tc.arguments.clone();

				// Check approval gate before dispatching any tool, including Agent.
				// This ensures the approval gate can deny sub-agent spawning just as it
				// can deny any other tool call.
				if let Some(gate) = approval_gate {
					let decision =
						tokio::task::block_in_place(|| gate.check(tool_name, &arguments));
					match decision {
						crate::runtime_loop::approval::ApprovalDecision::Approve => {}
						crate::runtime_loop::approval::ApprovalDecision::Deny(reason) => {
							// Count denied calls against step budget to prevent
							// unbounded retries if the model keeps requesting denied tools.
							loop_state.remaining_step_budget =
								loop_state.remaining_step_budget.saturating_sub(1);
							messages.push(Message::ToolResult {
								tool_use_id: tc.id.clone(),
								content: format!("[Tool denied] {reason}"),
								is_error: true,
							});
							continue;
						}
					}
				}

				// Intercept tool_search pseudo-tool — loads a deferred tool schema
				// for the next turn (non-terminal).
				if tool_name == PSEUDO_TOOL_SEARCH {
					// Deduct step budget so repeated tool_search calls cannot loop
					// without consuming budget.
					loop_state.remaining_step_budget =
						loop_state.remaining_step_budget.saturating_sub(1);

					let tool_name_arg = arguments
						.get("tool_name")
						.and_then(|v| v.as_str())
						.unwrap_or("");

					let result = if let Some(ref mut deferred) = loop_state.deferred_tools {
						if deferred.deferred_names.contains(&tool_name_arg.to_string()) {
							if !deferred.loaded_names.contains(&tool_name_arg.to_string()) {
								deferred.loaded_names.push(tool_name_arg.to_string());
								// Schema must be rebuilt next turn to include the newly loaded tool.
								loop_state.mark_tool_schema_dirty();
							}
							format!(
								"Tool '{}' schema will be available in the next turn.",
								tool_name_arg
							)
						} else {
							format!(
								"Tool '{}' is not in the deferred list. Available deferred tools: {}",
								tool_name_arg,
								deferred.deferred_names.join(", ")
							)
						}
					} else {
						format!(
							"All tools are already loaded. '{}' is available.",
							tool_name_arg
						)
					};

					messages.push(Message::ToolResult {
						tool_use_id: tc.id.clone(),
						content: result,
						is_error: false,
					});
					continue;
				}

				// Intercept task pseudo-tools — non-terminal, return ToolResult and continue.
				if matches!(
					tool_name.as_str(),
					PSEUDO_TASK_CREATE | PSEUDO_TASK_UPDATE | PSEUDO_TASK_LIST | PSEUDO_TASK_GET
				) {
					// Deduct step budget so repeated task-tool calls cannot loop
					// without consuming budget.
					loop_state.remaining_step_budget =
						loop_state.remaining_step_budget.saturating_sub(1);

					let result_content = self.handle_task_pseudo_tool(tool_name, &arguments);
					let is_error = result_content.starts_with("Error:");
					messages.push(Message::ToolResult {
						tool_use_id: tc.id.clone(),
						content: result_content,
						is_error,
					});
					continue;
				}

				// Intercept Agent after the approval gate check.
				// Agent is a pseudo-tool: it spawns a sub-agent and returns the result as a
				// ToolResult. Budget is deducted from the parent before the sub-agent runs so that
				// a failing sub-agent still consumes budget (Class G: skip path resource accounting).
				if tool_name == PSEUDO_AGENT {
					// Deduct one step from parent budget unconditionally (Class G).
					loop_state.remaining_step_budget =
						loop_state.remaining_step_budget.saturating_sub(1);

					// Emit ToolStart for symmetry with other tools (Class C).
					if let Some(sender) = event_sender {
						let _ = sender.send(crate::runtime_loop::LoopEvent::ToolStart {
							step: current_step_index,
							tool_name: tool_name.clone(),
							args_summary: crate::runtime_loop::loop_event::summarize_tool_args(
								tool_name, &arguments,
							),
							agent_id: None,
						});
					}
					let start_ms = std::time::Instant::now();

					let sub_result = execute_sub_agent(
						self,
						task_id,
						request,
						loop_state,
						&arguments,
						runtime_memory_sections,
						event_sender,
						approval_gate,
						&self.sub_agent_config,
					)
					.await;

					let elapsed_ms = start_ms.elapsed().as_millis() as u64;

					// Emit ToolEnd for symmetry with other tools (Class C).
					if let Some(sender) = event_sender {
						let _ = sender.send(crate::runtime_loop::LoopEvent::ToolEnd {
							step: current_step_index,
							tool_name: tool_name.clone(),
							elapsed_ms: Some(elapsed_ms),
							result_summary: None,
							agent_id: None,
						});
					}

					let (sub_content, sub_is_error) = sub_result;
					messages.push(Message::ToolResult {
						tool_use_id: tc.id.clone(),
						content: sub_content,
						is_error: sub_is_error,
					});
					continue;
				}

				// Emit ToolStart.
				if let Some(sender) = event_sender {
					let _ = sender.send(crate::runtime_loop::LoopEvent::ToolStart {
						step: current_step_index,
						tool_name: tool_name.clone(),
						args_summary: crate::runtime_loop::loop_event::summarize_tool_args(
							tool_name, &arguments,
						),
						agent_id: None,
					});
				}

				let step_summary = tool_loop_step_summary(&loop_state.goal, tool_name);
				let attachments =
					attachments_for_tool(tool_name, user_reply.unwrap_or(&loop_state.goal));
				let tool_name_owned = tool_name.clone();
				let execution = tokio::task::block_in_place(|| {
					self.execute_loop_tool_invocation(
						task_id,
						request,
						loop_state,
						&step_summary,
						runtime_memory_sections,
						&tool_name_owned,
						arguments.clone(),
						&attachments,
					)
				});

				let elapsed = execution_elapsed_ms(&execution.result);
				let cap = tool_result_cap(&tool_name_owned);
				// Keep the full (un-truncated) output for disk persistence.
				// The truncated copy is used for the StepRecord and LLM context.
				let full_raw_tool_output = raw_tool_output_from_result(&execution.result);
				let raw_tool_output = truncate_raw_tool_output(full_raw_tool_output.clone(), cap);
				let observation = observation_from_execution(
					&tool_name_owned,
					&execution.result,
					&self.resource_catalog,
				);

				let result_summary = crate::runtime_loop::loop_event::summarize_tool_result(
					&tool_name_owned,
					observation.ok,
					&observation.data,
				);

				// Emit ToolEnd.
				if let Some(sender) = event_sender {
					let _ = sender.send(crate::runtime_loop::LoopEvent::ToolEnd {
						step: current_step_index,
						tool_name: tool_name_owned.clone(),
						elapsed_ms: elapsed,
						result_summary,
						agent_id: None,
					});
				}
				let interpreted = interpret_observation(
					loop_state,
					observation.clone(),
					next_working_directory_from_observation(
						&observation,
						&loop_state.working_directory,
					),
				);

				// Build a synthetic NextStepDecision for the StepRecord.
				let synthetic_decision = crate::runtime_loop::NextStepDecision {
					action: crate::runtime_loop::NextStepAction::CallTool,
					tool_name: Some(tool_name_owned.clone()),
					arguments: Some(arguments.clone()),
					tool_calls: None,
					reason: "LLM tool_use".to_string(),
					final_message: None,
				};
				let step = StepRecord::tool_call(
					current_step_index,
					synthetic_decision,
					loop_state.visible_tools.clone(),
					loop_state.bound_resources.clone(),
					raw_tool_output.clone(),
					StepObservation::Tool(observation.clone()),
					interpreted.clone(),
					elapsed,
					interpreted.remaining_step_budget,
					interpreted.remaining_recovery_budget,
					interpreted
						.new_working_directory
						.clone()
						.unwrap_or_else(|| loop_state.working_directory.clone()),
				);
				loop_state.record_step(step);

				// Build the full (un-truncated) content string for disk persistence.
				let full_tool_result_content = if full_raw_tool_output.is_null() {
					observation.message.clone()
				} else if let Some(s) = full_raw_tool_output.as_str() {
					s.to_string()
				} else {
					serde_json::to_string(&full_raw_tool_output)
						.unwrap_or_else(|_| observation.message.clone())
				};

				// FileRead overflow: return error with max_bytes guidance
				// instead of truncation. Checked BEFORE disk persistence so the
				// raw content length is still available.
				let (tool_result_content, is_error) =
					if tool_name_owned == "Read" && full_tool_result_content.len() > 100_000 {
						let total = full_tool_result_content.len();
						(
							format!(
								"Error: file content is too large ({total} chars) to include in \
							 context. Re-read with a smaller `max_bytes` parameter to retrieve \
							 a manageable portion of the file."
							),
							true,
						)
					} else if tool_name_owned == "Read" {
						// Read delivers full content (TOOL_CAP_NO_TRUNCATE). Skip
						// disk persistence — the user controls size via max_bytes,
						// and preview-replacing a 5KB read would be a regression.
						(full_tool_result_content, !observation.ok)
					} else {
						// Disk persistence: persist full (un-truncated) content and
						// replace with preview (skipped for Read tool).
						let run_id = loop_state.run_id.clone();
						let (content, _is_preview) = loop_state.tool_result_store.register(
							&tc.id,
							&full_tool_result_content,
							&run_id,
						);
						// If register() produced a preview, use it; otherwise
						// truncate the (already-full) content for LLM context.
						let tool_cap = tool_result_cap(&tool_name_owned);
						(
							truncate_tool_result_for_message(&content, tool_cap),
							!observation.ok,
						)
					};
				messages.push(Message::ToolResult {
					tool_use_id: tc.id.clone(),
					content: tool_result_content,
					is_error,
				});
				turn_tool_ids.push(tc.id.clone());

				// If budget is exhausted after this step, inject a warning message.
				if interpreted.budget_exhausted || interpreted.recovery_exhausted {
					messages.push(Message::User {
						content: format!(
							"System: step budget exhausted (remaining: {}, recovery: {}). \
							 Please call final_answer or fail now.",
							interpreted.remaining_step_budget,
							interpreted.remaining_recovery_budget,
						),
					});
				}
			}

			// Advance tool result store state at end of turn.
			loop_state.tool_result_store.advance_turn();

			// Per-turn tool budget check.
			if !turn_tool_ids.is_empty() {
				let per_turn_tool_tokens =
					crate::runtime_loop::estimate_turn_tool_tokens(&messages, &turn_tool_ids);
				let exceeded =
					per_turn_tool_tokens > crate::runtime_loop::PER_TURN_TOOL_BUDGET_TOKENS;
				if let Some(sender) = event_sender {
					let _ = sender.send(crate::runtime_loop::LoopEvent::ToolBudgetCheck {
						step: current_step_index,
						per_turn_tool_tokens,
						exceeded,
					});
				}
				// When budget exceeded, trigger Layer 0 microcompact to free pressure.
				if exceeded {
					let freed = crate::runtime_loop::microcompact_old_tool_results(
						&mut messages,
						crate::runtime_loop::MICROCOMPACT_RETAIN_RECENT,
						&loop_state.estimator_calibration,
					);
					if freed > 0 {
						loop_state.cache_break_detector.notify_compaction();
						if let Some(sender) = event_sender {
							let _ = sender.send(crate::runtime_loop::LoopEvent::MicrocompactRan {
								step: current_step_index,
								freed_tokens: freed,
							});
						}
					}
				}
			}

			let (compact_pt, compact_ot) = self
				.maybe_compact(
					loop_state,
					&mut messages,
					current_step_index,
					event_sender,
					&system_prompt,
				)
				.await;
			if compact_pt > 0 || compact_ot > 0 {
				loop_state.cache_break_detector.notify_compaction();
			}
			total_prompt_tokens = total_prompt_tokens.saturating_add(compact_pt);
			total_output_tokens = total_output_tokens.saturating_add(compact_ot);

			// Emit StepComplete after all tools in this turn are done.
			if let Some(sender) = event_sender {
				let _ = sender.send(crate::runtime_loop::LoopEvent::StepComplete {
					step: current_step_index,
				});
			}
		}
	}

	// execute_sub_agent extracted to crate::sub_agent module.

	/// Handle task pseudo-tool calls (task_create, task_update, task_list, task_get).
	fn handle_task_pseudo_tool(&self, tool_name: &str, arguments: &Value) -> String {
		let mut store = self.task_store.lock().unwrap_or_else(|e| e.into_inner());
		match tool_name {
			PSEUDO_TASK_CREATE => {
				let description = arguments
					.get("description")
					.and_then(Value::as_str)
					.unwrap_or("(no description)")
					.to_string();
				let status = arguments
					.get("status")
					.and_then(Value::as_str)
					.map(TaskStatus::from_str_loose);
				let id = store.create(description, status);
				format!("Task #{id} created successfully.")
			}
			PSEUDO_TASK_UPDATE => {
				let Some(task_id) = parse_u32_from_json(arguments, "task_id") else {
					return "Error: invalid or missing task_id.".to_string();
				};
				let status = arguments
					.get("status")
					.and_then(Value::as_str)
					.map(TaskStatus::from_str_loose);
				let output = arguments
					.get("output")
					.and_then(Value::as_str)
					.map(str::to_string);
				if store.update(task_id, status, output) {
					format!("Task #{task_id} updated.")
				} else {
					format!("Error: task #{task_id} not found.")
				}
			}
			PSEUDO_TASK_LIST => store.format_list(),
			PSEUDO_TASK_GET => {
				let Some(task_id) = parse_u32_from_json(arguments, "task_id") else {
					return "Error: invalid or missing task_id.".to_string();
				};
				match store.get(task_id) {
					Some(task) => TaskStore::format_task(task),
					None => format!("Error: task #{task_id} not found."),
				}
			}
			_ => "Error: unknown task pseudo-tool.".to_string(),
		}
	}

	pub fn register_worker<W>(&mut self, priority: u8, worker: W)
	where
		W: RuntimeWorker + 'static,
	{
		self.workers.push(WorkerRegistryEntry {
			priority,
			worker: Arc::new(worker),
		});
		self.workers
			.sort_by(|left, right| right.priority.cmp(&left.priority));
	}

	fn execute_with_worker(
		&self,
		spec: &AgentInstanceSpec,
		node: &TaskNode,
	) -> Option<ResultEnvelope> {
		self.workers
			.iter()
			.find(|entry| entry.worker.supports(&spec.capabilities))
			.map(|entry| entry.worker.execute(spec, node))
	}

	fn with_execution_router(mut self, execution_router: Arc<LlmRouter>) -> Self {
		self.execution_router = Some(execution_router);
		self
	}

	fn with_route_router(mut self, route_router: Arc<LlmRouter>) -> Self {
		self.route_router = Some(route_router);
		self
	}

	fn visible_tools_for_decision(
		&self,
		route_decision: &crate::router::RouteDecision,
	) -> Vec<String> {
		self.compose_visible_tools(route_decision, None)
	}

	fn visible_tools_for_loop_state(&self, loop_state: &LoopState) -> Vec<String> {
		self.compose_visible_tools(&loop_state.route_decision, Some(loop_state))
	}

	fn refresh_tool_loop_visible_tools(&self, loop_state: &mut LoopState) {
		let currently_plan_mode = self.loop_mode == LoopMode::Plan;
		// Schema-dirty event: plan-mode entered or exited since last refresh.
		// First refresh of a loop state has `observed_plan_mode == None` and
		// starts already-dirty, so we only need to invalidate on actual
		// transitions.
		if loop_state
			.observed_plan_mode
			.is_some_and(|prev| prev != currently_plan_mode)
		{
			loop_state.mark_tool_schema_dirty();
		}
		loop_state.observed_plan_mode = Some(currently_plan_mode);

		let mut visible_tools = self.visible_tools_for_loop_state(loop_state);
		// In Plan mode, filter catalog tools to read-only via the registry,
		// and block mutating pseudo-tools via disallowed_tools (applied in
		// build_tool_definitions).
		if currently_plan_mode {
			visible_tools.retain(|name| {
				self.tool_registry
					.get(name)
					.is_some_and(|entry| entry.is_read_only())
			});
			for blocked in [PSEUDO_TASK_CREATE, PSEUDO_TASK_UPDATE] {
				if !loop_state.disallowed_tools.iter().any(|d| d == blocked) {
					// Schema-dirty event: disallowed_tools gained a new entry.
					// Idempotent on repeat turns — mark dirty only on the
					// turn that actually pushes.
					loop_state.disallowed_tools.push(blocked.to_string());
					loop_state.mark_tool_schema_dirty();
				}
			}
		}
		// Enforce disallowed_tools for catalog tools (sub-agents, plan mode).
		if !loop_state.disallowed_tools.is_empty() {
			visible_tools.retain(|name| !loop_state.disallowed_tools.contains(name));
		}
		// Schema-dirty event: the visible tool set diverged from the previous
		// turn for any reason (route_decision shifted candidates, runtime
		// availability snapshot changed, etc.). Without this guard, a stale
		// frozen schema would be served to the model while `build_tool_definitions`
		// is called with a different visible set — the cache hit would be
		// a silent correctness bug, not a missed optimization.
		if loop_state.visible_tools != visible_tools {
			loop_state.mark_tool_schema_dirty();
		}
		loop_state.visible_tools = visible_tools;
	}

	fn compose_visible_tools(
		&self,
		route_decision: &crate::router::RouteDecision,
		_loop_state: Option<&LoopState>,
	) -> Vec<String> {
		self.runtime_visible_tool_availability_snapshot
			.compose_visible_tools(route_decision.candidate_tools.iter().map(String::as_str))
	}

	fn synthetic_status_result(
		&self,
		task_id: &TaskId,
		node_id: &str,
		producer: &str,
		message: String,
		confidence: f32,
		status: ResultStatus,
	) -> DirectRouteExecutionResult {
		let node = TaskNode {
			node_id: NodeId(node_id.to_string()),
			kind: TaskNodeKind::Execution,
			description: message.clone(),
			resources: Vec::new(),
			capabilities: Vec::new(),
			dispatch_policy: TaskNodeDispatchPolicy::Automatic,
			join_policy: JoinPolicy::AllParents,
			aggregation_mode: AggregationMode::CollectAll,
			recovery_anchor: Default::default(),
			budget_snapshot: NodeBudgetSnapshot {
				token_budget: 0,
				time_budget_ms: 0,
			},
			deadline_ms: 0,
			capability_requirements_snapshot: Vec::new(),
			retry_policy: RetryPolicy::default(),
			rerun_policy: RerunPolicy::SafeToRerun,
		};
		let result = ResultEnvelope {
			task_id: task_id.clone(),
			node_id: node.node_id.clone(),
			producer: producer.to_string(),
			schema_version: "result.v1".to_string(),
			status,
			payload: json!({
				"message": message,
				"direct_route": true,
			})
			.to_string(),
			evidence: vec![EvidenceItem {
				kind: "runtime".to_string(),
				value: "direct-route".to_string(),
			}],
			confidence,
		};
		let message = extract_result_message(&result);
		DirectRouteExecutionResult {
			node,
			result,
			message,
			terminal_step_action: None,
		}
	}

	pub fn execute_approved_tool_invocation(
		&self,
		task: &Task,
		node: &TaskNode,
		tool_name: &str,
		input: Value,
		canonical_execution: CanonicalExecution,
	) -> ResultEnvelope {
		let capabilities = if node.capabilities.is_empty() {
			route_capabilities(&self.resource_catalog, &node.resources)
		} else {
			node.capabilities.clone()
		};
		let spec = AgentInstanceSpec {
			instance_id: format!("approval-resume:{}", node.node_id.0),
			context: AgentContext {
				task_id: task.task_id.clone(),
				node_id: node.node_id.clone(),
				summary: node.description.clone(),
				resources: node.resources.clone(),
				conversation_history: task.conversation_history.clone(),
				runtime_memory_sections: RuntimeMemorySections::default(),
			},
			capabilities: capabilities.clone(),
			capability_tokens: Vec::new(),
			policy_bindings: PolicyBindings {
				budget_tokens: node.budget_snapshot.token_budget.max(1),
				time_budget_ms: node.budget_snapshot.time_budget_ms.max(1),
			},
		};
		let invocation = ToolInvocation {
			tool_name: tool_name.to_string(),
			input: input.clone(),
			canonical_execution: Some(canonical_execution.clone()),
			approved_scope: Some(approved_scope_for_resume(
				&canonical_execution.resource_scope,
			)),
			skip_policy_check: true,
			granted_capabilities: capabilities,
			invocation_key: Some(format!("{}:{}:approved", task.task_id.0, node.node_id.0)),
			attachments: Vec::new(),
		};

		match self.tool_runtime.invoke(invocation) {
			Ok(execution) => {
				tool_success_result(&spec, node, "approval-resume", tool_name, execution, 0.9)
			}
			Err(error) => tool_failure_result(
				&spec,
				node,
				"approval-resume",
				tool_name,
				Some(input),
				Some(canonical_execution),
				error,
			),
		}
	}

	fn execute_tool_invocation_with_resources_and_summary(
		&self,
		task_id: &TaskId,
		request: &RequestEnvelope,
		selector: &ResourceSelector,
		arguments: Value,
		runtime_memory_sections: &RuntimeMemorySections,
		attachments: &[PathBuf],
		bound_resources: &[ResourceSelector],
		step_summary: &str,
	) -> DirectRouteExecutionResult {
		let mut resources = vec![selector.clone()];
		for resource in bound_resources {
			if !resources.iter().any(|existing| existing == resource) {
				resources.push(resource.clone());
			}
		}
		let capabilities = route_capabilities(&self.resource_catalog, &resources);
		let node = TaskNode {
			node_id: NodeId("direct-route".to_string()),
			kind: TaskNodeKind::Execution,
			description: step_description(&request.goal, step_summary),
			resources: resources.clone(),
			capabilities: capabilities.clone(),
			dispatch_policy: TaskNodeDispatchPolicy::Automatic,
			join_policy: JoinPolicy::AllParents,
			aggregation_mode: AggregationMode::CollectAll,
			recovery_anchor: Default::default(),
			budget_snapshot: NodeBudgetSnapshot {
				token_budget: 8_000,
				time_budget_ms: 60_000,
			},
			deadline_ms: 0,
			capability_requirements_snapshot: capabilities.clone(),
			retry_policy: RetryPolicy::default(),
			rerun_policy: RerunPolicy::SafeToRerun,
		};
		let spec = AgentInstanceSpec {
			instance_id: format!("direct-route:{}", node.node_id.0),
			context: AgentContext {
				task_id: task_id.clone(),
				node_id: node.node_id.clone(),
				summary: node.description.clone(),
				resources,
				conversation_history: request.conversation_history.clone(),
				runtime_memory_sections: runtime_memory_sections.clone(),
			},
			capabilities: capabilities.clone(),
			capability_tokens: Vec::new(),
			policy_bindings: PolicyBindings {
				budget_tokens: 8_000,
				time_budget_ms: 60_000,
			},
		};
		let mut input = json!({
			"task_id": task_id.0,
			"node_id": node.node_id.0,
			"goal": request.goal.clone(),
			"summary": node.description.clone(),
			"granted_capabilities": capabilities.clone(),
			"resource_selectors": spec
				.context
				.resources
				.iter()
				.map(|resource| resource.display_key())
				.collect::<Vec<_>>(),
			"conversation_history": render_conversation_history(&request.conversation_history),
			"runtime_memory_sections": runtime_memory_sections,
			"budget_tokens": spec.policy_bindings.budget_tokens,
			"time_budget_ms": spec.policy_bindings.time_budget_ms,
		});
		merge_json_object(&mut input, arguments);
		let canonical_execution =
			canonical_execution_for_builtin_tool_input(selector.name(), &input);
		let invocation = ToolInvocation {
			tool_name: selector.name().to_string(),
			input,
			canonical_execution: canonical_execution.clone(),
			approved_scope: None,
			skip_policy_check: false,
			granted_capabilities: spec.capabilities.clone(),
			invocation_key: Some(format!(
				"{}:{}:{}",
				task_id.0,
				node.node_id.0,
				selector.display_key()
			)),
			attachments: attachments.to_vec(),
		};
		let tool_input = invocation.input.clone();
		match self.tool_runtime.invoke(invocation) {
			Ok(execution) => {
				let result = tool_success_result(
					&spec,
					&node,
					"direct-route",
					selector.name(),
					execution,
					0.9,
				);
				let message = extract_result_message(&result);
				DirectRouteExecutionResult {
					node,
					result,
					message,
					terminal_step_action: None,
				}
			}
			Err(error) => {
				let result = tool_failure_result(
					&spec,
					&node,
					"direct-route",
					selector.name(),
					Some(tool_input),
					canonical_execution,
					error,
				);
				let message = extract_result_message(&result);
				DirectRouteExecutionResult {
					node,
					result,
					message,
					terminal_step_action: None,
				}
			}
		}
	}

	fn execute_loop_tool_invocation(
		&self,
		task_id: &TaskId,
		request: &RequestEnvelope,
		loop_state: &LoopState,
		step_summary: &str,
		runtime_memory_sections: &RuntimeMemorySections,
		tool_name: &str,
		arguments: Value,
		attachments: &[PathBuf],
	) -> DirectRouteExecutionResult {
		let Some(selector) = tool_selector_by_name(&self.resource_catalog, tool_name) else {
			return self.synthetic_status_result(
				task_id,
				"runtime-loop:tool",
				"runtime-loop",
				format!("tool `{tool_name}` is not enabled in the current runtime inventory"),
				0.0,
				ResultStatus::Error,
			);
		};
		self.execute_tool_invocation_with_resources_and_summary(
			task_id,
			request,
			&selector,
			arguments,
			runtime_memory_sections,
			attachments,
			&loop_state.bound_resources,
			step_summary,
		)
	}

	fn synthetic_loop_terminal_result(
		&self,
		task_id: &TaskId,
		loop_name: &str,
		message: String,
		terminal_step_action: StepAction,
		status: ResultStatus,
		loop_state: Option<&LoopState>,
	) -> DirectRouteExecutionResult {
		let node_id = format!("runtime-loop:{loop_name}");
		let node = TaskNode {
			node_id: NodeId(node_id),
			kind: TaskNodeKind::Execution,
			description: message.clone(),
			resources: Vec::new(),
			capabilities: Vec::new(),
			dispatch_policy: TaskNodeDispatchPolicy::Automatic,
			join_policy: JoinPolicy::AllParents,
			aggregation_mode: AggregationMode::CollectAll,
			recovery_anchor: Default::default(),
			budget_snapshot: NodeBudgetSnapshot {
				token_budget: 0,
				time_budget_ms: 0,
			},
			deadline_ms: 0,
			capability_requirements_snapshot: Vec::new(),
			retry_policy: RetryPolicy::default(),
			rerun_policy: RerunPolicy::SafeToRerun,
		};
		let result = ResultEnvelope {
			task_id: task_id.clone(),
			node_id: node.node_id.clone(),
			producer: "runtime-loop".to_string(),
			schema_version: "result.v1".to_string(),
			status,
			payload: json!({
				"message": message,
				"direct_route": true,
				"runtime_loop": loop_name,
				"probe_trace": loop_state.map(loop_probe_trace_payload),
			})
			.to_string(),
			evidence: vec![EvidenceItem {
				kind: "runtime".to_string(),
				value: "runtime-loop".to_string(),
			}],
			confidence: if status == ResultStatus::Ok {
				0.88
			} else {
				0.0
			},
		};
		let message = extract_result_message(&result);
		DirectRouteExecutionResult {
			node,
			result,
			message,
			terminal_step_action: Some(terminal_step_action),
		}
	}
}

impl AgentWorker for GenericAgentRuntime {
	fn execute(&self, spec: &AgentInstanceSpec, node: &TaskNode) -> ResultEnvelope {
		if spec.policy_bindings.budget_tokens == 0 || spec.policy_bindings.time_budget_ms == 0 {
			return policy_rejection_result(spec, node);
		}

		if let Some(result) = self.execute_with_worker(spec, node) {
			return result;
		}

		// No matching worker found — produce a synthetic fallback result.
		// Previously this used generic_worker (general.execute) as a catch-all,
		// but that meta-tool has been removed.
		let goal = node
			.description
			.strip_prefix("Goal: ")
			.and_then(|rest| rest.split_once("\nStep: ").map(|(g, _)| g.to_string()))
			.unwrap_or_else(|| node.description.clone());
		let payload = serde_json::json!({ "message": goal });
		ResultEnvelope {
			task_id: spec.context.task_id.clone(),
			node_id: node.node_id.clone(),
			producer: spec.instance_id.clone(),
			schema_version: "result.v1".to_string(),
			status: ResultStatus::Ok,
			payload: serde_json::to_string(&payload).unwrap_or_default(),
			evidence: vec![EvidenceItem {
				kind: "worker".to_string(),
				value: "worker-fallback:no-matching-worker".to_string(),
			}],
			confidence: 0.50,
		}
	}
}

impl Default for GenericAgentRuntime {
	fn default() -> Self {
		Self::with_skill_registry(SkillRegistry::disabled())
	}
}

/// Emit a `LoopEvent::TokenUsage` event if a sender is present.
///
/// Computes estimated cost using the supplied per-million-token rates.
/// `cache_creation_input_tokens` and `cache_read_input_tokens` are
/// additive per-tier counters sourced from the provider's `usage` block;
/// they are `0` when the provider did not report any cache activity (which
/// is the current default until 09 / 10 land).
#[allow(clippy::too_many_arguments)]
fn emit_token_usage(
	event_sender: Option<&crate::runtime_loop::LoopEventSender>,
	step: u32,
	prompt_tokens: u64,
	output_tokens: u64,
	cost_per_m_input_usd: f64,
	cost_per_m_output_usd: f64,
	model_id: Option<&str>,
	cache_creation_input_tokens: u64,
	cache_read_input_tokens: u64,
) {
	if let Some(sender) = event_sender {
		let total_tokens = prompt_tokens.saturating_add(output_tokens);
		let estimated_cost_usd = (prompt_tokens as f64 / 1_000_000.0) * cost_per_m_input_usd
			+ (output_tokens as f64 / 1_000_000.0) * cost_per_m_output_usd;
		let _ = sender.send(crate::runtime_loop::LoopEvent::TokenUsage {
			step,
			prompt_tokens,
			output_tokens,
			total_tokens,
			estimated_cost_usd,
			model_id: model_id.map(str::to_string),
			cache_creation_input_tokens,
			cache_read_input_tokens,
		});
	}
}

fn approved_scope_for_resume(
	scope: &roku_common_types::ExecutionResourceScope,
) -> roku_common_types::ExecutionResourceScope {
	let mut approved = scope.clone();
	for target in &scope.resolved_targets {
		if !approved
			.effective_read_roots
			.iter()
			.any(|existing| existing == target)
		{
			approved.effective_read_roots.push(target.clone());
		}
	}
	approved
}

fn step_description(goal: &str, step: &str) -> String {
	format!("Goal: {goal}\nStep: {step}")
}

fn merge_json_object(target: &mut Value, overlay: Value) {
	let Some(target_object) = target.as_object_mut() else {
		return;
	};
	let Some(overlay_object) = overlay.as_object() else {
		return;
	};
	for (key, value) in overlay_object {
		target_object.insert(key.clone(), value.clone());
	}
}

fn render_conversation_history(history: &[ConversationTurn]) -> String {
	history
		.iter()
		.map(|turn| format!("{}: {}", role_label(turn.role), turn.content))
		.collect::<Vec<_>>()
		.join("\n")
}

fn role_label(role: ConversationRole) -> &'static str {
	match role {
		ConversationRole::User => "user",
		ConversationRole::Assistant => "assistant",
		ConversationRole::System => "system",
	}
}

fn route_capabilities(catalog: &ResourceCatalog, resources: &[ResourceSelector]) -> Vec<String> {
	let mut capabilities = Vec::new();
	for resource in resources {
		if let Some(descriptor) = catalog.descriptor(resource) {
			for capability in &descriptor.required_capabilities {
				if !capabilities.iter().any(|existing| existing == capability) {
					capabilities.push(capability.clone());
				}
			}
		}
	}
	capabilities
}

// tool_selector_by_name, execution_elapsed_ms, raw_tool_output_from_result
// extracted to crate::tools::dispatch module.

fn loop_probe_trace_payload(loop_state: &LoopState) -> Value {
	serde_json::to_value(runtime_loop_trace(loop_state))
		.unwrap_or_else(|_| json!({ "schema_version": "runtime_loop_trace.v1" }))
}

/// Parse a `u32` from a JSON value that may be a string or a number.
/// Returns `None` for missing keys, non-numeric strings, or values exceeding `u32::MAX`.
fn parse_u32_from_json(args: &Value, key: &str) -> Option<u32> {
	let v = args.get(key)?;
	if let Some(s) = v.as_str() {
		s.parse::<u32>().ok()
	} else {
		v.as_u64().and_then(|n| u32::try_from(n).ok())
	}
}

fn terminal_decision(
	action: StepAction,
	reason: &str,
	observation: Option<&StepObservation>,
) -> crate::runtime_loop::NextStepDecision {
	let final_message = observation.map(|step_observation| match step_observation {
		StepObservation::AskUser { final_message }
		| StepObservation::FinalMessage { final_message } => final_message.clone(),
		StepObservation::Tool(observation) => observation.message.clone(),
	});
	crate::runtime_loop::NextStepDecision {
		action: match action {
			StepAction::CallTool | StepAction::CompactBoundary => {
				crate::runtime_loop::NextStepAction::CallTool
			}
			StepAction::AskUser => crate::runtime_loop::NextStepAction::AskUser,
			StepAction::FinalAnswer => crate::runtime_loop::NextStepAction::FinalAnswer,
			StepAction::Fail | StepAction::Stop => crate::runtime_loop::NextStepAction::Fail,
		},
		tool_name: None,
		arguments: None,
		tool_calls: None,
		reason: reason.to_string(),
		final_message,
	}
}

// normalize_tool_loop_observation, tool_result_cap, truncate_tool_result_for_message,
// truncate_raw_tool_output extracted to crate::tools::dispatch module.

fn awaiting_user_resume_prompt(
	loop_state: &LoopState,
	final_message: &str,
	missing_fields: &[String],
	user_input: &str,
) -> String {
	format!(
		r#"Decide whether the latest user message should resume an existing paused runtime loop or start a fresh request.

Return only JSON with this shape:
{{
  "resume_existing_loop": true,
  "reason": "short explanation"
}}

Rules:
- Resume only if the latest user message is best interpreted as answering the current paused clarification for the same task.
- Do not resume if the latest user message appears to start a new task, switch topics, or issue a fresh standalone request.
- When unsure, prefer `resume_existing_loop=false`.
- The paused loop still needs these required fields: {missing_fields}.

Paused ask-user message:
{final_message}

Latest user reply:
{user_input}

Paused loop goal:
{goal}"#,
		missing_fields = if missing_fields.is_empty() {
			"(none)".to_string()
		} else {
			missing_fields.join(", ")
		},
		final_message = final_message,
		user_input = user_input,
		goal = loop_state.goal,
	)
}

fn safe_baseline_tool_pool(config: &crate::runtime_config::LoopRuntimeConfig) -> Vec<&str> {
	config
		.baseline_tool_pool
		.iter()
		.map(String::as_str)
		.collect()
}

fn tool_loop_step_summary(goal: &str, tool_name: &str) -> String {
	format!("Executing tool `{tool_name}` for goal: {goal}")
}

fn extract_result_message(result: &ResultEnvelope) -> String {
	serde_json::from_str::<serde_json::Value>(&result.payload)
		.ok()
		.and_then(|payload| {
			payload
				.get("message")
				.and_then(serde_json::Value::as_str)
				.map(str::to_string)
		})
		.unwrap_or_else(|| result.payload.clone())
}

#[cfg(test)]
mod tests {
	use crate::IntentFamily;
	use async_trait::async_trait;
	use roku_common_types::{
		AgentContext, AggregationMode, EvidenceItem, JoinPolicy, NodeId, PolicyBindings,
		ResultStatus, TaskId, TaskNode, TaskNodeKind,
	};
	use roku_plugin_llm::{
		GenerationRequest, LlmProvider, LlmRouter, ModelProfile, ProviderCallError,
		ProviderResponse, RiskTier, RoutingPolicy,
	};
	use roku_plugin_skills::{
		DownloadedArchive, SkillArchiveFetcher, SkillRegistry, SkillRegistryError, SkillSource,
	};
	use std::collections::VecDeque;
	use std::env;
	use std::fs;
	use std::io::{Cursor, Write};
	use std::sync::{Arc, Mutex};
	use tempfile::Builder;

	use super::*;

	fn node_with_capability(capability: &str) -> TaskNode {
		TaskNode {
			node_id: NodeId("node-1".to_string()),
			kind: TaskNodeKind::Execution,
			description: "runtime test node".to_string(),
			capabilities: vec![capability.to_string()],
			join_policy: JoinPolicy::default(),
			aggregation_mode: AggregationMode::default(),
			..TaskNode::default()
		}
	}

	fn spec_with_capabilities(capabilities: Vec<&str>) -> AgentInstanceSpec {
		AgentInstanceSpec {
			instance_id: "agent-1".to_string(),
			context: AgentContext {
				task_id: TaskId("task-1".to_string()),
				node_id: NodeId("node-1".to_string()),
				summary: "summary".to_string(),
				resources: Vec::new(),
				conversation_history: Vec::new(),
				runtime_memory_sections: RuntimeMemorySections::default(),
			},
			capabilities: capabilities
				.into_iter()
				.map(std::string::ToString::to_string)
				.collect(),
			capability_tokens: Vec::new(),
			policy_bindings: PolicyBindings {
				budget_tokens: 10_000,
				time_budget_ms: 30_000,
			},
		}
	}

	fn payload_value(result: &ResultEnvelope) -> serde_json::Value {
		serde_json::from_str(&result.payload).expect("payload should be valid json")
	}

	fn awaiting_user_loop_state(goal: &str, payload: AskUserPayload) -> LoopState {
		let context = LoopContext {
			request_id: "req-awaiting-user".to_string(),
			session_id: "session-awaiting-user".to_string(),
			goal: goal.to_string(),
			workspace_root: "/workspace".to_string(),
			working_directory: "/workspace".to_string(),
			visible_tools: vec![
				"inventory.describe".to_string(),
				"Find".to_string(),
				"WebSearch".to_string(),
			],
			bound_resources: vec![ResourceSelector::tool("inventory.describe".to_string())],
			route_decision: crate::router::RouteDecision::new(
				IntentFamily::Chat,
				0.82,
				false,
				crate::router::RouteRisk::Low,
				vec!["inventory.describe".to_string()],
				Vec::new(),
				Vec::new(),
				"paused runtime loop test",
			),
			last_observation: None,
		};
		let mut loop_state = LoopState::new("loop-awaiting-user", &context);
		loop_state.status = LoopStatus::AwaitingUser;
		loop_state.awaiting_user = Some(payload);
		loop_state
	}

	#[tokio::test]
	async fn freeform_ask_user_resume_contract_requires_fresh_intake() {
		let runtime = GenericAgentRuntime::with_skill_registry(SkillRegistry::disabled());
		let loop_state = awaiting_user_loop_state(
			"继续之前的任务",
			AskUserPayload::freeform("您想继续什么任务？"),
		);

		assert_eq!(
			loop_state
				.awaiting_user
				.as_ref()
				.expect("awaiting-user payload should exist")
				.resume_contract,
			AskUserResumeContract::NoAutomaticResume
		);

		let assessment = runtime
			.assess_awaiting_user_resume(&loop_state, "项目里有几行代码？")
			.await;

		assert_eq!(
			assessment,
			AwaitingUserResumeAssessment {
				should_resume: false,
				reason:
					"freeform clarification pauses do not auto-resume; treat the next message as a fresh intake"
						.to_string(),
			}
		);
	}

	#[tokio::test]
	async fn candidate_selection_ask_user_resume_contract_requires_grounded_choice() {
		let runtime = GenericAgentRuntime::with_skill_registry(SkillRegistry::disabled());
		let candidates = vec![
			"/Users/jojo/cjj_project/Roku/Cargo.toml".to_string(),
			"/Users/jojo/cjj_project/Roku/crates/roku-agent-runtime/Cargo.toml".to_string(),
		];
		let loop_state = awaiting_user_loop_state(
			"看一下 Cargo.toml",
			AskUserPayload::candidate_selection(
				"我找到了多个候选路径。你想看哪一个？",
				candidates.clone(),
				None,
			),
		);

		assert_eq!(
			loop_state
				.awaiting_user
				.as_ref()
				.expect("awaiting-user payload should exist")
				.resume_contract,
			AskUserResumeContract::CandidateSelection { candidates }
		);

		let assessment = runtime
			.assess_awaiting_user_resume(
				&loop_state,
				"/Users/jojo/cjj_project/Roku/crates/roku-agent-runtime/Cargo.toml",
			)
			.await;

		assert_eq!(
			assessment,
			AwaitingUserResumeAssessment {
				should_resume: true,
				reason: "user reply selected one of the grounded candidates for the paused loop"
					.to_string(),
			}
		);
	}

	#[test]
	fn missing_required_input_ask_user_resume_contract_uses_router_decision() {
		let (router, prompts) = router_with_json_responses(vec![serde_json::json!({
			"resume_existing_loop": true,
			"reason": "The reply supplies the missing project_path for the paused request."
		})]);
		let runtime = GenericAgentRuntime::with_llm_router(router);
		let loop_state = awaiting_user_loop_state(
			"统计项目代码行数",
			AskUserPayload::missing_required_input(
				"请提供 project_path。",
				vec!["project_path".to_string()],
			),
		);

		assert_eq!(
			loop_state
				.awaiting_user
				.as_ref()
				.expect("awaiting-user payload should exist")
				.resume_contract,
			AskUserResumeContract::MissingRequiredInput {
				fields: vec!["project_path".to_string()]
			}
		);

		// assess_awaiting_user_resume is async; bridge via block_on so that the LlmRouter
		// (which holds a blocking_runtime) is dropped in sync scope rather than async scope.
		let assessment = tokio::runtime::Builder::new_current_thread()
			.enable_all()
			.build()
			.expect("tokio runtime for assess-awaiting bridge should build")
			.block_on(
				runtime.assess_awaiting_user_resume(&loop_state, "/Users/jojo/cjj_project/Roku"),
			);

		assert_eq!(
			assessment,
			AwaitingUserResumeAssessment {
				should_resume: true,
				reason: "The reply supplies the missing project_path for the paused request."
					.to_string(),
			}
		);
		let prompts = prompts.lock().expect("prompt lock should succeed");
		assert_eq!(prompts.len(), 1);
		assert!(prompts[0].contains("project_path"));
		assert!(prompts[0].contains("请提供 project_path"));
		assert!(prompts[0].contains("/Users/jojo/cjj_project/Roku"));
	}

	fn regression_fixture_path(suffix: &str, contents: &str) -> String {
		let root = env::current_dir()
			.expect("cwd should resolve for regression fixtures")
			.join("tmp");
		fs::create_dir_all(&root).expect("tmp fixture directory should exist");
		let mut fixture = Builder::new()
			.suffix(suffix)
			.tempfile_in(&root)
			.expect("fixture file should be created");
		fixture
			.write_all(contents.as_bytes())
			.expect("fixture contents should be written");
		fixture
			.into_temp_path()
			.keep()
			.expect("fixture path should persist")
			.display()
			.to_string()
	}

	fn cleanup_fixture(path: &str) {
		let _ = fs::remove_file(path);
	}

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
	fn dispatches_to_skill_worker_and_returns_install_message() {
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
		let runtime = GenericAgentRuntime::with_skill_registry(registry);
		let node = node_with_capability("SkillInstall");
		let mut spec = spec_with_capabilities(vec!["SkillInstall"]);
		spec.context.summary =
			"Goal: install skill\nStep: Ensure requested skill from source URL is installed"
				.to_string();
		spec.context.conversation_history = Vec::new();
		let node = TaskNode {
			description: "Goal: install the claude api skill from https://github.com/anthropics/skills/tree/main/skills/claude-api\nStep: Ensure requested skill from source URL is installed".to_string(),
			..node
		};

		let result = runtime.execute(&spec, &node);
		assert_eq!(result.status, ResultStatus::Ok);
		assert_eq!(result.evidence[0].value, "skill-worker");
		assert_eq!(result.evidence[1].value, "SkillInstall");
		assert!(
			payload_value(&result)["message"]
				.as_str()
				.expect("message should be a string")
				.contains("Installed skill `claude-api`")
		);
	}

	#[test]
	fn rejects_execution_when_policy_budget_is_zero() {
		let runtime = GenericAgentRuntime::default();
		let node = node_with_capability("data.read");
		let mut spec = spec_with_capabilities(vec!["data.read"]);
		spec.policy_bindings.budget_tokens = 0;

		let result = runtime.execute(&spec, &node);
		assert_eq!(result.status, ResultStatus::Error);
		assert_eq!(result.evidence[0].value, "budget-exhausted");
		assert_eq!(
			payload_value(&result)["error_code"],
			"policy_bindings_rejected"
		);
	}

	struct CustomWorker;

	impl RuntimeWorker for CustomWorker {
		fn worker_id(&self) -> &'static str {
			"custom-worker"
		}

		fn supports(&self, capabilities: &[String]) -> bool {
			capabilities
				.iter()
				.any(|capability| capability.starts_with("quant."))
		}

		fn execute(&self, spec: &AgentInstanceSpec, node: &TaskNode) -> ResultEnvelope {
			ResultEnvelope {
				task_id: spec.context.task_id.clone(),
				node_id: node.node_id.clone(),
				producer: spec.instance_id.clone(),
				schema_version: "result.v1".to_string(),
				status: ResultStatus::Ok,
				payload:
					r#"{"worker_id":"custom-worker","message":"custom quant worker executed"}"#
						.to_string(),
				evidence: vec![EvidenceItem {
					kind: "runtime".to_string(),
					value: "custom-worker".to_string(),
				}],
				confidence: 0.95,
			}
		}
	}

	#[test]
	fn allows_runtime_worker_extension() {
		let mut runtime = GenericAgentRuntime::default();
		runtime.register_worker(100, CustomWorker);
		let node = node_with_capability("quant.backtest");
		let spec = spec_with_capabilities(vec!["quant.backtest"]);

		let result = runtime.execute(&spec, &node);
		assert_eq!(result.status, ResultStatus::Ok);
		assert_eq!(result.evidence[0].value, "custom-worker");
		assert_eq!(payload_value(&result)["worker_id"], "custom-worker");
	}

	struct SequenceJsonProvider {
		prompts: Arc<Mutex<Vec<String>>>,
		responses: Arc<Mutex<VecDeque<String>>>,
	}

	/// Convert a legacy JSON-in-text test fixture into a `tool_calls` list so that
	/// tests remain compatible with the tool_use-only dispatch path.
	fn json_fixture_to_tool_calls(output: &str) -> Option<Vec<roku_plugin_llm::ToolCallBlock>> {
		let v = serde_json::from_str::<serde_json::Value>(output).ok()?;
		let action = v
			.get("action")
			.and_then(serde_json::Value::as_str)
			.unwrap_or("");
		let tool_name = v.get("tool_name").and_then(serde_json::Value::as_str);
		match (action, tool_name) {
			("call_tool", Some(name)) => {
				let arguments = v
					.get("arguments")
					.cloned()
					.filter(|a| !a.is_null())
					.unwrap_or(serde_json::json!({}));
				Some(vec![roku_plugin_llm::ToolCallBlock {
					id: "test-call-1".to_string(),
					name: name.to_string(),
					arguments,
				}])
			}
			("final_answer", _) => {
				let message = v
					.get("final_message")
					.and_then(serde_json::Value::as_str)
					.unwrap_or("")
					.to_string();
				Some(vec![roku_plugin_llm::ToolCallBlock {
					id: "test-call-1".to_string(),
					name: "final_answer".to_string(),
					arguments: serde_json::json!({ "message": message }),
				}])
			}
			("ask_user", _) => {
				let question = v
					.get("final_message")
					.and_then(serde_json::Value::as_str)
					.unwrap_or("")
					.to_string();
				Some(vec![roku_plugin_llm::ToolCallBlock {
					id: "test-call-1".to_string(),
					name: "ask_user".to_string(),
					arguments: serde_json::json!({ "question": question }),
				}])
			}
			("fail", _) => {
				let reason = v
					.get("final_message")
					.and_then(serde_json::Value::as_str)
					.or_else(|| v.get("reason").and_then(serde_json::Value::as_str))
					.unwrap_or("")
					.to_string();
				Some(vec![roku_plugin_llm::ToolCallBlock {
					id: "test-call-1".to_string(),
					name: "fail".to_string(),
					arguments: serde_json::json!({ "reason": reason }),
				}])
			}
			_ => None,
		}
	}

	#[async_trait]
	impl LlmProvider for SequenceJsonProvider {
		fn provider_name(&self) -> &'static str {
			"sequence-json-provider"
		}

		async fn complete(
			&self,
			_model: &ModelProfile,
			request: &GenerationRequest,
		) -> Result<ProviderResponse, ProviderCallError> {
			// Capture a serialized snapshot of the request that includes both the
			// legacy prompt field and the new messages/tools fields, so tests can
			// assert on either representation.
			let captured = serde_json::json!({
				"prompt": request.prompt,
				"messages": request.messages,
				"tool_names": request.tools.as_deref().map(|tools| {
					tools.iter().map(|t| &t.name).collect::<Vec<_>>()
				}),
			})
			.to_string();
			self.prompts
				.lock()
				.expect("prompt lock should succeed")
				.push(captured);
			let output = self
				.responses
				.lock()
				.expect("response lock should succeed")
				.pop_front()
				.expect("a canned response should be available");
			let tool_calls = json_fixture_to_tool_calls(&output);
			Ok(ProviderResponse {
				output,
				finish_reason: None,
				prompt_tokens: 24,
				output_tokens: 18,
				cache_creation_input_tokens: 0,
				cache_read_input_tokens: 0,
				latency_ms: 10,
				tool_calls,
			})
		}
	}

	fn router_with_json_responses(
		responses: Vec<serde_json::Value>,
	) -> (LlmRouter, Arc<Mutex<Vec<String>>>) {
		let prompts = Arc::new(Mutex::new(Vec::new()));
		let mut router = LlmRouter::new(RoutingPolicy {
			max_request_cost_usd: 1.0,
			max_latency_ms: 5_000,
		});
		router.register_provider(SequenceJsonProvider {
			prompts: Arc::clone(&prompts),
			responses: Arc::new(Mutex::new(
				responses
					.into_iter()
					.map(|value| value.to_string())
					.collect::<VecDeque<_>>(),
			)),
		});
		router.register_model(ModelProfile {
			model_id: "sequence-json-model".to_string(),
			provider: "sequence-json-provider".to_string(),
			max_context_tokens: 16_000,
			cost_per_1k_tokens_usd: 0.0,
			max_risk_tier: RiskTier::Low,
			route_priority: 100,
		});
		(router, prompts)
	}

	struct StaticTextProvider {
		name: &'static str,
		output: &'static str,
	}

	#[async_trait]
	impl LlmProvider for StaticTextProvider {
		fn provider_name(&self) -> &'static str {
			self.name
		}

		async fn complete(
			&self,
			_model: &ModelProfile,
			_request: &GenerationRequest,
		) -> Result<ProviderResponse, ProviderCallError> {
			Ok(ProviderResponse {
				output: self.output.to_string(),
				finish_reason: None,
				prompt_tokens: 20,
				output_tokens: 16,
				cache_creation_input_tokens: 0,
				cache_read_input_tokens: 0,
				latency_ms: 10,
				tool_calls: None,
			})
		}
	}

	fn router_with_text_output(name: &'static str, output: &'static str) -> LlmRouter {
		let mut router = LlmRouter::new(RoutingPolicy {
			max_request_cost_usd: 1.0,
			max_latency_ms: 5_000,
		});
		router.register_provider(StaticTextProvider { name, output });
		router.register_model(ModelProfile {
			model_id: format!("{name}-model"),
			provider: name.to_string(),
			max_context_tokens: 16_000,
			cost_per_1k_tokens_usd: 0.0,
			max_risk_tier: RiskTier::Medium,
			route_priority: 100,
		});
		router
	}

	/// Test provider that returns a sequence of mixed Result entries — useful for
	/// simulating reactive-compact recovery where the first call fails with
	/// `ContextWindowExceeded` and a later call succeeds.
	struct ReactiveCompactProvider {
		invocations: Arc<std::sync::atomic::AtomicUsize>,
		responses: Arc<Mutex<VecDeque<Result<String, ProviderCallError>>>>,
	}

	#[async_trait]
	impl LlmProvider for ReactiveCompactProvider {
		fn provider_name(&self) -> &'static str {
			"reactive-compact-provider"
		}

		async fn complete(
			&self,
			_model: &ModelProfile,
			_request: &GenerationRequest,
		) -> Result<ProviderResponse, ProviderCallError> {
			self.invocations
				.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
			let next = self
				.responses
				.lock()
				.expect("response lock should succeed")
				.pop_front()
				.expect("a canned reactive-compact response should be available");
			match next {
				Ok(output) => {
					let tool_calls = json_fixture_to_tool_calls(&output);
					Ok(ProviderResponse {
						output,
						finish_reason: None,
						prompt_tokens: 24,
						output_tokens: 18,
						cache_creation_input_tokens: 0,
						cache_read_input_tokens: 0,
						latency_ms: 10,
						tool_calls,
					})
				}
				Err(err) => Err(err),
			}
		}

		// Override the default stream() so that tool_call chunks reach the
		// runtime accumulator (the default implementation only forwards
		// TextDelta + Done, which would drop the tool_calls field on the
		// response).
		async fn stream(
			&self,
			model: &ModelProfile,
			request: &GenerationRequest,
			tx: tokio::sync::mpsc::Sender<roku_plugin_llm::StreamChunk>,
		) -> Result<ProviderResponse, ProviderCallError> {
			let response = self.complete(model, request).await?;
			if !response.output.is_empty() {
				let _ = tx
					.send(roku_plugin_llm::StreamChunk::TextDelta {
						text: response.output.clone(),
					})
					.await;
			}
			if let Some(tool_calls) = response.tool_calls.as_ref() {
				for tc in tool_calls {
					let _ = tx
						.send(roku_plugin_llm::StreamChunk::ToolCallStart {
							id: tc.id.clone(),
							name: tc.name.clone(),
						})
						.await;
					let _ = tx
						.send(roku_plugin_llm::StreamChunk::ToolCallDelta {
							id: tc.id.clone(),
							arguments_chunk: tc.arguments.to_string(),
						})
						.await;
					let _ = tx
						.send(roku_plugin_llm::StreamChunk::ToolCallDone { id: tc.id.clone() })
						.await;
				}
			}
			let _ = tx
				.send(roku_plugin_llm::StreamChunk::Done {
					finish_reason: response.finish_reason.clone(),
					prompt_tokens: response.prompt_tokens,
					output_tokens: response.output_tokens,
				})
				.await;
			Ok(response)
		}
	}

	fn router_with_reactive_compact_provider(
		responses: Vec<Result<String, ProviderCallError>>,
	) -> (LlmRouter, Arc<std::sync::atomic::AtomicUsize>) {
		let invocations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
		let mut router = LlmRouter::new(RoutingPolicy {
			max_request_cost_usd: 1.0,
			max_latency_ms: 5_000,
		});
		router.register_provider(ReactiveCompactProvider {
			invocations: Arc::clone(&invocations),
			responses: Arc::new(Mutex::new(responses.into())),
		});
		router.register_model(ModelProfile {
			model_id: "reactive-compact-model".to_string(),
			provider: "reactive-compact-provider".to_string(),
			max_context_tokens: 16_000,
			cost_per_1k_tokens_usd: 0.0,
			max_risk_tier: RiskTier::Low,
			route_priority: 100,
		});
		(router, invocations)
	}

	#[test]
	fn execute_tool_loop_can_continue_after_non_terminal_success() {
		let cwd = env::current_dir()
			.expect("cwd should resolve for tests")
			.display()
			.to_string();
		let (route_router, prompts) = router_with_json_responses(vec![
			serde_json::json!({
				"action": "call_tool",
				"tool_name": "Inspect",
				"arguments": { "path": cwd },
				"reason": "Inspect the grounded workspace path before answering.",
				"final_message": null
			}),
			serde_json::json!({
				"action": "final_answer",
				"tool_name": null,
				"arguments": null,
				"reason": "The workspace inspection is sufficient now.",
				"final_message": "Loop concluded after a non-terminal filesystem observation."
			}),
		]);
		let execution_router =
			router_with_text_output("tool-execution-provider", "live inventory response");
		let root = tempfile::tempdir().expect("temp root should exist");
		let runtime =
			GenericAgentRuntime::with_route_and_execution_routers_skill_registry_tool_config_and_plugin_snapshot(
				route_router,
				execution_router,
				SkillRegistry::file_backed(root.keep()),
				ToolCatalogConfig::default(),
				PluginRegistrySnapshot::permissive(),
				ToolsRuntimeConfig::default(),
			);
		let request = RequestEnvelope {
			request_id: roku_common_types::RequestId("req-loop".to_string()),
			session_id: "session-loop".to_string(),
			goal: "Inspect the current workspace directory".to_string(),
			planning_mode_hint: None,
			conversation_history: Vec::new(),
			model_override: None,
			thinking_effort: None,
		};
		let decision = crate::router::RouteDecision::new(
			IntentFamily::FilesystemRead,
			0.95,
			false,
			crate::router::RouteRisk::Low,
			vec!["Inspect".to_string()],
			Vec::new(),
			Vec::new(),
			"filesystem request",
		);
		let mut loop_state =
			runtime.initialize_runtime_loop(&request, &request.session_id, &decision, Vec::new());

		// execute_tool_loop is async; bridge via block_on with a multi-thread runtime so that
		// (a) block_in_place inside execute_tool_loop can run blocking tool invocations, and
		// (b) LlmRouter (which holds a blocking_runtime) is dropped in sync scope, not async.
		let execution = tokio::runtime::Builder::new_multi_thread()
			.enable_all()
			.build()
			.expect("tokio runtime for execute-tool-loop bridge should build")
			.block_on(runtime.execute_tool_loop(
				&TaskId("task-loop".to_string()),
				&request,
				&mut loop_state,
				&RuntimeMemorySections::default(),
				None,
				None,
				None,
			));

		assert_eq!(execution.result.status, ResultStatus::Ok);
		assert_eq!(
			execution.terminal_step_action,
			Some(StepAction::FinalAnswer)
		);
		assert_eq!(
			execution.message,
			"Loop concluded after a non-terminal filesystem observation."
		);
		assert_eq!(loop_state.history.len(), 2);
		assert_eq!(loop_state.history[0].tool_name.as_deref(), Some("Inspect"));
		assert_eq!(loop_state.history[1].action, StepAction::FinalAnswer);
		assert_eq!(
			loop_state.status,
			crate::runtime_loop::LoopStatus::Succeeded
		);
		assert_eq!(
			loop_state.visible_tools.first().map(String::as_str),
			Some("Inspect")
		);
		assert!(
			loop_state
				.history
				.first()
				.and_then(|step| step.observation.as_ref())
				.is_some_and(|observation| matches!(
					observation,
					crate::runtime_loop::StepObservation::Tool(tool_observation)
						if !tool_observation.terminal
				))
		);

		let prompts = prompts.lock().expect("prompt lock should succeed");
		assert_eq!(prompts.len(), 2);
		// First LLM call: tool definitions include Inspect.
		assert!(prompts[0].contains("\"Inspect\""));
		// Second LLM call: messages include the ToolResult from the first Inspect call,
		// so the Inspect name should appear in the captured messages JSON.
		assert!(prompts[1].contains("Inspect"));
	}

	#[test]
	fn execute_tool_loop_records_ask_user_terminal_state() {
		let (route_router, _prompts) = router_with_json_responses(vec![serde_json::json!({
			"action": "ask_user",
			"tool_name": null,
			"arguments": null,
			"reason": "Need a concrete file path before continuing.",
			"final_message": "Which file should I inspect?"
		})]);
		let execution_router = router_with_text_output("unused-execution-provider", "unused");
		let root = tempfile::tempdir().expect("temp root should exist");
		let runtime =
			GenericAgentRuntime::with_route_and_execution_routers_skill_registry_tool_config_and_plugin_snapshot(
				route_router,
				execution_router,
				SkillRegistry::file_backed(root.keep()),
				ToolCatalogConfig::default(),
				PluginRegistrySnapshot::permissive(),
				ToolsRuntimeConfig::default(),
			);
		let request = RequestEnvelope {
			request_id: roku_common_types::RequestId("req-ask-user".to_string()),
			session_id: "session-ask-user".to_string(),
			goal: "Inspect a file for me".to_string(),
			planning_mode_hint: None,
			conversation_history: Vec::new(),
			model_override: None,
			thinking_effort: None,
		};
		let decision = crate::router::RouteDecision::new(
			IntentFamily::Chat,
			0.91,
			false,
			crate::router::RouteRisk::Low,
			vec!["inventory.describe".to_string()],
			Vec::new(),
			Vec::new(),
			"chat request",
		);
		let mut loop_state =
			runtime.initialize_runtime_loop(&request, &request.session_id, &decision, Vec::new());

		// execute_tool_loop is async; bridge via block_on with a multi-thread runtime so that
		// (a) block_in_place inside execute_tool_loop can run blocking tool invocations, and
		// (b) LlmRouter (which holds a blocking_runtime) is dropped in sync scope, not async.
		let execution = tokio::runtime::Builder::new_multi_thread()
			.enable_all()
			.build()
			.expect("tokio runtime for execute-tool-loop bridge should build")
			.block_on(runtime.execute_tool_loop(
				&TaskId("task-ask-user".to_string()),
				&request,
				&mut loop_state,
				&RuntimeMemorySections::default(),
				None,
				None,
				None,
			));

		assert_eq!(execution.result.status, ResultStatus::Ok);
		assert_eq!(execution.terminal_step_action, Some(StepAction::AskUser));
		assert_eq!(loop_state.history.len(), 1);
		assert_eq!(loop_state.history[0].action, StepAction::AskUser);
		assert_eq!(
			loop_state.status,
			crate::runtime_loop::LoopStatus::AwaitingUser
		);
	}

	#[test]
	fn execute_tool_loop_preserves_approval_required_command_payload_for_terminal_failures() {
		let (route_router, _prompts) = router_with_json_responses(vec![
			serde_json::json!({
				"action": "call_tool",
				"tool_name": "Bash",
				"arguments": {
					"command": "dd if=/dev/zero of=/dev/null"
				},
				"reason": "Run the requested command directly.",
				"final_message": null
			}),
			// Under the new semantics, approval_required is a non-terminal error that
			// lets the LLM see the error as an observation and decide next. Provide a
			// second response so the LLM router does not panic on the follow-up call.
			serde_json::json!({
				"action": "fail",
				"reason": "Cannot proceed: Bash requires approval.",
				"final_message": null
			}),
		]);
		let execution_router = router_with_text_output("unused-execution-provider", "unused");
		let root = tempfile::tempdir().expect("temp root should exist");
		let runtime =
			GenericAgentRuntime::with_route_and_execution_routers_skill_registry_tool_config_and_plugin_snapshot(
				route_router,
				execution_router,
				SkillRegistry::file_backed(root.keep()),
				ToolCatalogConfig::default(),
				PluginRegistrySnapshot::permissive(),
				ToolsRuntimeConfig::default(),
			);
		let request = RequestEnvelope {
			request_id: roku_common_types::RequestId("req-command-approval".to_string()),
			session_id: "session-command-approval".to_string(),
			goal: "run dd if=/dev/zero of=/dev/null".to_string(),
			planning_mode_hint: None,
			conversation_history: Vec::new(),
			model_override: None,
			thinking_effort: None,
		};
		let decision = crate::router::RouteDecision::new(
			IntentFamily::CodeExec,
			0.95,
			false,
			crate::router::RouteRisk::Low,
			vec!["Bash".to_string()],
			vec!["core-command".to_string()],
			Vec::new(),
			"command execution request",
		);
		let mut loop_state =
			runtime.initialize_runtime_loop(&request, &request.session_id, &decision, Vec::new());

		// execute_tool_loop is async; bridge via block_on with a multi-thread runtime so that
		// (a) block_in_place inside execute_tool_loop can run blocking tool invocations, and
		// (b) LlmRouter (which holds a blocking_runtime) is dropped in sync scope, not async.
		let execution = tokio::runtime::Builder::new_multi_thread()
			.enable_all()
			.build()
			.expect("tokio runtime for execute-tool-loop bridge should build")
			.block_on(runtime.execute_tool_loop(
				&TaskId("task-command-approval".to_string()),
				&request,
				&mut loop_state,
				&RuntimeMemorySections::default(),
				None,
				None,
				None,
			));

		let payload = payload_value(&execution.result);
		assert_eq!(execution.result.status, ResultStatus::Error);
		// Under the new semantics, approval_required is a non-terminal error: the loop
		// continues and terminates via synthetic_loop_terminal_result, which uses
		// "runtime-loop:tool" as the node_id rather than "direct-route".
		assert_eq!(execution.node.node_id.0, "runtime-loop:tool");
		assert_eq!(execution.terminal_step_action, Some(StepAction::Fail));
		// The policy payload is no longer promoted to the top-level result; it is
		// captured in the probe_trace for the tool step.
		assert_eq!(
			payload
				.get("runtime_loop")
				.and_then(serde_json::Value::as_str),
			Some("tool")
		);
		assert!(payload.get("probe_trace").is_some());
	}

	#[test]
	fn initialize_runtime_loop_uses_shortlist_and_baseline_visible_tools() {
		let runtime = GenericAgentRuntime::default();
		let request = RequestEnvelope {
			request_id: roku_common_types::RequestId("req-loop".to_string()),
			session_id: "session-loop".to_string(),
			goal: "Read Cargo.toml".to_string(),
			planning_mode_hint: None,
			conversation_history: Vec::new(),
			model_override: None,
			thinking_effort: None,
		};
		let decision = crate::router::RouteDecision::new(
			IntentFamily::FilesystemRead,
			0.92,
			false,
			crate::router::RouteRisk::Low,
			vec!["Read".to_string(), "not.enabled".to_string()],
			vec!["core-fs".to_string()],
			Vec::new(),
			"filesystem request",
		);

		let loop_state =
			runtime.initialize_runtime_loop(&request, &request.session_id, &decision, Vec::new());

		assert_eq!(loop_state.run_id, "loop-req-loop");
		assert_eq!(
			loop_state.visible_tools.first().map(String::as_str),
			Some("Read")
		);
		assert!(
			!loop_state
				.visible_tools
				.contains(&"general.execute".to_string()),
			"general.execute must not appear in visible tools after removal"
		);
		assert!(
			!loop_state
				.visible_tools
				.contains(&"inventory.describe".to_string()),
			"inventory.describe must not appear in visible tools after removal"
		);
		assert!(
			loop_state
				.visible_tools
				.contains(&"SkillInstall".to_string())
		);
		assert!(
			loop_state
				.visible_tools
				.contains(&"TablePreview".to_string())
		);
		assert!(loop_state.visible_tools.contains(&"Python".to_string()));
		assert_eq!(loop_state.history.len(), 0);
	}

	#[test]
	fn initialize_runtime_loop_seeds_visible_tools_from_shared_availability_snapshot() {
		let runtime = GenericAgentRuntime {
			runtime_visible_tool_availability_snapshot: RuntimeVisibleToolAvailabilitySnapshot {
				enabled_tools: ["inventory.describe".to_string(), "TablePreview".to_string()]
					.into_iter()
					.collect(),
				baseline_visible_tools: vec!["inventory.describe".to_string()],
			},
			..GenericAgentRuntime::default()
		};
		let request = RequestEnvelope {
			request_id: roku_common_types::RequestId("req-loop-snapshot".to_string()),
			session_id: "session-loop-snapshot".to_string(),
			goal: "Preview the loaded table".to_string(),
			planning_mode_hint: None,
			conversation_history: Vec::new(),
			model_override: None,
			thinking_effort: None,
		};
		let decision = crate::router::RouteDecision::new(
			IntentFamily::TableRead,
			0.88,
			false,
			crate::router::RouteRisk::Low,
			vec!["Read".to_string(), "TablePreview".to_string()],
			vec!["core-table".to_string()],
			Vec::new(),
			"table preview request",
		);

		let loop_state =
			runtime.initialize_runtime_loop(&request, &request.session_id, &decision, Vec::new());

		assert_eq!(
			loop_state.visible_tools,
			vec!["TablePreview".to_string(), "inventory.describe".to_string(),]
		);
	}

	#[test]
	fn execute_tool_loop_can_switch_tools_after_an_insufficient_lookup_observation() {
		let text_path = regression_fixture_path(".txt", "react recovery fixture\n");
		let file_name = PathBuf::from(&text_path)
			.file_name()
			.and_then(|value| value.to_str())
			.expect("fixture file name should resolve")
			.to_string();
		// Supply a two-turn mock: first call Find (which finds and returns a path),
		// then read the located path with Read, then emit final_answer.
		let (route_router, _prompts) = router_with_json_responses(vec![
			serde_json::json!({
				"action": "call_tool",
				"tool_name": "Find",
				"arguments": { "name": file_name },
				"reason": "find the file first",
				"final_message": null
			}),
			serde_json::json!({
				"action": "call_tool",
				"tool_name": "Read",
				"arguments": { "path": text_path },
				"reason": "read the located file",
				"final_message": null
			}),
			serde_json::json!({
				"action": "final_answer",
				"tool_name": null,
				"arguments": null,
				"reason": "file read complete",
				"final_message": "react recovery fixture\n"
			}),
		]);
		let root = tempfile::tempdir().expect("temp root should exist");
		let runtime =
			GenericAgentRuntime::with_route_and_execution_routers_skill_registry_tool_config_and_plugin_snapshot(
				route_router,
				router_with_text_output("execution-provider", "live answer"),
				SkillRegistry::file_backed(root.keep()),
				ToolCatalogConfig::default(),
				PluginRegistrySnapshot::permissive(),
				ToolsRuntimeConfig::default(),
			);
		let request = RequestEnvelope {
			request_id: roku_common_types::RequestId("req-react-recovery".to_string()),
			session_id: "session-react-recovery".to_string(),
			goal: format!("Find and read {file_name}."),
			planning_mode_hint: None,
			conversation_history: Vec::new(),
			model_override: None,
			thinking_effort: None,
		};
		let decision = crate::router::RouteDecision::new(
			IntentFamily::FilesystemRead,
			0.8,
			false,
			crate::router::RouteRisk::Low,
			vec!["Find".to_string(), "Read".to_string()],
			vec!["core-fs".to_string()],
			Vec::new(),
			"seed the loop with a lookup-first filesystem hint",
		);
		let mut loop_state =
			runtime.initialize_runtime_loop(&request, &request.session_id, &decision, Vec::new());
		let task_id = TaskId("task-react-recovery".to_string());

		// execute_tool_loop is async; bridge via block_on with a multi-thread runtime so that
		// block_in_place inside execute_tool_loop can run blocking tool invocations.
		let result = tokio::runtime::Builder::new_multi_thread()
			.enable_all()
			.build()
			.expect("tokio runtime for execute-tool-loop bridge should build")
			.block_on(runtime.execute_tool_loop(
				&task_id,
				&request,
				&mut loop_state,
				&RuntimeMemorySections::default(),
				None,
				None,
				None,
			));

		let tool_sequence = loop_state
			.history
			.iter()
			.filter_map(|step| step.tool_name.clone())
			.collect::<Vec<_>>();

		assert_eq!(tool_sequence, vec!["Find".to_string(), "Read".to_string()]);
		assert_eq!(result.terminal_step_action, Some(StepAction::FinalAnswer));
		cleanup_fixture(&text_path);
	}
	#[test]
	fn visible_tools_recompute_keeps_shortlist_and_safe_baseline_after_tool_steps() {
		let runtime = GenericAgentRuntime {
			runtime_visible_tool_availability_snapshot: RuntimeVisibleToolAvailabilitySnapshot {
				enabled_tools: [
					"Read".to_string(),
					"inventory.describe".to_string(),
					"TablePreview".to_string(),
				]
				.into_iter()
				.collect(),
				baseline_visible_tools: vec!["inventory.describe".to_string()],
			},
			..GenericAgentRuntime::default()
		};
		let request = RequestEnvelope {
			request_id: roku_common_types::RequestId("req-followup".to_string()),
			session_id: "session-followup".to_string(),
			goal: "Read Cargo.toml and summarize the workspace layout".to_string(),
			planning_mode_hint: None,
			conversation_history: Vec::new(),
			model_override: None,
			thinking_effort: None,
		};
		let decision = crate::router::RouteDecision::new(
			IntentFamily::FilesystemRead,
			0.94,
			false,
			crate::router::RouteRisk::Low,
			vec!["Read".to_string()],
			vec!["core-fs".to_string()],
			Vec::new(),
			"filesystem request",
		);
		let mut loop_state =
			runtime.initialize_runtime_loop(&request, &request.session_id, &decision, Vec::new());
		// With all-tools-always-visible, visible_tools includes all enabled tools
		assert!(loop_state.visible_tools.contains(&"Read".to_string()));
		assert!(
			loop_state
				.visible_tools
				.contains(&"inventory.describe".to_string())
		);
		assert!(
			loop_state
				.visible_tools
				.contains(&"TablePreview".to_string())
		);
		let observation = ToolObservation {
			ok: true,
			tool_name: "Read".to_string(),
			error_type: None,
			terminal: false,
			data: serde_json::json!({
				"path": "/workspace/Cargo.toml",
				"content": "[workspace]\nmembers = [\"crates/roku-agent-runtime\"]",
			}),
			message: "Read the grounded workspace manifest.".to_string(),
		};
		let interpreted =
			crate::runtime_loop::interpret_observation(&loop_state, observation.clone(), None);
		loop_state.record_step(StepRecord::tool_call(
			1,
			crate::runtime_loop::NextStepDecision {
				action: crate::runtime_loop::NextStepAction::CallTool,
				tool_name: Some("Read".to_string()),
				arguments: Some(serde_json::json!({ "path": "Cargo.toml" })),
				tool_calls: None,
				reason: "Read the grounded workspace manifest first.".to_string(),
				final_message: None,
			},
			loop_state.visible_tools.clone(),
			loop_state.bound_resources.clone(),
			serde_json::json!({
				"ok": true,
				"terminal": false,
				"message": "Read the grounded workspace manifest.",
				"data": observation.data.clone(),
			}),
			StepObservation::Tool(observation),
			interpreted.clone(),
			Some(12),
			interpreted.remaining_step_budget,
			interpreted.remaining_recovery_budget,
			"/workspace",
		));

		let visible_tools = runtime.visible_tools_for_loop_state(&loop_state);

		// With all-tools-always-visible, visible_tools includes all enabled tools
		assert!(visible_tools.contains(&"Read".to_string()));
		assert!(visible_tools.contains(&"inventory.describe".to_string()));
		assert!(visible_tools.contains(&"TablePreview".to_string()));
		assert!(
			!visible_tools.contains(&"general.execute".to_string()),
			"recompute must not reintroduce removed tools"
		);
	}

	#[test]
	fn record_terminal_step_updates_history_and_status() {
		let runtime = GenericAgentRuntime::default();
		let request = RequestEnvelope {
			request_id: roku_common_types::RequestId("req-terminal".to_string()),
			session_id: "session-terminal".to_string(),
			goal: "hello".to_string(),
			planning_mode_hint: None,
			conversation_history: Vec::new(),
			model_override: None,
			thinking_effort: None,
		};
		let decision = crate::router::RouteDecision::new(
			IntentFamily::Chat,
			0.8,
			false,
			crate::router::RouteRisk::Low,
			Vec::new(),
			Vec::new(),
			Vec::new(),
			"chat request",
		);
		let mut loop_state =
			runtime.initialize_runtime_loop(&request, &request.session_id, &decision, Vec::new());

		let step = runtime.record_terminal_step(
			&mut loop_state,
			crate::runtime_loop::StepAction::FinalAnswer,
			"phase1 skeleton terminal step",
			Some("hello".to_string()),
		);

		assert_eq!(step.step_index, 1);
		assert_eq!(loop_state.history.len(), 1);
		assert_eq!(
			loop_state.status,
			crate::runtime_loop::LoopStatus::Succeeded
		);
	}

	// normalize_tool_loop_observation test removed — function was a no-op identity
	// and has been removed along with the function extraction to tools::dispatch.

	#[test]
	fn execute_tool_loop_recovers_from_context_window_exceeded_via_reactive_compact() {
		// First LLM call fails with ContextWindowExceeded; the runtime should
		// run a single reactive compaction on the message buffer, retry the
		// call, and then succeed with the canned final_answer response.
		let (route_router, invocations) = router_with_reactive_compact_provider(vec![
			Err(ProviderCallError::ContextWindowExceeded {
				detail: "prompt is too long: 215321 tokens > 200000".to_string(),
			}),
			Ok(serde_json::json!({
				"action": "final_answer",
				"tool_name": null,
				"arguments": null,
				"reason": "Recovered after reactive compact.",
				"final_message": "Loop recovered from context overflow."
			})
			.to_string()),
		]);
		let execution_router =
			router_with_text_output("reactive-execution-provider", "unused execution");
		let root = tempfile::tempdir().expect("temp root should exist");
		let runtime =
			GenericAgentRuntime::with_route_and_execution_routers_skill_registry_tool_config_and_plugin_snapshot(
				route_router,
				execution_router,
				SkillRegistry::file_backed(root.keep()),
				ToolCatalogConfig::default(),
				PluginRegistrySnapshot::permissive(),
				ToolsRuntimeConfig::default(),
			);
		let request = RequestEnvelope {
			request_id: roku_common_types::RequestId("req-reactive".to_string()),
			session_id: "session-reactive".to_string(),
			goal: "Reactive compact recovery test".to_string(),
			planning_mode_hint: None,
			conversation_history: Vec::new(),
			model_override: None,
			thinking_effort: None,
		};
		let decision = crate::router::RouteDecision::new(
			IntentFamily::Chat,
			0.95,
			false,
			crate::router::RouteRisk::Low,
			Vec::new(),
			Vec::new(),
			Vec::new(),
			"chat request",
		);
		let mut loop_state =
			runtime.initialize_runtime_loop(&request, &request.session_id, &decision, Vec::new());

		let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
		let execution = tokio::runtime::Builder::new_multi_thread()
			.enable_all()
			.build()
			.expect("tokio runtime for execute-tool-loop bridge should build")
			.block_on(runtime.execute_tool_loop(
				&TaskId("task-reactive".to_string()),
				&request,
				&mut loop_state,
				&RuntimeMemorySections::default(),
				None,
				Some(&event_tx),
				None,
			));

		assert_eq!(execution.result.status, ResultStatus::Ok);
		assert_eq!(
			execution.terminal_step_action,
			Some(StepAction::FinalAnswer)
		);
		assert_eq!(execution.message, "Loop recovered from context overflow.");
		assert_eq!(
			loop_state.status,
			crate::runtime_loop::LoopStatus::Succeeded
		);
		// Provider was called twice: 1 failure + 1 success after compaction.
		assert_eq!(invocations.load(std::sync::atomic::Ordering::SeqCst), 2);

		// Verify the runtime emitted a ReactiveCompactTriggered event.
		drop(event_tx);
		let mut events = Vec::new();
		while let Ok(event) = event_rx.try_recv() {
			events.push(event);
		}
		let reactive_count = events
			.iter()
			.filter(|e| {
				matches!(
					e,
					crate::runtime_loop::LoopEvent::ReactiveCompactTriggered { .. }
				)
			})
			.count();
		assert_eq!(
			reactive_count, 1,
			"exactly one ReactiveCompactTriggered event should be emitted"
		);
		assert!(
			events
				.iter()
				.any(|e| matches!(e, crate::runtime_loop::LoopEvent::CompactComplete { .. })),
			"a CompactComplete event should follow the reactive trigger"
		);
	}

	#[test]
	fn execute_tool_loop_terminates_when_context_window_exceeded_persists_after_compact() {
		// Both LLM calls fail with ContextWindowExceeded. The runtime allows
		// at most one reactive compact + retry per turn, so the second failure
		// must terminate the loop instead of looping forever.
		let (route_router, invocations) = router_with_reactive_compact_provider(vec![
			Err(ProviderCallError::ContextWindowExceeded {
				detail: "prompt is too long: 215321 tokens > 200000".to_string(),
			}),
			Err(ProviderCallError::ContextWindowExceeded {
				detail: "prompt is too long: 214900 tokens > 200000".to_string(),
			}),
		]);
		let execution_router =
			router_with_text_output("reactive-execution-provider-2", "unused execution");
		let root = tempfile::tempdir().expect("temp root should exist");
		let runtime =
			GenericAgentRuntime::with_route_and_execution_routers_skill_registry_tool_config_and_plugin_snapshot(
				route_router,
				execution_router,
				SkillRegistry::file_backed(root.keep()),
				ToolCatalogConfig::default(),
				PluginRegistrySnapshot::permissive(),
				ToolsRuntimeConfig::default(),
			);
		let request = RequestEnvelope {
			request_id: roku_common_types::RequestId("req-reactive-fail".to_string()),
			session_id: "session-reactive-fail".to_string(),
			goal: "Reactive compact terminal test".to_string(),
			planning_mode_hint: None,
			conversation_history: Vec::new(),
			model_override: None,
			thinking_effort: None,
		};
		let decision = crate::router::RouteDecision::new(
			IntentFamily::Chat,
			0.95,
			false,
			crate::router::RouteRisk::Low,
			Vec::new(),
			Vec::new(),
			Vec::new(),
			"chat request",
		);
		let mut loop_state =
			runtime.initialize_runtime_loop(&request, &request.session_id, &decision, Vec::new());

		let execution = tokio::runtime::Builder::new_multi_thread()
			.enable_all()
			.build()
			.expect("tokio runtime for execute-tool-loop bridge should build")
			.block_on(runtime.execute_tool_loop(
				&TaskId("task-reactive-fail".to_string()),
				&request,
				&mut loop_state,
				&RuntimeMemorySections::default(),
				None,
				None,
				None,
			));

		assert_eq!(execution.result.status, ResultStatus::Error);
		assert_eq!(execution.terminal_step_action, Some(StepAction::Fail));
		assert!(
			execution
				.message
				.contains("context window still exceeded after reactive compact"),
			"terminal message must explain the persistent context overflow, got: {}",
			execution.message
		);
		// Exactly two calls — the second failure terminates the loop.
		assert_eq!(invocations.load(std::sync::atomic::Ordering::SeqCst), 2);
		assert!(matches!(
			loop_state.status,
			crate::runtime_loop::LoopStatus::Failed
		));
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
				.start_file("skills-main/skills/claude-api/SKILL.md", options)
				.expect("skill file should start");
			writer
				.write_all(
					br#"---
name: claude-api
description: Build apps with the Claude API.
---

# Claude API Skill

Use this skill when the user explicitly asks for Claude API integration help.
"#,
				)
				.expect("skill markdown should write");
			writer.finish().expect("zip should finish");
		}
		cursor.into_inner()
	}
}
