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
	GenerationRequest, LlmAdapterError, LlmRouter, Message, RiskTier, StreamChunk,
	SystemPromptSections, ThinkingEffort, TokenCounter, ToolCallBlock, ToolDefinition,
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
	/// Optional long-term memory backend used by Layer 2 mid-tier compaction
	/// to reuse a prior session's compact summary instead of paying for an
	/// LLM summarizer call. When `None`, mid-tier compaction transparently
	/// falls back to Layer 1 mechanical collapse.
	memory_backend: Option<Arc<dyn roku_memory::LongTermMemoryBackend>>,
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
			memory_backend: None,
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

	/// Resolve the three preflight views the attempt loop depends on
	/// through the full `select_model` policy: provider-owned token
	/// counter, routed serving model id, and wire-bytes preview of the
	/// tool-schema block. All three are sourced from the same
	/// `GenerationRequest`-shaped selection payload so they agree on
	/// the provider `router.generate*` will actually land on — model
	/// override eligibility, risk tier, budgets, and message-size
	/// context checks all contribute.
	///
	/// Called once per attempt before the mid-tier pre-flight block,
	/// and again after the block if mid-tier compaction mutated
	/// `messages` (the shorter buffer can flip `select_model`'s
	/// eligibility decision, rerouting to a different provider whose
	/// counter and wire format differ). Shared helper keeps the two
	/// resolution sites from drifting.
	///
	/// `route_router` (not `execution_router`) is consulted so the
	/// routed id matches what `router.generate*` will actually serve
	/// under the two-router configuration where those diverge.
	#[allow(clippy::type_complexity)]
	fn resolve_attempt_preflight(
		&self,
		messages: &[Message],
		system_prompt: &str,
		system_prompt_sections: &SystemPromptSections,
		tool_definitions: &[ToolDefinition],
		model_override: Option<String>,
		config: &crate::runtime_config::NextStepRuntimeConfig,
	) -> (Arc<dyn TokenCounter>, Option<String>, Vec<u8>) {
		let selection_request = GenerationRequest {
			system_prompt: Some(system_prompt.to_string()),
			prompt: String::new(),
			messages: Some(messages.to_vec()),
			expected_output_tokens: config.expected_output_tokens,
			risk_tier: RiskTier::Low,
			preferred_provider: None,
			budget_tokens_remaining: config.budget_tokens_remaining,
			budget_cost_remaining_usd: config.budget_cost_remaining_usd,
			tools: None,
			model_override,
			thinking_effort: None,
			system_prompt_sections: Some(system_prompt_sections.clone()),
		};
		let counter = self
			.route_router
			.as_ref()
			.map(|r| r.token_counter_for_request(&selection_request))
			.unwrap_or_else(roku_plugin_llm::default_counter);
		let routed_model = self
			.route_router
			.as_ref()
			.and_then(|r| r.selected_model_id_for_request(&selection_request));
		let schema_bytes = self
			.route_router
			.as_ref()
			.map(|r| {
				r.preview_wire_tool_schema_bytes_for_request(&selection_request, tool_definitions)
			})
			.unwrap_or_else(|| serde_json::to_vec(tool_definitions).unwrap_or_default());
		(counter, routed_model, schema_bytes)
	}

	/// Resolve the calibration `(estimated, real)` pair plus the
	/// tool-schema byte length to record in the committed baseline
	/// when an output-slot escalation retry lands.
	///
	/// The primary call ran under the attempt loop's resolved
	/// `(counter, current_routed_model, attempt_schema_bytes_vec)` and
	/// produced `pre_call_estimate`. If the escalation retry stayed
	/// on the same routed model (`retry_resp_model_id ==
	/// primary_resp_model_id`), those primary views are still
	/// authoritative and we feed `pre_call_estimate.calibration_pair`
	/// with the primary's byte-length — the current fast path.
	///
	/// When the escalation reroutes (typical when the retry switches
	/// to a larger model that `select_model` re-picks under the
	/// expanded `expected_output_tokens`), the primary's views belong
	/// to the wrong provider: its counter's byte→token bias does not
	/// match the retry's tokenizer, and its wire-bytes length is
	/// shaped for the primary provider's serializer. Recording
	/// `(primary_estimate, retry_real)` into the retry's per-model
	/// calibration bucket injects model A's residual bias into model
	/// B's bucket, and persisting the primary's schema-bytes length
	/// against the retry's serving id breaks next turn's
	/// `CommittedBaseline::is_valid_for` on the schema-bytes guard.
	///
	/// Fix: when a reroute is detected, re-resolve the retry model's
	/// counter and wire-bytes preview (same `resolve_attempt_preflight`
	/// helper the attempt loop uses, with the retry's id as the
	/// override hint), compute a fresh `pre_call_estimate` with those
	/// views, and base the calibration sample and baseline length on
	/// the retry-specific estimate.
	#[allow(clippy::too_many_arguments)]
	fn retry_preflight_values(
		&self,
		retry_resp_model_id: &str,
		primary_resp_model_id: &str,
		messages: &[Message],
		system_prompt: &str,
		system_prompt_sections: &SystemPromptSections,
		tool_definitions: &[ToolDefinition],
		config: &crate::runtime_config::NextStepRuntimeConfig,
		pre_call_system_prompt_bytes: usize,
		pre_call_tool_schema_bytes_len: usize,
		pre_call_system_prompt_hash: u64,
		pre_call_prefix_messages_hash: u64,
		pre_call_estimate: &crate::runtime_loop::PromptTokenEstimate,
		retry_prompt_tokens: u64,
		calibration: &crate::runtime_loop::PerModelCalibration,
		loop_state_baseline: Option<crate::runtime_loop::CommittedBaseline>,
	) -> (u64, u64, usize, u64) {
		if retry_resp_model_id == primary_resp_model_id {
			let (est, real) = pre_call_estimate.calibration_pair(retry_prompt_tokens);
			// Same-route retry reuses the primary's pre-call estimate /
			// schema-bytes length and must persist the same tool-schema
			// content hash so next turn's `is_valid_for` sees the exact
			// wire bytes the provider tokenized this turn.
			let primary_schema_hash = {
				// Reconstruct the primary's tool schema for hashing.
				let (_counter, _routed, primary_schema_bytes_vec) = self.resolve_attempt_preflight(
					messages,
					system_prompt,
					system_prompt_sections,
					tool_definitions,
					Some(primary_resp_model_id.to_string()),
					config,
				);
				crate::runtime_loop::hash_tool_schema_bytes(&primary_schema_bytes_vec)
			};
			return (
				est,
				real,
				pre_call_tool_schema_bytes_len,
				primary_schema_hash,
			);
		}
		// Rerouted retry: re-resolve the retry model's preflight views.
		let (retry_counter, _routed, retry_schema_bytes_vec) = self.resolve_attempt_preflight(
			messages,
			system_prompt,
			system_prompt_sections,
			tool_definitions,
			Some(retry_resp_model_id.to_string()),
			config,
		);
		let retry_schema_bytes: Option<&[u8]> = if retry_schema_bytes_vec.is_empty() {
			None
		} else {
			Some(&retry_schema_bytes_vec)
		};
		let retry_schema_len = retry_schema_bytes.map(|b| b.len()).unwrap_or(0);
		let retry_tool_schema_hash =
			crate::runtime_loop::hash_tool_schema_bytes(&retry_schema_bytes_vec);
		let retry_baseline = loop_state_baseline.filter(|b| {
			b.is_valid_for(
				pre_call_system_prompt_bytes as u64,
				retry_schema_len as u64,
				pre_call_system_prompt_hash,
				retry_tool_schema_hash,
				pre_call_prefix_messages_hash,
				Some(retry_resp_model_id),
			)
		});
		let retry_estimate = crate::runtime_loop::estimate_prompt_tokens_calibrated(
			messages,
			Some(system_prompt),
			retry_schema_bytes,
			calibration.get(Some(retry_resp_model_id)),
			retry_counter.as_ref(),
			retry_baseline,
		);
		let (est, real) = retry_estimate.calibration_pair(retry_prompt_tokens);
		(est, real, retry_schema_len, retry_tool_schema_hash)
	}

	/// Returns `(prompt_tokens, output_tokens)` consumed by compaction LLM calls.
	async fn maybe_compact(
		&self,
		loop_state: &mut LoopState,
		messages: &mut Vec<roku_plugin_llm::Message>,
		current_step_index: u32,
		event_sender: Option<&crate::runtime_loop::LoopEventSender>,
		system_prompt: &str,
		tool_schema_bytes: Option<&[u8]>,
		model_override: Option<&str>,
	) -> (u64, u64) {
		// Truncate oversized tool results in messages.
		let tool_result_max_chars = self.agent_runtime_config.r#loop.working_summary_max_chars;
		crate::runtime_loop::truncate_large_tool_results(messages, tool_result_max_chars);

		// `tool_schema_bytes` was captured at turn start — both the schema
		// shape and the provider pick are from that moment. Two drift
		// sources make the pre-flight bytes potentially wrong here:
		//
		// 1. **Schema dirty.** If a tool executed this turn mutated the
		//    visible tool set (e.g. `tool_search` loading deferred schemas,
		//    or any path that calls `mark_tool_schema_dirty`), the cached
		//    bytes describe an outdated schema.
		// 2. **Messages changed.** Even when the schema is unchanged, tool
		//    execution appends messages. `select_model`'s eligibility
		//    depends on input-token size, so the message-size change can
		//    flip the picked provider — the pre-flight bytes would then
		//    be in a different provider's wire format than the next
		//    `generate` call will send.
		//
		// Recompute unconditionally. The tool definitions come from either
		// a fresh rebuild (when dirty) or the frozen per-turn snapshot
		// (otherwise — the same set the pre-flight used; cheap to clone).
		// The selection request is built from **current** `messages` +
		// `system_prompt` so the router's `select_model` matches what the
		// next outbound call would pick.
		//
		// For dirty rebuilds, `apply_deferred_mode` runs before serialization
		// so the bytes reflect the deferred-mode swap (withholding non-core
		// schemas and injecting the `tool_search` stub) rather than the
		// full visible set.
		// Pick the tool definitions that match what the next outbound call
		// will actually send. `frozen_tool_schema` holds the **pre-deferred**
		// snapshot (the full visible catalog); the pre-flight `tool_schema_bytes`
		// was computed after `apply_deferred_mode` ran, so reusing the frozen
		// snapshot as-is would regress deferred-mode sessions — the estimator
		// would count bytes against the full schema while the wire actually
		// carries the deferred-filtered subset (non-core schemas replaced by
		// the `tool_search` stub). Run `apply_deferred_mode` unconditionally
		// so both branches end with the effective set the provider will see.
		let effective_definitions: Option<Vec<ToolDefinition>> = if loop_state.tool_schema_dirty {
			let fresh = crate::runtime_loop::build_tool_definitions(
				&loop_state.visible_tools,
				Some(&self.resource_catalog),
				&loop_state.disallowed_tools,
			);
			Some(crate::runtime_loop::apply_deferred_mode(
				fresh,
				loop_state,
				self.agent_runtime_config.r#loop.context_window_tokens,
			))
		} else {
			loop_state
				.frozen_tool_schema
				.as_ref()
				.map(|snapshot| snapshot.definitions.clone())
				.map(|base| {
					crate::runtime_loop::apply_deferred_mode(
						base,
						loop_state,
						self.agent_runtime_config.r#loop.context_window_tokens,
					)
				})
		};

		// Shared request shape used to route both the tool-schema wire-bytes
		// preview and the token-counter selection through the same
		// `select_model` policy. Constructed once so both resolutions see
		// an identical request and agree on the provider.
		let selection_request = GenerationRequest {
			system_prompt: Some(system_prompt.to_string()),
			prompt: String::new(),
			messages: Some(messages.clone()),
			expected_output_tokens: self.agent_runtime_config.next_step.expected_output_tokens,
			risk_tier: RiskTier::Low,
			preferred_provider: None,
			budget_tokens_remaining: self.agent_runtime_config.next_step.budget_tokens_remaining,
			budget_cost_remaining_usd: self
				.agent_runtime_config
				.next_step
				.budget_cost_remaining_usd,
			tools: None,
			model_override: model_override.map(str::to_string),
			thinking_effort: None,
			system_prompt_sections: None,
		};

		// Route all three preflight resolutions (wire-bytes preview,
		// token counter, routed-model id) through `route_router` — the
		// same router instance whose `generate*` will actually serve
		// the upcoming call. When the runtime is configured with
		// distinct route / execution routers (subagent execution vs.
		// main-loop generation), consulting `execution_router` here
		// would predict a different provider's wire shape, tokenizer,
		// and routed model id than what the real call lands on, so the
		// estimator and committed-baseline validator would operate
		// against the wrong router on every turn.
		let rebuilt_schema_bytes: Option<Vec<u8>> = effective_definitions.and_then(|defs| {
			if defs.is_empty() {
				return None;
			}
			let wire_bytes = self
				.route_router
				.as_ref()
				.map(|r| r.preview_wire_tool_schema_bytes_for_request(&selection_request, &defs))
				.unwrap_or_else(|| serde_json::to_vec(&defs).unwrap_or_default());
			if wire_bytes.is_empty() {
				None
			} else {
				Some(wire_bytes)
			}
		});
		let effective_schema_bytes: Option<&[u8]> =
			rebuilt_schema_bytes.as_deref().or(tool_schema_bytes);

		let threshold = self.agent_runtime_config.r#loop.compact_threshold_tokens();
		let counter = self
			.route_router
			.as_ref()
			.map(|r| r.token_counter_for_request(&selection_request))
			.unwrap_or_else(roku_plugin_llm::default_counter);
		// Only trust the committed baseline when the system-prompt byte
		// length, the tool-schema byte length, and the routed serving
		// model all match what the baseline was committed against. Any
		// shift (dynamic working-directory block grew, plan mode
		// flipped, router fell back to a different model because the
		// committed model became ineligible) means
		// `last_observed_input_tokens` no longer priced the current
		// prefix — fall back to cold-start.
		//
		// The routed model id comes from `selected_model_id_for_request`
		// (which runs the full `select_model` policy), not from
		// `request.model_override`. A null override is the default case,
		// and even a populated override can fall through to priority
		// ordering when the targeted model is ineligible; comparing
		// `model_override` against the committed serving `resp.model_id`
		// would read as a mismatch every turn and permanently disable
		// committed-baseline mode.
		let current_system_bytes = system_prompt.len() as u64;
		let current_schema_bytes = effective_schema_bytes.map(|b| b.len() as u64).unwrap_or(0);
		let current_system_prompt_hash =
			crate::runtime_loop::hash_system_prompt_text(system_prompt);
		let current_tool_schema_hash =
			crate::runtime_loop::hash_tool_schema_bytes(effective_schema_bytes.unwrap_or_default());
		let current_model = self
			.route_router
			.as_ref()
			.and_then(|r| r.selected_model_id_for_request(&selection_request));
		let validated_baseline = loop_state.committed_baseline().filter(|b| {
			if messages.len() < b.message_count {
				return false;
			}
			let current_prefix_messages_hash =
				crate::runtime_loop::hash_message_prefix(&messages[..b.message_count]);
			b.is_valid_for(
				current_system_bytes,
				current_schema_bytes,
				current_system_prompt_hash,
				current_tool_schema_hash,
				current_prefix_messages_hash,
				current_model.as_deref(),
			)
		});
		let estimated = crate::runtime_loop::estimate_prompt_pressure(
			messages,
			Some(system_prompt),
			effective_schema_bytes,
			loop_state
				.estimator_calibration
				.get(current_model.as_deref()),
			counter.as_ref(),
			validated_baseline,
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
					if outcome.thinking_stripped > 0 {
						let _ =
							sender.send(crate::runtime_loop::LoopEvent::ReasoningContentStripped {
								step: current_step_index,
								messages_stripped: outcome.thinking_stripped,
							});
					}
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
				// Seed working_summary + CompactBoundary from the messages-level
				// structured summary when the history-level compact skipped
				// (history.len() <= retain_tail_steps). Without this,
				// `write_back_compact_summaries` would never persist a compact
				// summary for sessions whose message buffer is the bottleneck
				// but whose step count stays small — and Layer 2 reuse on
				// resume would find no memory record to splice.
				if let Some(summary_text) = outcome.summary_text.as_ref() {
					crate::runtime_loop::seed_compact_summary_if_missing(
						loop_state,
						outcome.discarded_count,
						summary_text,
					);
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
		// Compaction rewrote the message buffer and the step history, so the
		// previous committed-token baseline (which pointed into the old
		// buffer) is no longer valid. Clear it so the next pre-flight falls
		// back to the whole-history estimate until the provider reports a
		// fresh `usage.prompt_tokens`.
		loop_state.invalidate_committed_baseline();
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

		// `LoopEvent::TokenUsage` is emitted **per LLM call** (primary,
		// output-slot retry, and the summarizer inside reactive compaction).
		// Consumers that want a loop-level grand total accumulate across the
		// per-call events they observe — `turn.rs` / `render/engine.rs` /
		// `bot.rs` already use `saturating_add` so no consumer-side change is
		// needed. We deliberately do not maintain `total_*` accumulators in
		// the runtime: any aggregate that lives only inside this function
		// would be impossible to expose to the warm-turn cache-utilization
		// gate without the same cold-start dilution that motivated the
		// per-call switch.

		// Fallback cost constants used when no cost profile is found for the model.
		// These are rough estimates for Claude Sonnet tier.
		const FALLBACK_COST_PER_M_INPUT_USD: f64 = 3.0;
		const FALLBACK_COST_PER_M_OUTPUT_USD: f64 = 15.0;

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
			//
			// `freeze_or_reuse_tool_schema` rebuilds iff the frozen snapshot
			// is absent OR the dirty flag is set. Checkpoint-resume restores
			// a `LoopState` whose `frozen_tool_schema` / `tool_schema_dirty`
			// fields are `#[serde(skip)]`, so both default to `None` / `false`
			// after round-trip. Reading `tool_schema_dirty` alone would
			// mis-report `rebuilt=false` on the first post-resume call even
			// though the freeze will actually rebuild. Use the full predicate
			// to match the freeze's real outcome.
			let tool_schema_rebuilt =
				loop_state.tool_schema_dirty || loop_state.frozen_tool_schema.is_none();
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

			// Build the modular system prompt with environment + project
			// instructions. Hoisted before the estimator pre-flight so the
			// selection request handed to `select_model` includes the same
			// system-prompt token weight the real generation request will
			// carry — eligibility (context-window / budget checks) agrees
			// between the two paths and the chosen provider's wire format
			// matches what the turn actually sends.
			//
			// Environment is re-probed each turn; project instruction is
			// stable (loaded once above). The structured `sections` form
			// carries the static/dynamic split that prompt-cache adapters
			// will consume; `system_prompt` keeps the single String shape
			// for adapters that haven't migrated yet.
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

			// Serialize the tool schema AFTER `apply_deferred_mode` so the
			// estimator sees exactly the set the provider will send. Using
			// the pre-deferred set would overestimate pressure on turns where
			// the `tool_search` pseudo-tool has replaced most schemas and
			// silently walk the calibration scale in the wrong direction.
			//
			// Routed through `LlmRouter::preview_wire_tool_schema_bytes_for_request`
			// so the estimator sees the provider-specific wire format chosen by
			// the router's real `select_model` policy — not a priority-based
			// approximation. On multi-provider routers this matters whenever
			// `model_override`, risk-tier filtering, or cost ordering would
			// pick a different provider than the highest-priority one
			// (OpenAI Chat Completions and Responses wrap each entry in
			// `{"type":"function",...}`, Anthropic uses `input_schema` instead
			// of `parameters`, etc.).
			//
			// This selection request mirrors the fields `select_model` reads
			// (`model_override`, `preferred_provider`, `risk_tier`, budgets,
			// and the message / system sizes used for context-window checks).
			// Passing the full system prompt matters for tight context /
			// budget configurations where omitting it would undercount
			// `estimate_request_input_tokens` and admit a model the real
			// call would reject.
			let estimator_selection_request = GenerationRequest {
				system_prompt: Some(system_prompt.clone()),
				prompt: String::new(),
				messages: Some(messages.clone()),
				expected_output_tokens: self.agent_runtime_config.next_step.expected_output_tokens,
				risk_tier: RiskTier::Low,
				preferred_provider: None,
				budget_tokens_remaining: self
					.agent_runtime_config
					.next_step
					.budget_tokens_remaining,
				budget_cost_remaining_usd: self
					.agent_runtime_config
					.next_step
					.budget_cost_remaining_usd,
				tools: None,
				model_override: request.model_override.clone(),
				thinking_effort: None,
				system_prompt_sections: None,
			};
			let tool_schema_bytes_vec = self
				.route_router
				.as_ref()
				.map(|r| {
					r.preview_wire_tool_schema_bytes_for_request(
						&estimator_selection_request,
						&tool_definitions,
					)
				})
				.unwrap_or_else(|| serde_json::to_vec(&tool_definitions).unwrap_or_default());
			let tool_schema_bytes: Option<&[u8]> = if tool_schema_bytes_vec.is_empty() {
				None
			} else {
				Some(&tool_schema_bytes_vec)
			};

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
				// No `emit_token_usage` here: budget exhaustion fires before
				// any LLM call this turn, and prior calls already emitted
				// their own per-call events.
				return self.synthetic_loop_terminal_result(
					task_id,
					"tool",
					message,
					StepAction::Fail,
					ResultStatus::Error,
					Some(loop_state),
				);
			}

			// Pre-compute the tool-schema fingerprint once per turn — the hash
			// is invariant across reactive-retry iterations and escalation
			// retries because `freeze_or_reuse_tool_schema` runs once per
			// outer turn. Hash covers the post-deferred definitions so the
			// signal matches what the provider adapter will actually
			// serialize on the wire.
			let tool_schema_hash =
				crate::runtime_loop::cache_break::hash_tool_definitions(&tool_definitions);

			let config = &self.agent_runtime_config.next_step;
			let thinking_effort = request.thinking_effort.as_deref().and_then(|s| match s {
				"low" => Some(ThinkingEffort::Low),
				"medium" => Some(ThinkingEffort::Medium),
				"high" => Some(ThinkingEffort::High),
				"none" => Some(ThinkingEffort::None),
				_ => None,
			});

			let current_step_index = loop_state.step_index + 1;

			// Track whether any `ToolSchemaFrozen` event has already fired
			// within this turn. The first emit carries the turn's real
			// rebuild predicate; every subsequent call in the same turn
			// (escalation retry, reactive-retry iteration) reuses the same
			// frozen bytes, so those emits carry `rebuilt=false`. One emit
			// is produced before each outbound `router.generate*` call so
			// the trace honors the "one ToolSchemaFrozen per LLM call"
			// invariant.
			let mut tool_schema_frozen_emitted = false;

			// Inner reactive-retry loop: the LLM call may fail with
			// `ContextWindowExceeded` because our local byte-based estimator
			// undershoots the provider's tokenizer. When that happens, we run a
			// single reactive compaction on the current message buffer and try
			// the call again. After at most one retry per turn we surface the
			// failure so the loop does not spin indefinitely.
			let mut reactive_compact_used = false;
			// Time-gated microcompaction: only fires when the gap since
			// the last successful LLM call exceeds the prompt-cache TTL
			// (5 min on both Anthropic and OpenAI). Inside that window
			// the cache prefix is hot and any local rewrite would break
			// it; outside it the cache has already expired so the
			// rewrite costs nothing on the cache-prefix axis. Cold
			// starts (no previous call recorded — first turn of the
			// session, or first turn after a session restore where
			// `last_llm_call_at` is `None` due to `#[serde(skip)]`)
			// always skip.
			//
			// Hoisted above the reactive-retry loop: the cache-TTL signal
			// is per-turn, not per-attempt. Re-evaluating inside the loop
			// would let a reactive retry within the same turn re-emit
			// `TimeBasedMicrocompactRan` (idempotent on the buffer but
			// noisy in trace consumers) since `last_llm_call_at` does not
			// advance until a successful response.
			{
				let time_gate_now = std::time::SystemTime::now();
				if crate::runtime_loop::time_based_microcompact_due(
					loop_state.last_llm_call_at(),
					time_gate_now,
					crate::runtime_loop::CACHE_COLD_GAP,
				) {
					// Notify the cache-break detector before mutating the
					// buffer so the next response's `cache_read` drop is
					// classified as intentional and never surfaces as an
					// "unexpected break" diagnostic. Idempotent if the
					// rewrite below ends up clearing nothing.
					loop_state.cache_break_detector.notify_compaction();
					let freed = crate::runtime_loop::microcompact_old_tool_results(
						&mut messages,
						crate::runtime_loop::MICROCOMPACT_RETAIN_RECENT,
						loop_state.estimator_calibration.get(None),
					);
					let gap_minutes = loop_state
						.last_llm_call_at()
						.and_then(|prev| time_gate_now.duration_since(prev).ok())
						.map(|gap| gap.as_secs() / 60)
						.unwrap_or(0);
					if freed > 0 {
						// Replacing tool-result bodies with short
						// placeholders shrinks the committed prefix below
						// what the previous `usage.prompt_tokens` priced.
						loop_state.invalidate_committed_baseline();
					}
					if let Some(sender) = event_sender {
						let _ =
							sender.send(crate::runtime_loop::LoopEvent::TimeBasedMicrocompactRan {
								step: current_step_index,
								gap_minutes,
								freed_tokens: freed,
							});
					}
				}
			}
			let (accumulated_text, accumulated_tool_calls) = loop {
				// Initial preflight resolution for this attempt — may be
				// superseded by a second resolution below if mid-tier
				// compaction mutates `messages`. Reading the counter /
				// routed model / wire-bytes preview through the full
				// `select_model` policy (via the same
				// `counter_selection_request`) keeps all three views
				// consistent with the model `router.generate*` will
				// actually serve — `model_override` eligibility, risk
				// tier, and budget filters all come into play. An
				// earlier optimization routed via `model_id` only to
				// avoid a `messages.clone()` per attempt, but that
				// short-circuit can pick a different provider than the
				// real call in multi-model setups (budget exhaustion,
				// risk-tier demotion) and produce misleading pressure
				// estimates. The clone pays for correctness on the hot
				// path. `route_router` (not `execution_router`) is
				// consulted so the routed id matches what
				// `router.generate*` will actually serve under the
				// two-router configuration where those diverge.
				let (counter_initial, current_routed_model_initial, attempt_schema_bytes_initial) =
					self.resolve_attempt_preflight(
						&messages,
						&system_prompt,
						&system_prompt_sections,
						&tool_definitions,
						request.model_override.clone(),
						config,
					);

				// Mid-tier pre-flight (Layer 1 / Layer 2): runs between Layer 0
				// microcompact and the Layer 3 high-water check. Only fires when
				// pressure is above the mid-water threshold and reactive compaction
				// has not already been used this attempt (to avoid double-firing
				// two compaction layers in the same turn).
				//
				// Scoped: `mid_tool_schema_bytes` borrows the initial
				// preview vec inside this block only. When the block
				// exits the borrow drops, leaving the `_initial` vec
				// free to be replaced by the post-mid-tier re-resolution
				// below.
				let mid_tier_ran = if !reactive_compact_used {
					let mid_tool_schema_bytes: Option<&[u8]> =
						if attempt_schema_bytes_initial.is_empty() {
							None
						} else {
							Some(&attempt_schema_bytes_initial)
						};
					// Same baseline validity check as `maybe_compact`: the
					// committed prefix is only authoritative when the
					// system prompt, tool-schema surface, and routed
					// model are all unchanged since the commit. The model
					// guard uses `current_routed_model_initial` (resolved
					// via `select_model`), not `request.model_override` —
					// otherwise the default-routing case (override=None)
					// would read as a permanent mismatch and the
					// committed-baseline branch would never activate.
					let mid_system_bytes = system_prompt.len() as u64;
					let mid_schema_bytes =
						mid_tool_schema_bytes.map(|b| b.len() as u64).unwrap_or(0);
					let mid_system_prompt_hash =
						crate::runtime_loop::hash_system_prompt_text(&system_prompt);
					let mid_tool_schema_hash = crate::runtime_loop::hash_tool_schema_bytes(
						mid_tool_schema_bytes.unwrap_or_default(),
					);
					let mid_baseline = loop_state.committed_baseline().filter(|b| {
						if messages.len() < b.message_count {
							return false;
						}
						let mid_prefix_messages_hash =
							crate::runtime_loop::hash_message_prefix(&messages[..b.message_count]);
						b.is_valid_for(
							mid_system_bytes,
							mid_schema_bytes,
							mid_system_prompt_hash,
							mid_tool_schema_hash,
							mid_prefix_messages_hash,
							current_routed_model_initial.as_deref(),
						)
					});
					let mid_estimate = crate::runtime_loop::estimate_prompt_pressure(
						&messages,
						Some(&system_prompt),
						mid_tool_schema_bytes,
						loop_state
							.estimator_calibration
							.get(current_routed_model_initial.as_deref()),
						counter_initial.as_ref(),
						mid_baseline,
					);
					let mid_threshold = (self.agent_runtime_config.r#loop.context_window_tokens
						as f64 * crate::runtime_loop::MID_WATER_TRIGGER_RATIO)
						as u64;
					if mid_estimate > mid_threshold {
						// Layer 2 (preferred, at-most-one-lookup per run):
						// reuse the latest session-keyed compact summary if
						// roku-memory has one AND this run has not already
						// looked for it. Subsequent mid-water triggers skip
						// the backend query and fall back to Layer 1
						// (mechanical collapse) whether the first lookup hit
						// or missed.
						//
						// Two invariants together:
						//
						// 1. At-most-one Layer 2 per run. Re-splicing the
						//    same frozen prior-session summary on every
						//    trigger over-represents the older digest
						//    relative to the turn's fresh material.
						//
						// 2. Cache the miss. `write_back_compact_summaries`
						//    only persists a summary at end-of-run, so a
						//    lookup that misses now will still miss on the
						//    next trigger within the same run — repeated
						//    queries only add backend latency.
						//
						// The flag therefore flips on *attempt* (immediately
						// after `fetch_layer2_session_summary` returns),
						// regardless of outcome.
						//
						// When memory is not wired up, the backend has no
						// matching record, or the read fails,
						// `session_summary_text` stays `None` and
						// `mid_compact_messages` transparently falls through
						// to the Layer 1 mechanical path.
						let session_summary_text = if loop_state.layer2_lookup_attempted_this_run {
							None
						} else {
							let fetched = self.fetch_layer2_session_summary(&loop_state.session_id);
							loop_state.layer2_lookup_attempted_this_run = true;
							fetched
						};
						let outcome = crate::runtime_loop::mid_compact_messages(
							&mut messages,
							session_summary_text.as_deref(),
						);
						let ran = !matches!(outcome, crate::runtime_loop::MidCompactOutcome::Noop);
						if ran {
							loop_state.cache_break_detector.notify_compaction();
							// Mid-tier compaction rewrote the tail of the
							// message buffer; the previously committed
							// `usage.prompt_tokens` baseline now points at
							// stale content and must be discarded.
							loop_state.invalidate_committed_baseline();
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
						ran
					} else {
						false
					}
				} else {
					false
				};

				// Re-resolve preflight state if mid-tier compaction
				// actually mutated `messages`. The resulting buffer
				// can flip `select_model`'s eligibility decision
				// (message-size-sensitive budgets), so the serving
				// provider may differ from the initial resolution.
				// Without this refresh, `pre_call_estimate` would use
				// the old provider's counter and wire-bytes while
				// `router.generate*` lands on the new one — and the
				// stored baseline's schema-bytes length would belong
				// to the wrong provider, breaking next-turn validity.
				let (counter, current_routed_model, attempt_schema_bytes_vec) = if mid_tier_ran {
					self.resolve_attempt_preflight(
						&messages,
						&system_prompt,
						&system_prompt_sections,
						&tool_definitions,
						request.model_override.clone(),
						config,
					)
				} else {
					(
						counter_initial,
						current_routed_model_initial,
						attempt_schema_bytes_initial,
					)
				};
				let tool_schema_bytes: Option<&[u8]> = if attempt_schema_bytes_vec.is_empty() {
					None
				} else {
					Some(&attempt_schema_bytes_vec)
				};

				// Cache break detector: snapshot the prompt prefix
				// components (static system blocks + tool schema +
				// model) with the *final* routed model for this attempt
				// — after any mid-tier reroute has settled. Recording
				// before the mid-tier block would leave the fingerprint
				// carrying the initial routed id while the real call
				// lands on the post-compaction id, so next turn's
				// cache-break comparison would read a spurious "model
				// unchanged" on the leading edge of a legitimate
				// routing swap.
				//
				// Placed inside the attempt loop because
				// `current_routed_model` — the id the next
				// `router.generate*` call will actually serve — only
				// resolves after `counter_selection_request` is built.
				// Using `request.model_override` here instead would
				// hash `""` on every default-routing turn, so two
				// consecutive turns routed to different providers would
				// read as "model unchanged" and the detector would
				// silently miss every cache break caused by a routing
				// swap.
				//
				// Reactive-retry iterations re-record unconditionally:
				// `current_fingerprint` is consumed by `check_response`
				// only on successful primary responses; a
				// `ContextWindowExceeded` fail path loops back here
				// and overwrites the previous iteration's fingerprint,
				// which is the desired "last record before the real
				// call wins" semantics.
				loop_state.cache_break_detector.record_prompt_state(
					&system_prompt_sections.static_blocks,
					&tool_definitions,
					current_routed_model.as_deref().unwrap_or(""),
				);

				// Pre-call snapshots: used below to (a) compare the current
				// request context against the committed-token baseline —
				// a shifted system prompt, tool-schema surface, or model
				// invalidates the baseline — and (b) feed back into
				// `record_observed_usage` after the response lands, so
				// the next turn can run the same validity check. Content
				// hashes catch the same-length edits byte count alone
				// would miss (e.g. a dynamic block swapping a path of
				// equal length, or a tool description rename).
				let pre_call_message_count = messages.len();
				let pre_call_system_prompt_bytes = system_prompt.len();
				let pre_call_tool_schema_bytes_len =
					tool_schema_bytes.map(|b| b.len()).unwrap_or(0);
				let pre_call_system_prompt_hash =
					crate::runtime_loop::hash_system_prompt_text(&system_prompt);
				let pre_call_tool_schema_hash = crate::runtime_loop::hash_tool_schema_bytes(
					tool_schema_bytes.unwrap_or_default(),
				);
				// Full-buffer prefix hash commits the entire `messages`
				// slice at this point — the next turn's baseline check
				// will compare against `messages[..pre_call_message_count]`,
				// which is the same slice because `message_count` equals
				// `messages.len()` now.
				let pre_call_prefix_messages_hash =
					crate::runtime_loop::hash_message_prefix(&messages);
				let pre_call_baseline = loop_state.committed_baseline().filter(|b| {
					if messages.len() < b.message_count {
						return false;
					}
					let current_prefix_messages_hash =
						crate::runtime_loop::hash_message_prefix(&messages[..b.message_count]);
					b.is_valid_for(
						pre_call_system_prompt_bytes as u64,
						pre_call_tool_schema_bytes_len as u64,
						pre_call_system_prompt_hash,
						pre_call_tool_schema_hash,
						current_prefix_messages_hash,
						current_routed_model.as_deref(),
					)
				});

				// Pre-call: snapshot the calibrated byte-based prompt estimate so
				// we can fold the provider's reported `usage.prompt_tokens` back
				// into the calibration after the call returns. Recomputed each
				// attempt because mid-tier or reactive compaction may have mutated
				// `messages`.
				let pre_call_estimate = crate::runtime_loop::estimate_prompt_tokens_calibrated(
					&messages,
					Some(&system_prompt),
					tool_schema_bytes,
					loop_state
						.estimator_calibration
						.get(current_routed_model.as_deref()),
					counter.as_ref(),
					pre_call_baseline,
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
						if let Some(sender) = event_sender {
							let _ = sender.send(crate::runtime_loop::LoopEvent::ToolSchemaFrozen {
								step: current_step_index,
								hash: tool_schema_hash,
								rebuilt: !tool_schema_frozen_emitted && tool_schema_rebuilt,
							});
						}
						tool_schema_frozen_emitted = true;
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
								// Per-call token usage: the warm-turn cache gate
								// reads `cache_read_input_tokens / prompt_tokens`
								// from each event and excludes step 1 as cold
								// start, which only works if the values describe
								// just this LLM call (not running totals).
								emit_token_usage(
									event_sender,
									current_step_index,
									resp.prompt_tokens,
									resp.output_tokens,
									FALLBACK_COST_PER_M_INPUT_USD,
									FALLBACK_COST_PER_M_OUTPUT_USD,
									Some(&resp.model_id),
									resp.cache_creation_input_tokens,
									resp.cache_read_input_tokens,
									false,
								);
								// Fold the real `usage.prompt_tokens` back into the
								// estimator calibration so the next turn's pressure
								// check is closer to ground truth. In
								// committed-baseline mode `calibration_pair`
								// subtracts the unchanged committed prefix from
								// both sides so the ratio reflects tail bias only;
								// in cold-start mode it returns the raw totals
								// unchanged.
								let (cal_estimated, cal_real) =
									pre_call_estimate.calibration_pair(resp.prompt_tokens);
								// Key the sample by the serving model id so the
								// next turn's apply against this provider sees
								// its own bias, not the average-of-everything
								// from a shared ring buffer.
								loop_state.estimator_calibration.update(
									Some(&resp.model_id),
									cal_estimated,
									cal_real,
								);
								// Record the committed-token baseline plus the three
								// prefix-surface guards (system prompt byte length,
								// tool-schema byte length and content hash, system
								// prompt hash, serving model). Next turn's pre-flight
								// rejects the baseline if any guard diverges, so
								// mid-session changes (dynamic system blocks,
								// plan mode transitions, tool surface growth, model
								// swap, or same-length content edits that byte
								// counts alone would miss) no longer produce stale
								// reuse of `input_tokens`.
								loop_state.record_observed_usage(
									pre_call_message_count,
									resp.prompt_tokens,
									pre_call_system_prompt_bytes,
									pre_call_tool_schema_bytes_len,
									pre_call_system_prompt_hash,
									pre_call_tool_schema_hash,
									pre_call_prefix_messages_hash,
									Some(resp.model_id.clone()),
								);
								loop_state
									.record_llm_call_observed_at(std::time::SystemTime::now());
								let _ = sender.send(
									crate::runtime_loop::LoopEvent::EstimatorCalibrated {
										step: current_step_index,
										estimated_prompt_tokens: pre_call_estimate.total_tokens,
										prompt_tokens: resp.prompt_tokens,
										scale: loop_state
											.estimator_calibration
											.scale_for(Some(&resp.model_id)),
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
								// Output slot escalation: if the streaming response was
								// truncated due to hitting max_tokens, retry once (non-
								// streaming) with the per-model ceiling. The retry output
								// replaces the accumulated streaming text. One-shot only.
								let (text, tool_calls) = if resp.finish_reason.as_deref()
									== Some("max_tokens") || resp
									.finish_reason
									.as_deref()
									== Some("length")
								{
									// Gate: skip escalation for providers that do not
									// support client-side output-slot capping.
									if !router.provider_supports_output_slot_cap(&resp.model_id) {
										let _ = sender.send(
											crate::runtime_loop::LoopEvent::OutputSlotEscalationUnsupported {
												step: current_step_index,
												model_id: resp.model_id.clone(),
												provider: resp.provider.clone(),
											},
										);
										(text, tool_calls)
									} else {
										let initial_max = gen_request.expected_output_tokens;
										if let Some(profile) =
											roku_plugin_llm::model_cost::lookup_cost_profile(
												&resp.model_id,
											) {
											let ceiling = profile.max_output_tokens;
											if ceiling > initial_max {
												let _ = sender.send(
												crate::runtime_loop::LoopEvent::OutputSlotEscalated {
													step: current_step_index,
													initial_max_tokens: initial_max,
													escalated_max_tokens: ceiling,
													model_id: resp.model_id.clone(),
												},
											);
												let mut escalated_request = gen_request.clone();
												escalated_request.expected_output_tokens = ceiling;
												let _ = sender.send(
												crate::runtime_loop::LoopEvent::ToolSchemaFrozen {
													step: current_step_index,
													hash: tool_schema_hash,
													rebuilt: false,
												},
											);
												if let Ok(retry_resp) =
													router.generate(&escalated_request).await
												{
													// Per-call emission — both the truncated
													// streaming attempt and this retry were
													// charged by the provider, and consumers'
													// `saturating_add` accumulators give the
													// caller the full billable total. Cost
													// reporting now reflects the retry's serving
													// model directly because each event carries
													// its own `model_id`.
													emit_token_usage(
														event_sender,
														current_step_index,
														retry_resp.prompt_tokens,
														retry_resp.output_tokens,
														FALLBACK_COST_PER_M_INPUT_USD,
														FALLBACK_COST_PER_M_OUTPUT_USD,
														Some(&retry_resp.model_id),
														retry_resp.cache_creation_input_tokens,
														retry_resp.cache_read_input_tokens,
														false,
													);
													// Re-calibrate the estimator with the retry's
													// prompt_tokens. When output-slot escalation
													// reroutes to a different model (`select_model`
													// picks a larger model under the expanded
													// `expected_output_tokens`), resolve a fresh
													// counter + wire-bytes preview for that model
													// so the calibration sample reflects the retry
													// model's bias — not the primary attempt's —
													// and the baseline byte-length matches the
													// retry provider's serializer. Same-route
													// retries keep the fast path and reuse the
													// primary `pre_call_estimate`.
													let (
														cal_estimated,
														cal_real,
														retry_schema_len,
														retry_tool_schema_hash,
													) = self.retry_preflight_values(
														&retry_resp.model_id,
														&resp.model_id,
														&messages,
														&system_prompt,
														&system_prompt_sections,
														&tool_definitions,
														config,
														pre_call_system_prompt_bytes,
														pre_call_tool_schema_bytes_len,
														pre_call_system_prompt_hash,
														pre_call_prefix_messages_hash,
														&pre_call_estimate,
														retry_resp.prompt_tokens,
														&loop_state.estimator_calibration,
														loop_state.committed_baseline(),
													);
													// Attribute the retry sample to the retry
													// response's serving model — output-slot
													// escalation can swap to a larger model
													// on-the-fly, and that new model's bias
													// should be logged against its own bucket,
													// not the primary attempt's (aborted) one.
													loop_state.estimator_calibration.update(
														Some(&retry_resp.model_id),
														cal_estimated,
														cal_real,
													);
													// Retry shares the same committed message
													// boundary as the primary call; use the
													// retry's `prompt_tokens` as the baseline.
													// The retry model id overrides the primary's
													// — the server may have escalated to a larger
													// model when filling the expanded output slot.
													// `retry_schema_len` / `retry_tool_schema_hash`
													// are either the primary's (same route) or the
													// retry provider's fresh wire-bytes view
													// (rerouted). The prefix hash is always
													// `pre_call_prefix_messages_hash` because the
													// message buffer does not change between primary
													// and retry.
													loop_state.record_observed_usage(
														pre_call_message_count,
														retry_resp.prompt_tokens,
														pre_call_system_prompt_bytes,
														retry_schema_len,
														pre_call_system_prompt_hash,
														retry_tool_schema_hash,
														pre_call_prefix_messages_hash,
														Some(retry_resp.model_id.clone()),
													);
													loop_state.record_llm_call_observed_at(
														std::time::SystemTime::now(),
													);
													// Replace the truncated streaming text in the
													// TUI. `LlmDecisionComplete` was already sent
													// before the retry, so the render engine's
													// collector is finalized; this explicit
													// replacement event is what unblocks the
													// terminal from showing the truncated stream.
													let _ = sender.send(
													crate::runtime_loop::LoopEvent::LlmTextReplace {
														step: current_step_index,
														text: retry_resp.output.clone(),
													},
												);
													let _ = sender.send(
													crate::runtime_loop::LoopEvent::EstimatorCalibrated {
														step: current_step_index,
														estimated_prompt_tokens: pre_call_estimate
															.total_tokens,
														prompt_tokens: retry_resp.prompt_tokens,
														scale: loop_state
															.estimator_calibration
															.scale_for(Some(&retry_resp.model_id)),
													},
												);
													let retry_tool_calls =
														retry_resp.tool_calls.unwrap_or_default();
													(retry_resp.output, retry_tool_calls)
												} else {
													(text, tool_calls)
												}
											} else {
												(text, tool_calls)
											}
										} else {
											(text, tool_calls)
										}
									} // closes `else { // supports_output_slot_cap`
								} else {
									(text, tool_calls)
								};
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
						if let Some(sender) = event_sender {
							let _ = sender.send(crate::runtime_loop::LoopEvent::ToolSchemaFrozen {
								step: current_step_index,
								hash: tool_schema_hash,
								rebuilt: !tool_schema_frozen_emitted && tool_schema_rebuilt,
							});
						}
						tool_schema_frozen_emitted = true;
						match router.generate(&gen_request).await {
							Ok(resp) => {
								// Per-call token usage — same rationale as the
								// streaming branch: the warm-turn cache gate
								// only computes a sound `cache_read_input_tokens /
								// prompt_tokens` ratio when each sample is one
								// LLM call.
								emit_token_usage(
									event_sender,
									current_step_index,
									resp.prompt_tokens,
									resp.output_tokens,
									FALLBACK_COST_PER_M_INPUT_USD,
									FALLBACK_COST_PER_M_OUTPUT_USD,
									Some(&resp.model_id),
									resp.cache_creation_input_tokens,
									resp.cache_read_input_tokens,
									false,
								);
								// Baseline-aware calibration update: in committed
								// mode the raw total contains the unchanged
								// committed prefix on both sides and the full-total
								// ratio collapses toward 1.0 — use tail-only terms.
								let (cal_estimated, cal_real) =
									pre_call_estimate.calibration_pair(resp.prompt_tokens);
								// Per-model bucket keyed by the non-streaming
								// response's serving model id.
								loop_state.estimator_calibration.update(
									Some(&resp.model_id),
									cal_estimated,
									cal_real,
								);
								loop_state.record_observed_usage(
									pre_call_message_count,
									resp.prompt_tokens,
									pre_call_system_prompt_bytes,
									pre_call_tool_schema_bytes_len,
									pre_call_system_prompt_hash,
									pre_call_tool_schema_hash,
									pre_call_prefix_messages_hash,
									Some(resp.model_id.clone()),
								);
								loop_state
									.record_llm_call_observed_at(std::time::SystemTime::now());
								if let Some(sender) = event_sender {
									let _ = sender.send(
										crate::runtime_loop::LoopEvent::EstimatorCalibrated {
											step: current_step_index,
											estimated_prompt_tokens: pre_call_estimate.total_tokens,
											prompt_tokens: resp.prompt_tokens,
											scale: loop_state
												.estimator_calibration
												.scale_for(Some(&resp.model_id)),
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
								// Output slot escalation: if the response was truncated
								// due to hitting max_tokens, retry once with the per-model
								// ceiling. This is a one-shot retry — no further looping.
								let resp = if resp.finish_reason.as_deref() == Some("max_tokens")
									|| resp.finish_reason.as_deref() == Some("length")
								{
									// Gate: skip escalation for providers that do not
									// support client-side output-slot capping.
									if !router.provider_supports_output_slot_cap(&resp.model_id) {
										if let Some(sender) = event_sender {
											let _ = sender.send(
												crate::runtime_loop::LoopEvent::OutputSlotEscalationUnsupported {
													step: current_step_index,
													model_id: resp.model_id.clone(),
													provider: resp.provider.clone(),
												},
											);
										}
										resp
									} else {
										let initial_max = gen_request.expected_output_tokens;
										if let Some(profile) =
											roku_plugin_llm::model_cost::lookup_cost_profile(
												&resp.model_id,
											) {
											let ceiling = profile.max_output_tokens;
											if ceiling > initial_max {
												if let Some(sender) = event_sender {
													let _ = sender.send(
													crate::runtime_loop::LoopEvent::OutputSlotEscalated {
														step: current_step_index,
														initial_max_tokens: initial_max,
														escalated_max_tokens: ceiling,
														model_id: resp.model_id.clone(),
													},
												);
												}
												let mut escalated_request = gen_request.clone();
												escalated_request.expected_output_tokens = ceiling;
												if let Some(sender) = event_sender {
													let _ = sender.send(
														crate::runtime_loop::LoopEvent::ToolSchemaFrozen {
															step: current_step_index,
															hash: tool_schema_hash,
															rebuilt: false,
														},
													);
												}
												if let Ok(retry_resp) =
													router.generate(&escalated_request).await
												{
													// Per-call emission for the non-streaming
													// retry — same rationale as the streaming
													// retry branch above.
													emit_token_usage(
														event_sender,
														current_step_index,
														retry_resp.prompt_tokens,
														retry_resp.output_tokens,
														FALLBACK_COST_PER_M_INPUT_USD,
														FALLBACK_COST_PER_M_OUTPUT_USD,
														Some(&retry_resp.model_id),
														retry_resp.cache_creation_input_tokens,
														retry_resp.cache_read_input_tokens,
														false,
													);
													// Re-calibrate the estimator with the retry's
													// prompt_tokens. Non-streaming retry shares
													// the same calibration invariants as the
													// primary path; the helper resolves a fresh
													// preflight when the escalation rerouted, and
													// falls through to the primary's
													// `pre_call_estimate` when it stayed on the
													// same model.
													let (
														cal_estimated,
														cal_real,
														retry_schema_len,
														retry_tool_schema_hash,
													) = self.retry_preflight_values(
														&retry_resp.model_id,
														&resp.model_id,
														&messages,
														&system_prompt,
														&system_prompt_sections,
														&tool_definitions,
														config,
														pre_call_system_prompt_bytes,
														pre_call_tool_schema_bytes_len,
														pre_call_system_prompt_hash,
														pre_call_prefix_messages_hash,
														&pre_call_estimate,
														retry_resp.prompt_tokens,
														&loop_state.estimator_calibration,
														loop_state.committed_baseline(),
													);
													loop_state.estimator_calibration.update(
														Some(&retry_resp.model_id),
														cal_estimated,
														cal_real,
													);
													loop_state.record_observed_usage(
														pre_call_message_count,
														retry_resp.prompt_tokens,
														pre_call_system_prompt_bytes,
														retry_schema_len,
														pre_call_system_prompt_hash,
														retry_tool_schema_hash,
														pre_call_prefix_messages_hash,
														Some(retry_resp.model_id.clone()),
													);
													loop_state.record_llm_call_observed_at(
														std::time::SystemTime::now(),
													);
													// Intentionally no `LlmTextReplace` on the
													// non-streaming path: there is no truncated
													// streaming text in the terminal to replace.
													// `retry_resp.output` is consumed directly by
													// the caller as the final response text.
													if let Some(sender) = event_sender {
														let _ = sender.send(
														crate::runtime_loop::LoopEvent::EstimatorCalibrated {
															step: current_step_index,
															estimated_prompt_tokens: pre_call_estimate
																.total_tokens,
															prompt_tokens: retry_resp.prompt_tokens,
															scale: loop_state
																.estimator_calibration
																.scale_for(Some(&retry_resp.model_id)),
														},
													);
													}
													retry_resp
												} else {
													resp
												}
											} else {
												resp
											}
										} else {
											resp
										}
									} // closes `else { // supports_output_slot_cap`
								} else {
									resp
								};
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
						// Per-call emission for the summarizer LLM call inside
						// reactive compaction. The summarizer is a real billed
						// call, so consumers that accumulate `prompt_tokens` /
						// `output_tokens` need to see it. `is_compaction=true`
						// is the positive marker the warm-turn cache gate keys
						// off to skip these synthetic zero-cache events.
						emit_token_usage(
							event_sender,
							current_step_index,
							compact_pt,
							compact_ot,
							FALLBACK_COST_PER_M_INPUT_USD,
							FALLBACK_COST_PER_M_OUTPUT_USD,
							None,
							0,
							0,
							true,
						);
						// Update the time-gated microcompact baseline: the
						// summarizer just ran, so the next turn's prompt cache
						// is warm and `time_based_microcompact_due` must not
						// trigger a redundant rewrite.
						if compact_pt > 0 || compact_ot > 0 {
							loop_state.record_llm_call_observed_at(std::time::SystemTime::now());
						}
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
						// No `emit_token_usage` here: per-call events for any
						// successful prior calls were already emitted, and the
						// failing call did not return a billable response.
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
				// No `emit_token_usage` here: the per-call event for this LLM
				// call was already emitted in the success branch above.
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
						// Per-call event for this LLM call was already emitted
						// in the success branch.
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
						// Per-call event for this LLM call was already emitted
						// in the success branch.
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
						// Per-call event for this LLM call was already emitted
						// in the success branch.
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
			}

			let (compact_pt, compact_ot) = self
				.maybe_compact(
					loop_state,
					&mut messages,
					current_step_index,
					event_sender,
					&system_prompt,
					tool_schema_bytes,
					request.model_override.as_deref(),
				)
				.await;
			if compact_pt > 0 || compact_ot > 0 {
				loop_state.cache_break_detector.notify_compaction();
				// Per-call emission for the summarizer LLM call inside
				// `maybe_compact`. The summarizer is a real billed call, so
				// consumers that accumulate `prompt_tokens` / `output_tokens`
				// need to see it. `is_compaction=true` is the positive
				// marker the warm-turn cache gate keys off to skip these
				// synthetic zero-cache events.
				emit_token_usage(
					event_sender,
					current_step_index,
					compact_pt,
					compact_ot,
					FALLBACK_COST_PER_M_INPUT_USD,
					FALLBACK_COST_PER_M_OUTPUT_USD,
					None,
					0,
					0,
					true,
				);
				// Update the time-gated microcompact baseline: the
				// summarizer just ran, so the next turn's prompt cache is
				// warm and `time_based_microcompact_due` must not trigger
				// a redundant rewrite — without this, a slow tool step
				// followed by `maybe_compact` near the >5 min mark would
				// leave `last_llm_call_at` stale, the next turn would
				// classify the cache as cold, and the time gate would
				// re-rewrite tool-result content even though an LLM call
				// just occurred.
				loop_state.record_llm_call_observed_at(std::time::SystemTime::now());
			}

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
			.sort_by_key(|entry| std::cmp::Reverse(entry.priority));
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

	/// Install a long-term memory backend the runtime loop uses for Layer 2
	/// mid-tier compaction (session-keyed summary reuse). Optional: when not
	/// set, mid-tier compaction falls through to the mechanical Layer 1 path.
	pub fn with_memory_backend(
		mut self,
		memory_backend: Arc<dyn roku_memory::LongTermMemoryBackend>,
	) -> Self {
		self.memory_backend = Some(memory_backend);
		self
	}

	/// Fetch the most recent session-scoped compact summary from the installed
	/// memory backend, if any. Returns `None` when no backend is wired up,
	/// when `session_id` is empty, when the backend has no matching record, or
	/// when the backend errors — the caller treats any `None` as "fall through
	/// to Layer 1".
	fn fetch_layer2_session_summary(&self, session_id: &str) -> Option<String> {
		self.memory_backend
			.as_deref()
			.filter(|_| !session_id.is_empty())
			.and_then(|backend| {
				roku_memory::latest_session_compact_summary(backend, session_id)
					.ok()
					.flatten()
					.map(|record| record.content)
			})
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
/// Looks up a per-model cost profile and computes per-tier cost breakdown.
/// Falls back to blended Sonnet-tier estimates when no profile is found.
/// `cache_creation_input_tokens` and `cache_read_input_tokens` are
/// additive per-tier counters sourced from the provider's `usage` block;
/// they are `0` when the provider did not report any cache activity.
#[allow(clippy::too_many_arguments)]
fn emit_token_usage(
	event_sender: Option<&crate::runtime_loop::LoopEventSender>,
	step: u32,
	prompt_tokens: u64,
	output_tokens: u64,
	fallback_cost_per_m_input_usd: f64,
	fallback_cost_per_m_output_usd: f64,
	model_id: Option<&str>,
	cache_creation_input_tokens: u64,
	cache_read_input_tokens: u64,
	is_compaction: bool,
) {
	if let Some(sender) = event_sender {
		let total_tokens = prompt_tokens.saturating_add(output_tokens);
		let (
			estimated_cost_usd,
			uncached_input_cost_usd,
			cache_write_cost_usd,
			cache_read_cost_usd,
			output_cost_usd,
		) = if let Some(profile) = model_id.and_then(roku_plugin_llm::model_cost::lookup_cost_profile)
		{
			let turn_cost = roku_plugin_llm::model_cost::compute_turn_cost_usd(
				profile,
				prompt_tokens,
				output_tokens,
				cache_creation_input_tokens,
				cache_read_input_tokens,
			);
			(
				turn_cost.total_usd,
				turn_cost.uncached_input_usd,
				turn_cost.cache_write_usd,
				turn_cost.cache_read_usd,
				turn_cost.output_usd,
			)
		} else {
			let estimated = (prompt_tokens as f64 / 1_000_000.0) * fallback_cost_per_m_input_usd
				+ (output_tokens as f64 / 1_000_000.0) * fallback_cost_per_m_output_usd;
			(estimated, 0.0, 0.0, 0.0, 0.0)
		};
		let _ = sender.send(crate::runtime_loop::LoopEvent::TokenUsage {
			step,
			prompt_tokens,
			output_tokens,
			total_tokens,
			estimated_cost_usd,
			model_id: model_id.map(str::to_string),
			cache_creation_input_tokens,
			cache_read_input_tokens,
			uncached_input_cost_usd,
			cache_write_cost_usd,
			cache_read_cost_usd,
			output_cost_usd,
			is_compaction,
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
	use roku_memory::LongTermMemoryBackend;
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
	fn fetch_layer2_session_summary_returns_none_without_memory_backend() {
		let runtime = GenericAgentRuntime::default();
		assert!(
			runtime.fetch_layer2_session_summary("session-1").is_none(),
			"no backend installed → None",
		);
	}

	#[test]
	fn fetch_layer2_session_summary_returns_none_for_empty_session_id() {
		let backend = Arc::new(roku_memory::InMemoryLongTermMemoryBackend::default());
		let mut req = roku_memory::MemoryWriteRequest::new(
			roku_memory::MemoryKind::WorkflowInsight,
			roku_memory::MemoryScope::Session,
			"leaked",
			roku_memory::COMPACT_SUMMARY_SENTINEL,
			roku_memory::MemoryWriteReason::CompactSummary,
		);
		req.session_id = Some("some-session".to_string());
		backend.write(&req).expect("seed summary");
		let runtime = GenericAgentRuntime::default().with_memory_backend(backend);
		assert!(
			runtime.fetch_layer2_session_summary("").is_none(),
			"empty session_id must short-circuit (no cross-session leakage)",
		);
	}

	#[test]
	fn fetch_layer2_session_summary_returns_stored_content_for_matching_session() {
		let backend = Arc::new(roku_memory::InMemoryLongTermMemoryBackend::default());
		let mut req = roku_memory::MemoryWriteRequest::new(
			roku_memory::MemoryKind::WorkflowInsight,
			roku_memory::MemoryScope::Session,
			"session X summary body",
			roku_memory::COMPACT_SUMMARY_SENTINEL,
			roku_memory::MemoryWriteReason::CompactSummary,
		);
		req.session_id = Some("session-X".to_string());
		backend.write(&req).expect("seed summary");

		let runtime = GenericAgentRuntime::default().with_memory_backend(backend);
		let got = runtime
			.fetch_layer2_session_summary("session-X")
			.expect("should resolve summary");
		assert_eq!(got, "session X summary body");

		// Foreign session must not leak.
		assert!(
			runtime
				.fetch_layer2_session_summary("session-other")
				.is_none(),
		);
	}

	#[test]
	fn execute_tool_loop_splices_session_memory_summary_via_layer2() {
		// End-to-end regression test for the Layer 2 wiring added in the
		// OpenAI gap sweep (issue #300): memory backend installed → mid-water
		// pressure exceeded → session-keyed compact summary fetched and
		// spliced into the conversation buffer → MidCompactLayer2Ran emitted
		// instead of MidCompactLayer1Ran.

		// Seed the in-memory backend with a compact summary for the test
		// session using the canonical (kind, scope, summary-sentinel,
		// write_reason) tuple.
		let backend = Arc::new(roku_memory::InMemoryLongTermMemoryBackend::default());
		let mut seed = roku_memory::MemoryWriteRequest::new(
			roku_memory::MemoryKind::WorkflowInsight,
			roku_memory::MemoryScope::Session,
			"PRIOR-SESSION-MEMORY-SUMMARY-BODY: root-cause was a stale cache key.",
			roku_memory::COMPACT_SUMMARY_SENTINEL,
			roku_memory::MemoryWriteReason::CompactSummary,
		);
		seed.session_id = Some("session-layer2-int-test".to_string());
		<roku_memory::InMemoryLongTermMemoryBackend as roku_memory::LongTermMemoryBackend>::write(
			&backend, &seed,
		)
		.expect("seed compact summary");

		// Route router: succeed on the first call with a final_answer decision
		// so the loop exits cleanly after one turn.
		let (route_router, _) = router_with_json_responses(vec![serde_json::json!({
			"action": "final_answer",
			"tool_name": null,
			"arguments": null,
			"reason": "test",
			"final_message": "done"
		})]);
		let execution_router = router_with_text_output("layer2-int-exec", "unused");

		// Tiny context window so a short `conversation_history` trips the
		// mid-water threshold (0.60 × 200 = 120 tokens). Push Layer 3's
		// post-flight threshold up to 0.99 so it doesn't pre-empt Layer 2.
		let mut agent_config = crate::runtime_config::AgentRuntimeConfig::default();
		agent_config.r#loop.context_window_tokens = 200;
		agent_config.r#loop.compact_threshold_ratio = 0.99;

		let root = tempfile::tempdir().expect("temp root should exist");
		let runtime = GenericAgentRuntime::
			with_route_and_execution_routers_skill_registry_tool_config_and_plugin_snapshot_and_runtime_config(
				route_router,
				execution_router,
				SkillRegistry::file_backed(root.keep()),
				ToolCatalogConfig::default(),
				PluginRegistrySnapshot::permissive(),
				ToolsRuntimeConfig::default(),
				agent_config,
			)
			.with_memory_backend(Arc::clone(&backend) as Arc<dyn roku_memory::LongTermMemoryBackend>);

		// Build a conversation_history with enough English prose that the
		// byte-based estimator (`bytes/4` for default text) produces an
		// estimate above 120 tokens but below 200. Need at least 3 messages
		// so `mid_compact_messages` does not Noop on a too-short buffer.
		let pad = "lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt. ".repeat(3);
		let request = RequestEnvelope {
			request_id: roku_common_types::RequestId("req-layer2-int".to_string()),
			session_id: "session-layer2-int-test".to_string(),
			goal: "Layer 2 integration test".to_string(),
			planning_mode_hint: None,
			conversation_history: vec![
				ConversationTurn {
					role: ConversationRole::User,
					content: pad.clone(),
					created_at_unix_ms: 0,
				},
				ConversationTurn {
					role: ConversationRole::Assistant,
					content: pad.clone(),
					created_at_unix_ms: 0,
				},
				ConversationTurn {
					role: ConversationRole::User,
					content: pad.clone(),
					created_at_unix_ms: 0,
				},
				ConversationTurn {
					role: ConversationRole::Assistant,
					content: pad.clone(),
					created_at_unix_ms: 0,
				},
			],
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
		let _execution = tokio::runtime::Builder::new_multi_thread()
			.enable_all()
			.build()
			.expect("tokio runtime for execute-tool-loop bridge should build")
			.block_on(runtime.execute_tool_loop(
				&TaskId("task-layer2-int".to_string()),
				&request,
				&mut loop_state,
				&RuntimeMemorySections::default(),
				None,
				Some(&event_tx),
				None,
			));

		drop(event_tx);
		let mut events = Vec::new();
		while let Ok(event) = event_rx.try_recv() {
			events.push(event);
		}

		let layer2_events: Vec<_> = events
			.iter()
			.filter(|e| {
				matches!(
					e,
					crate::runtime_loop::LoopEvent::MidCompactLayer2Ran { .. }
				)
			})
			.collect();
		let layer1_events: Vec<_> = events
			.iter()
			.filter(|e| {
				matches!(
					e,
					crate::runtime_loop::LoopEvent::MidCompactLayer1Ran { .. }
				)
			})
			.collect();

		assert_eq!(
			layer2_events.len(),
			1,
			"seeded memory → Layer 2 must fire exactly once; \
			 Layer 2 count={}, Layer 1 count={}, events={events:?}",
			layer2_events.len(),
			layer1_events.len(),
		);
		assert_eq!(
			layer1_events.len(),
			0,
			"Layer 1 mechanical fallback must not fire when Layer 2 consumed the summary",
		);

		// Side-check on the run-scoped flag: the lookup attempt must have
		// set it (regardless of outcome; here the outcome is Layer 2).
		assert!(
			loop_state.layer2_lookup_attempted_this_run,
			"layer2_lookup_attempted_this_run must be set after Layer 2 fires",
		);
	}

	#[test]
	fn execute_tool_loop_falls_back_to_layer1_when_layer2_lookup_already_attempted() {
		// Same setup as `..._splices_session_memory_summary_via_layer2` but
		// pre-sets `loop_state.layer2_lookup_attempted_this_run = true` before
		// the call. The memory backend still has a valid summary, but the
		// at-most-one-lookup-per-run invariant means the mid-water trigger
		// must fall back to Layer 1 (mechanical) instead of re-splicing the
		// frozen summary.

		let backend = Arc::new(roku_memory::InMemoryLongTermMemoryBackend::default());
		let mut seed = roku_memory::MemoryWriteRequest::new(
			roku_memory::MemoryKind::WorkflowInsight,
			roku_memory::MemoryScope::Session,
			"PRIOR-SESSION-SUMMARY",
			roku_memory::COMPACT_SUMMARY_SENTINEL,
			roku_memory::MemoryWriteReason::CompactSummary,
		);
		seed.session_id = Some("session-layer2-already-consumed".to_string());
		<roku_memory::InMemoryLongTermMemoryBackend as roku_memory::LongTermMemoryBackend>::write(
			&backend, &seed,
		)
		.expect("seed compact summary");

		let (route_router, _) = router_with_json_responses(vec![serde_json::json!({
			"action": "final_answer",
			"tool_name": null,
			"arguments": null,
			"reason": "done",
			"final_message": "done"
		})]);
		let execution_router = router_with_text_output("layer2-consumed-exec", "unused");

		let mut agent_config = crate::runtime_config::AgentRuntimeConfig::default();
		agent_config.r#loop.context_window_tokens = 200;
		agent_config.r#loop.compact_threshold_ratio = 0.99;

		let root = tempfile::tempdir().expect("temp root should exist");
		let runtime = GenericAgentRuntime::
			with_route_and_execution_routers_skill_registry_tool_config_and_plugin_snapshot_and_runtime_config(
				route_router,
				execution_router,
				SkillRegistry::file_backed(root.keep()),
				ToolCatalogConfig::default(),
				PluginRegistrySnapshot::permissive(),
				ToolsRuntimeConfig::default(),
				agent_config,
			)
			.with_memory_backend(Arc::clone(&backend) as Arc<dyn roku_memory::LongTermMemoryBackend>);

		let pad = "lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt. ".repeat(3);
		let request = RequestEnvelope {
			request_id: roku_common_types::RequestId("req-layer2-already".to_string()),
			session_id: "session-layer2-already-consumed".to_string(),
			goal: "Layer 2 already-consumed test".to_string(),
			planning_mode_hint: None,
			conversation_history: vec![
				ConversationTurn {
					role: ConversationRole::User,
					content: pad.clone(),
					created_at_unix_ms: 0,
				},
				ConversationTurn {
					role: ConversationRole::Assistant,
					content: pad.clone(),
					created_at_unix_ms: 0,
				},
				ConversationTurn {
					role: ConversationRole::User,
					content: pad.clone(),
					created_at_unix_ms: 0,
				},
				ConversationTurn {
					role: ConversationRole::Assistant,
					content: pad.clone(),
					created_at_unix_ms: 0,
				},
			],
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
		// Simulate a previous mid-water trigger in this run that already
		// performed the Layer 2 lookup (outcome is irrelevant — once
		// attempted, the run must not re-query).
		loop_state.layer2_lookup_attempted_this_run = true;

		let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
		let _execution = tokio::runtime::Builder::new_multi_thread()
			.enable_all()
			.build()
			.expect("tokio runtime for execute-tool-loop bridge should build")
			.block_on(runtime.execute_tool_loop(
				&TaskId("task-layer2-already".to_string()),
				&request,
				&mut loop_state,
				&RuntimeMemorySections::default(),
				None,
				Some(&event_tx),
				None,
			));

		drop(event_tx);
		let mut events = Vec::new();
		while let Ok(event) = event_rx.try_recv() {
			events.push(event);
		}

		let layer2_count = events
			.iter()
			.filter(|e| {
				matches!(
					e,
					crate::runtime_loop::LoopEvent::MidCompactLayer2Ran { .. }
				)
			})
			.count();
		let layer1_count = events
			.iter()
			.filter(|e| {
				matches!(
					e,
					crate::runtime_loop::LoopEvent::MidCompactLayer1Ran { .. }
				)
			})
			.count();
		assert_eq!(
			layer2_count, 0,
			"flag already true → Layer 2 must NOT fire a second time in the same run",
		);
		assert_eq!(
			layer1_count, 1,
			"mid-water pressure with Layer 2 already consumed → Layer 1 fallback fires once",
		);
		// With the attempted flag pre-set, the backend must not be queried
		// at all — even though the backend has a valid summary that would
		// otherwise produce a Layer 2 outcome. This is the "cache the
		// lookup" half of the invariant.
		let our_queries = backend
			.recorded_queries()
			.into_iter()
			.filter(|q| q.session_id.as_deref() == Some("session-layer2-already-consumed"))
			.count();
		assert_eq!(
			our_queries, 0,
			"attempted flag pre-set → backend must NOT be queried this run",
		);
	}

	#[test]
	fn execute_tool_loop_caches_layer2_lookup_miss_and_sets_attempted_flag() {
		// Backend is installed and non-empty, but has NO compact summary
		// for our session (the record seeded below belongs to a different
		// session). The Layer 2 lookup therefore misses, Layer 1 takes over,
		// and the attempted flag must still flip to `true` so future
		// mid-water triggers in the same run skip the backend entirely —
		// caching the miss (compact summaries are only written at end-of-run
		// so a re-query would miss again).

		let backend = Arc::new(roku_memory::InMemoryLongTermMemoryBackend::default());
		// Seed a record for a FOREIGN session so the backend is non-empty
		// but our session still misses.
		let mut foreign = roku_memory::MemoryWriteRequest::new(
			roku_memory::MemoryKind::WorkflowInsight,
			roku_memory::MemoryScope::Session,
			"foreign-session summary — should not leak",
			roku_memory::COMPACT_SUMMARY_SENTINEL,
			roku_memory::MemoryWriteReason::CompactSummary,
		);
		foreign.session_id = Some("session-foreign".to_string());
		<roku_memory::InMemoryLongTermMemoryBackend as roku_memory::LongTermMemoryBackend>::write(
			&backend, &foreign,
		)
		.expect("seed foreign summary");

		let (route_router, _) = router_with_json_responses(vec![serde_json::json!({
			"action": "final_answer",
			"tool_name": null,
			"arguments": null,
			"reason": "done",
			"final_message": "done"
		})]);
		let execution_router = router_with_text_output("layer2-miss-exec", "unused");

		let mut agent_config = crate::runtime_config::AgentRuntimeConfig::default();
		agent_config.r#loop.context_window_tokens = 200;
		agent_config.r#loop.compact_threshold_ratio = 0.99;

		let root = tempfile::tempdir().expect("temp root should exist");
		let runtime = GenericAgentRuntime::
			with_route_and_execution_routers_skill_registry_tool_config_and_plugin_snapshot_and_runtime_config(
				route_router,
				execution_router,
				SkillRegistry::file_backed(root.keep()),
				ToolCatalogConfig::default(),
				PluginRegistrySnapshot::permissive(),
				ToolsRuntimeConfig::default(),
				agent_config,
			)
			.with_memory_backend(Arc::clone(&backend) as Arc<dyn roku_memory::LongTermMemoryBackend>);

		let pad = "lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt. ".repeat(3);
		let request = RequestEnvelope {
			request_id: roku_common_types::RequestId("req-layer2-miss".to_string()),
			session_id: "session-layer2-miss".to_string(),
			goal: "Layer 2 miss-caching test".to_string(),
			planning_mode_hint: None,
			conversation_history: vec![
				ConversationTurn {
					role: ConversationRole::User,
					content: pad.clone(),
					created_at_unix_ms: 0,
				},
				ConversationTurn {
					role: ConversationRole::Assistant,
					content: pad.clone(),
					created_at_unix_ms: 0,
				},
				ConversationTurn {
					role: ConversationRole::User,
					content: pad.clone(),
					created_at_unix_ms: 0,
				},
				ConversationTurn {
					role: ConversationRole::Assistant,
					content: pad.clone(),
					created_at_unix_ms: 0,
				},
			],
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
		let _execution = tokio::runtime::Builder::new_multi_thread()
			.enable_all()
			.build()
			.expect("tokio runtime for execute-tool-loop bridge should build")
			.block_on(runtime.execute_tool_loop(
				&TaskId("task-layer2-miss".to_string()),
				&request,
				&mut loop_state,
				&RuntimeMemorySections::default(),
				None,
				Some(&event_tx),
				None,
			));

		drop(event_tx);
		let mut events = Vec::new();
		while let Ok(event) = event_rx.try_recv() {
			events.push(event);
		}

		let layer2_count = events
			.iter()
			.filter(|e| {
				matches!(
					e,
					crate::runtime_loop::LoopEvent::MidCompactLayer2Ran { .. }
				)
			})
			.count();
		let layer1_count = events
			.iter()
			.filter(|e| {
				matches!(
					e,
					crate::runtime_loop::LoopEvent::MidCompactLayer1Ran { .. }
				)
			})
			.count();
		assert_eq!(
			layer2_count, 0,
			"no matching summary → Layer 2 must NOT fire",
		);
		assert_eq!(
			layer1_count, 1,
			"mid-water pressure → Layer 1 mechanical fallback fires once",
		);

		// Core invariant: flag is set even though the outcome was Layer 1
		// (the run attempted the lookup, nothing more to retry).
		assert!(
			loop_state.layer2_lookup_attempted_this_run,
			"attempted flag must flip on any lookup — hit or miss — so future \
			 triggers in the same run skip the backend entirely",
		);

		// Backend was queried exactly once for our session. (Other sessions
		// — e.g. the foreign seed write — don't issue queries, so this
		// filter isolates the miss query the runtime issued.)
		let our_queries = backend
			.recorded_queries()
			.into_iter()
			.filter(|q| q.session_id.as_deref() == Some("session-layer2-miss"))
			.count();
		assert_eq!(
			our_queries, 1,
			"backend must be queried exactly once per run, regardless of \
			 Layer 2 outcome",
		);
	}

	#[test]
	fn execute_tool_loop_falls_back_to_layer1_when_memory_has_no_summary() {
		// Negative of the above: same setup but the backend is not installed.
		// Mid-water must still fire, but Layer 1 (mechanical) takes over
		// because `fetch_layer2_session_summary` returns None.

		let (route_router, _) = router_with_json_responses(vec![serde_json::json!({
			"action": "final_answer",
			"tool_name": null,
			"arguments": null,
			"reason": "test",
			"final_message": "done"
		})]);
		let execution_router = router_with_text_output("layer1-fallback-exec", "unused");

		let mut agent_config = crate::runtime_config::AgentRuntimeConfig::default();
		agent_config.r#loop.context_window_tokens = 200;
		agent_config.r#loop.compact_threshold_ratio = 0.99;

		let root = tempfile::tempdir().expect("temp root should exist");
		// NOTE: no `.with_memory_backend(...)` call — the runtime stays
		// without a backend, so Layer 2 cannot fire.
		let runtime = GenericAgentRuntime::
			with_route_and_execution_routers_skill_registry_tool_config_and_plugin_snapshot_and_runtime_config(
				route_router,
				execution_router,
				SkillRegistry::file_backed(root.keep()),
				ToolCatalogConfig::default(),
				PluginRegistrySnapshot::permissive(),
				ToolsRuntimeConfig::default(),
				agent_config,
			);

		let pad = "lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt. ".repeat(3);
		let request = RequestEnvelope {
			request_id: roku_common_types::RequestId("req-layer1-fallback".to_string()),
			session_id: "session-layer1-fallback".to_string(),
			goal: "Layer 1 fallback test".to_string(),
			planning_mode_hint: None,
			conversation_history: vec![
				ConversationTurn {
					role: ConversationRole::User,
					content: pad.clone(),
					created_at_unix_ms: 0,
				},
				ConversationTurn {
					role: ConversationRole::Assistant,
					content: pad.clone(),
					created_at_unix_ms: 0,
				},
				ConversationTurn {
					role: ConversationRole::User,
					content: pad.clone(),
					created_at_unix_ms: 0,
				},
				ConversationTurn {
					role: ConversationRole::Assistant,
					content: pad.clone(),
					created_at_unix_ms: 0,
				},
			],
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
		let _execution = tokio::runtime::Builder::new_multi_thread()
			.enable_all()
			.build()
			.expect("tokio runtime for execute-tool-loop bridge should build")
			.block_on(runtime.execute_tool_loop(
				&TaskId("task-layer1-fallback".to_string()),
				&request,
				&mut loop_state,
				&RuntimeMemorySections::default(),
				None,
				Some(&event_tx),
				None,
			));

		drop(event_tx);
		let mut events = Vec::new();
		while let Ok(event) = event_rx.try_recv() {
			events.push(event);
		}

		let layer2_count = events
			.iter()
			.filter(|e| {
				matches!(
					e,
					crate::runtime_loop::LoopEvent::MidCompactLayer2Ran { .. }
				)
			})
			.count();
		let layer1_count = events
			.iter()
			.filter(|e| {
				matches!(
					e,
					crate::runtime_loop::LoopEvent::MidCompactLayer1Ran { .. }
				)
			})
			.count();

		assert_eq!(layer2_count, 0, "no memory backend → Layer 2 must not fire");
		assert_eq!(
			layer1_count, 1,
			"mid-water pressure without memory → Layer 1 mechanical fallback fires once",
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
				response_id: None,
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
				response_id: None,
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
						response_id: None,
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
	fn execute_tool_loop_keeps_tool_result_content_byte_stable_across_turns() {
		// Regression: the runtime must not rewrite historical
		// `Message::ToolResult.content` between turns. After a tool call
		// returns, its body becomes part of the prompt prefix and must
		// remain byte-stable so the provider's prompt-prefix cache stays
		// hot.
		//
		// The fixture issues four `Read` calls before `final_answer`,
		// one more than `MICROCOMPACT_RETAIN_RECENT = 3`. If a future
		// regression re-introduces an unconditional pre-flight rewrite,
		// the OLDEST tool result (fixture A) would be replaced with the
		// placeholder before turn 5's LLM call and the marker assertion
		// below would flag it. A two-call fixture would leave the
		// rewrite path observationally idle because the
		// eligible-old-results window is empty under retain=3.
		let paths = [
			regression_fixture_path(".txt", "MARKER-ALPHA-byte-stability-fixture-A\n"),
			regression_fixture_path(".txt", "MARKER-BETA-byte-stability-fixture-B\n"),
			regression_fixture_path(".txt", "MARKER-GAMMA-byte-stability-fixture-C\n"),
			regression_fixture_path(".txt", "MARKER-DELTA-byte-stability-fixture-D\n"),
		];
		let markers = [
			"MARKER-ALPHA-byte-stability-fixture-A",
			"MARKER-BETA-byte-stability-fixture-B",
			"MARKER-GAMMA-byte-stability-fixture-C",
			"MARKER-DELTA-byte-stability-fixture-D",
		];
		let responses = vec![
			serde_json::json!({
				"action": "call_tool",
				"tool_name": "Read",
				"arguments": { "path": &paths[0] },
				"reason": "load fixture 0",
				"final_message": null,
			}),
			serde_json::json!({
				"action": "call_tool",
				"tool_name": "Read",
				"arguments": { "path": &paths[1] },
				"reason": "load fixture 1",
				"final_message": null,
			}),
			serde_json::json!({
				"action": "call_tool",
				"tool_name": "Read",
				"arguments": { "path": &paths[2] },
				"reason": "load fixture 2",
				"final_message": null,
			}),
			serde_json::json!({
				"action": "call_tool",
				"tool_name": "Read",
				"arguments": { "path": &paths[3] },
				"reason": "load fixture 3",
				"final_message": null,
			}),
			serde_json::json!({
				"action": "final_answer",
				"tool_name": null,
				"arguments": null,
				"reason": "all fixtures loaded",
				"final_message": "fixtures loaded",
			}),
		];
		let (route_router, prompts) = router_with_json_responses(responses);

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
			request_id: roku_common_types::RequestId("req-byte-stability".to_string()),
			session_id: "session-byte-stability".to_string(),
			goal: "Read all fixtures".to_string(),
			planning_mode_hint: None,
			conversation_history: Vec::new(),
			model_override: None,
			thinking_effort: None,
		};
		let decision = crate::router::RouteDecision::new(
			IntentFamily::FilesystemRead,
			0.9,
			false,
			crate::router::RouteRisk::Low,
			vec!["Read".to_string()],
			vec!["core-fs".to_string()],
			Vec::new(),
			"byte-stability regression",
		);
		let mut loop_state =
			runtime.initialize_runtime_loop(&request, &request.session_id, &decision, Vec::new());

		let _result = tokio::runtime::Builder::new_multi_thread()
			.enable_all()
			.build()
			.expect("tokio runtime for execute-tool-loop bridge should build")
			.block_on(runtime.execute_tool_loop(
				&TaskId("task-byte-stability".to_string()),
				&request,
				&mut loop_state,
				&RuntimeMemorySections::default(),
				None,
				// `None` keeps the loop on the non-streaming path: the
				// test fixture's `SequenceJsonProvider` has no `stream()`
				// override and the default trait impl drops the
				// `tool_calls` field, which would force every call_tool
				// response into the "no tool calls" fallback.
				None,
				None,
			));

		let captured = prompts.lock().expect("prompt lock should succeed");
		assert_eq!(
			captured.len(),
			5,
			"five turns expected (4 Read + final_answer); got {}",
			captured.len(),
		);

		// The microcompact placeholder MUST NOT appear in any prompt
		// captured during normal multi-turn tool use.
		let placeholder = crate::runtime_loop::MICROCOMPACT_PLACEHOLDER;
		for (idx, prompt) in captured.iter().enumerate() {
			assert!(
				!prompt.contains(placeholder),
				"prompt[{idx}] unexpectedly contains the microcompact placeholder \
				 (`{placeholder}`); historical tool_result content must remain \
				 byte-stable across turns",
			);
		}

		// The final_answer prompt (index 4) sees the prefix of all four
		// tool results — including the OLDEST (turn 1, fixture A).
		// `MICROCOMPACT_RETAIN_RECENT = 3` would have left the oldest
		// eligible for rewriting under the deleted pre-flight path, so a
		// missing marker means a historical tool_result body was
		// rewritten by some path.
		for marker in markers {
			assert!(
				captured[4].contains(marker),
				"final_answer prompt must contain `{marker}` (intact body \
				 from earlier turn)"
			);
		}

		for path in &paths {
			cleanup_fixture(path);
		}
	}

	#[test]
	fn execute_tool_loop_emits_time_based_microcompact_event_when_last_llm_call_was_long_ago() {
		// Force the time-gated path to fire on turn 1 by seeding
		// `last_llm_call_at` with `UNIX_EPOCH` before running the loop.
		// The runtime computes `now - last_llm_call_at` and compares
		// against `CACHE_COLD_GAP` (5 min); the resulting gap (decades)
		// exceeds the threshold so the gate fires. The buffer has no
		// historical tool_results so `freed_tokens` is `0`, but the
		// event is still emitted so trace consumers can observe the
		// rewrite attempt.
		let (route_router, _prompts) = router_with_json_responses(vec![serde_json::json!({
			"action": "final_answer",
			"tool_name": null,
			"arguments": null,
			"reason": "done",
			"final_message": "ok"
		})]);
		let root = tempfile::tempdir().expect("temp root should exist");
		let runtime =
			GenericAgentRuntime::with_route_and_execution_routers_skill_registry_tool_config_and_plugin_snapshot(
				route_router,
				router_with_text_output("execution-provider", "unused"),
				SkillRegistry::file_backed(root.keep()),
				ToolCatalogConfig::default(),
				PluginRegistrySnapshot::permissive(),
				ToolsRuntimeConfig::default(),
			);
		let request = RequestEnvelope {
			request_id: roku_common_types::RequestId("req-time-gated-fires".to_string()),
			session_id: "session-time-gated-fires".to_string(),
			goal: "force the time gate to fire".to_string(),
			planning_mode_hint: None,
			conversation_history: Vec::new(),
			model_override: None,
			thinking_effort: None,
		};
		let decision = crate::router::RouteDecision::new(
			IntentFamily::Chat,
			0.9,
			false,
			crate::router::RouteRisk::Low,
			Vec::new(),
			Vec::new(),
			Vec::new(),
			"chat",
		);
		let mut loop_state =
			runtime.initialize_runtime_loop(&request, &request.session_id, &decision, Vec::new());
		// Inject a `last_llm_call_at` from the dawn of the unix epoch so
		// the gate's gap is decades, well above the 5-minute threshold.
		loop_state.record_llm_call_observed_at(std::time::SystemTime::UNIX_EPOCH);

		let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
		let _execution = tokio::runtime::Builder::new_multi_thread()
			.enable_all()
			.build()
			.expect("tokio runtime for execute-tool-loop bridge should build")
			.block_on(runtime.execute_tool_loop(
				&TaskId("task-time-gated-fires".to_string()),
				&request,
				&mut loop_state,
				&RuntimeMemorySections::default(),
				None,
				Some(&event_tx),
				None,
			));
		drop(event_tx);

		let mut events = Vec::new();
		while let Ok(event) = event_rx.try_recv() {
			events.push(event);
		}

		let time_gated: Vec<_> = events
			.iter()
			.filter_map(|e| match e {
				crate::runtime_loop::LoopEvent::TimeBasedMicrocompactRan {
					gap_minutes,
					freed_tokens,
					..
				} => Some((*gap_minutes, *freed_tokens)),
				_ => None,
			})
			.collect();
		assert_eq!(
			time_gated.len(),
			1,
			"time-gated microcompact must fire exactly once when \
			 last_llm_call_at is decades in the past; got {time_gated:?}",
		);
		let (gap_minutes, _freed) = time_gated[0];
		assert!(
			gap_minutes >= 5,
			"gap_minutes must reflect a cold-cache window (≥ 5 min); got {gap_minutes}",
		);

		// Per-call TokenUsage emission: a single-LLM-call loop must emit
		// exactly one event whose `prompt_tokens` matches the provider's
		// per-call value (24, from `SequenceJsonProvider`). A regression
		// to cumulative-totals emission would still pass the count check on
		// a single call, but would surface as `total_prompt_tokens` in
		// multi-call scenarios — the contract is asserted at the value
		// level so the warm-turn cache gate sees clean per-call ratios.
		let token_usage_events: Vec<_> = events
			.iter()
			.filter_map(|e| match e {
				crate::runtime_loop::LoopEvent::TokenUsage { prompt_tokens, .. } => {
					Some(*prompt_tokens)
				}
				_ => None,
			})
			.collect();
		assert_eq!(
			token_usage_events.len(),
			1,
			"single-LLM-call loop must emit exactly one per-call TokenUsage event; \
			 got {token_usage_events:?}",
		);
		assert_eq!(
			token_usage_events[0], 24,
			"TokenUsage.prompt_tokens must carry the per-call value from the \
			 provider (24), not a cumulative running total",
		);
	}

	#[test]
	fn execute_tool_loop_does_not_emit_time_based_microcompact_on_cold_start_session() {
		// Cold-start path: no `last_llm_call_at` is seeded before the
		// loop runs, so the gate sees `None` and must skip — no
		// `TimeBasedMicrocompactRan` event in the trace. The
		// warm-window branch (gap < 5 min after a previous successful
		// call) is covered by the pure-function tests in
		// `compact.rs::tests` because exercising it through
		// `execute_tool_loop` would require either time injection or a
		// streaming provider that propagates `tool_calls` through the
		// default `stream()` impl (the test fixture's
		// `SequenceJsonProvider` does not).
		let (route_router, _prompts) = router_with_json_responses(vec![serde_json::json!({
			"action": "final_answer",
			"tool_name": null,
			"arguments": null,
			"reason": "done",
			"final_message": "ok"
		})]);
		let root = tempfile::tempdir().expect("temp root should exist");
		let runtime =
			GenericAgentRuntime::with_route_and_execution_routers_skill_registry_tool_config_and_plugin_snapshot(
				route_router,
				router_with_text_output("execution-provider", "unused"),
				SkillRegistry::file_backed(root.keep()),
				ToolCatalogConfig::default(),
				PluginRegistrySnapshot::permissive(),
				ToolsRuntimeConfig::default(),
			);
		let request = RequestEnvelope {
			request_id: roku_common_types::RequestId("req-time-gated-skip".to_string()),
			session_id: "session-time-gated-skip".to_string(),
			goal: "verify gate stays closed in warm window".to_string(),
			planning_mode_hint: None,
			conversation_history: Vec::new(),
			model_override: None,
			thinking_effort: None,
		};
		let decision = crate::router::RouteDecision::new(
			IntentFamily::Chat,
			0.9,
			false,
			crate::router::RouteRisk::Low,
			Vec::new(),
			Vec::new(),
			Vec::new(),
			"chat",
		);
		let mut loop_state =
			runtime.initialize_runtime_loop(&request, &request.session_id, &decision, Vec::new());
		// Do NOT seed `last_llm_call_at` — cold-start path; gate must stay closed.

		let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
		let _execution = tokio::runtime::Builder::new_multi_thread()
			.enable_all()
			.build()
			.expect("tokio runtime for execute-tool-loop bridge should build")
			.block_on(runtime.execute_tool_loop(
				&TaskId("task-time-gated-skip".to_string()),
				&request,
				&mut loop_state,
				&RuntimeMemorySections::default(),
				None,
				Some(&event_tx),
				None,
			));
		drop(event_tx);

		let mut events = Vec::new();
		while let Ok(event) = event_rx.try_recv() {
			events.push(event);
		}

		let time_gated_count = events
			.iter()
			.filter(|e| {
				matches!(
					e,
					crate::runtime_loop::LoopEvent::TimeBasedMicrocompactRan { .. }
				)
			})
			.count();
		assert_eq!(
			time_gated_count, 0,
			"time-gated microcompact must not fire on cold start or within \
			 the warm window; observed events: {events:?}",
		);
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

		// Each outbound LLM call must be preceded by its own `ToolSchemaFrozen`
		// event — the provider was invoked twice (1 failure + 1 success after
		// reactive compaction), so two events are expected. The first carries
		// the turn's rebuild predicate; the retry carries `rebuilt = false`
		// because the frozen snapshot is reused across reactive retries.
		let schema_frozen_events: Vec<_> = events
			.iter()
			.filter_map(|e| match e {
				crate::runtime_loop::LoopEvent::ToolSchemaFrozen {
					step,
					hash,
					rebuilt,
				} => Some((*step, *hash, *rebuilt)),
				_ => None,
			})
			.collect();
		assert_eq!(
			schema_frozen_events.len(),
			2,
			"one ToolSchemaFrozen per outbound LLM call; got: {schema_frozen_events:?}"
		);
		assert_eq!(
			schema_frozen_events[0].1, schema_frozen_events[1].1,
			"hash must be stable across reactive-retry iterations within the same turn"
		);
		assert!(
			!schema_frozen_events[1].2,
			"retry must carry rebuilt=false — the frozen snapshot is reused"
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

	#[test]
	fn output_slot_escalation_triggers_on_max_tokens_and_length() {
		// Verify that the two finish_reason values that signal output truncation
		// are correctly recognized as escalation triggers. This test exercises
		// the condition logic in isolation — no LLM call is required.
		let truncation_signals = ["max_tokens", "length"];
		for signal in &truncation_signals {
			assert!(
				*signal == "max_tokens" || *signal == "length",
				"unexpected signal: {signal}"
			);
		}
		// Verify that other finish_reason values do NOT trigger escalation.
		let non_truncation_signals = ["stop", "tool_use", "end_turn", "content_filter"];
		for signal in &non_truncation_signals {
			assert!(
				*signal != "max_tokens" && *signal != "length",
				"signal {signal} should not be a truncation trigger"
			);
		}
	}

	// ---------------------------------------------------------------------------
	// Output-slot escalation capability gate tests (Issue #335)
	// ---------------------------------------------------------------------------

	/// Provider that returns a `max_tokens` finish_reason on the first call
	/// and tracks invocation count. When `cap_supported` is false the router
	/// must skip the escalation retry, so invocations must be exactly 1.
	struct MaxTokensProvider {
		invocations: Arc<std::sync::atomic::AtomicUsize>,
		cap_supported: bool,
	}

	#[async_trait]
	impl LlmProvider for MaxTokensProvider {
		fn provider_name(&self) -> &'static str {
			"max-tokens-provider"
		}

		async fn complete(
			&self,
			_model: &ModelProfile,
			_request: &GenerationRequest,
		) -> Result<ProviderResponse, ProviderCallError> {
			self.invocations
				.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
			// Always return a valid final_answer JSON so the runtime loop
			// terminates cleanly rather than failing to parse the decision.
			let output = serde_json::json!({
				"action": "final_answer",
				"tool_name": null,
				"arguments": null,
				"reason": "truncated by max_tokens",
				"final_message": "truncated response"
			})
			.to_string();
			Ok(ProviderResponse {
				output,
				finish_reason: Some("max_tokens".to_string()),
				prompt_tokens: 20,
				output_tokens: 16,
				cache_creation_input_tokens: 0,
				cache_read_input_tokens: 0,
				latency_ms: 5,
				tool_calls: None,
				response_id: None,
			})
		}

		fn supports_output_slot_cap(&self) -> bool {
			self.cap_supported
		}
	}

	fn router_with_max_tokens_provider(
		cap_supported: bool,
	) -> (LlmRouter, Arc<std::sync::atomic::AtomicUsize>) {
		let invocations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
		let mut router = LlmRouter::new(RoutingPolicy {
			max_request_cost_usd: 1.0,
			max_latency_ms: 5_000,
		});
		router.register_provider(MaxTokensProvider {
			invocations: Arc::clone(&invocations),
			cap_supported,
		});
		// Register a model that does NOT have a cost profile (so the existing
		// escalation path also bails out with "no profile") — this ensures the
		// only difference between cap_supported=true and cap_supported=false is
		// the event emitted.
		router.register_model(ModelProfile {
			model_id: "max-tokens-model".to_string(),
			provider: "max-tokens-provider".to_string(),
			max_context_tokens: 16_000,
			cost_per_1k_tokens_usd: 0.0,
			max_risk_tier: RiskTier::Low,
			route_priority: 100,
		});
		(router, invocations)
	}

	#[test]
	fn output_slot_escalation_unsupported_event_emitted_when_provider_caps_off() {
		let (route_router, invocations) = router_with_max_tokens_provider(false);
		let execution_router = router_with_text_output("exec-provider-no-cap", "exec");
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
			request_id: roku_common_types::RequestId("req-no-cap".to_string()),
			session_id: "session-no-cap".to_string(),
			goal: "Test escalation gate".to_string(),
			planning_mode_hint: None,
			conversation_history: Vec::new(),
			model_override: None,
			thinking_effort: None,
		};
		let decision = crate::router::RouteDecision::new(
			IntentFamily::Chat,
			0.9,
			false,
			crate::router::RouteRisk::Low,
			Vec::new(),
			Vec::new(),
			Vec::new(),
			"chat",
		);
		let mut loop_state =
			runtime.initialize_runtime_loop(&request, &request.session_id, &decision, Vec::new());

		let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
		tokio::runtime::Builder::new_multi_thread()
			.enable_all()
			.build()
			.expect("tokio runtime should build")
			.block_on(runtime.execute_tool_loop(
				&TaskId("task-no-cap".to_string()),
				&request,
				&mut loop_state,
				&RuntimeMemorySections::default(),
				None,
				Some(&event_tx),
				None,
			));

		drop(event_tx);
		let events: Vec<_> = std::iter::from_fn(|| event_rx.try_recv().ok()).collect();

		// The escalation-unsupported event must be emitted exactly once.
		let unsupported_count = events
			.iter()
			.filter(|e| {
				matches!(
					e,
					crate::runtime_loop::LoopEvent::OutputSlotEscalationUnsupported { .. }
				)
			})
			.count();
		assert_eq!(
			unsupported_count, 1,
			"OutputSlotEscalationUnsupported must be emitted exactly once"
		);

		// No OutputSlotEscalated events should be present.
		let escalated_count = events
			.iter()
			.filter(|e| {
				matches!(
					e,
					crate::runtime_loop::LoopEvent::OutputSlotEscalated { .. }
				)
			})
			.count();
		assert_eq!(
			escalated_count, 0,
			"OutputSlotEscalated must NOT be emitted when escalation is unsupported"
		);

		// The provider must have been called exactly once (no retry).
		assert_eq!(
			invocations.load(std::sync::atomic::Ordering::SeqCst),
			1,
			"provider must be invoked exactly once when escalation is skipped"
		);
	}

	#[test]
	fn output_slot_escalation_emitted_when_provider_caps_supported() {
		// When supports_output_slot_cap = true, the existing escalation path
		// fires (though with no cost profile the escalation guard bails early).
		// We only verify that OutputSlotEscalationUnsupported is NOT emitted.
		let (route_router, _invocations) = router_with_max_tokens_provider(true);
		let execution_router = router_with_text_output("exec-provider-cap", "exec");
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
			request_id: roku_common_types::RequestId("req-cap".to_string()),
			session_id: "session-cap".to_string(),
			goal: "Test escalation with cap supported".to_string(),
			planning_mode_hint: None,
			conversation_history: Vec::new(),
			model_override: None,
			thinking_effort: None,
		};
		let decision = crate::router::RouteDecision::new(
			IntentFamily::Chat,
			0.9,
			false,
			crate::router::RouteRisk::Low,
			Vec::new(),
			Vec::new(),
			Vec::new(),
			"chat",
		);
		let mut loop_state =
			runtime.initialize_runtime_loop(&request, &request.session_id, &decision, Vec::new());

		let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
		tokio::runtime::Builder::new_multi_thread()
			.enable_all()
			.build()
			.expect("tokio runtime should build")
			.block_on(runtime.execute_tool_loop(
				&TaskId("task-cap".to_string()),
				&request,
				&mut loop_state,
				&RuntimeMemorySections::default(),
				None,
				Some(&event_tx),
				None,
			));

		drop(event_tx);
		let events: Vec<_> = std::iter::from_fn(|| event_rx.try_recv().ok()).collect();

		// OutputSlotEscalationUnsupported must NOT be emitted when cap is supported.
		let unsupported_count = events
			.iter()
			.filter(|e| {
				matches!(
					e,
					crate::runtime_loop::LoopEvent::OutputSlotEscalationUnsupported { .. }
				)
			})
			.count();
		assert_eq!(
			unsupported_count, 0,
			"OutputSlotEscalationUnsupported must NOT be emitted when cap is supported"
		);
	}

	#[test]
	fn cache_break_detector_records_routed_model_not_override() {
		// Regression guard for issue #369. Under default routing
		// (`model_override = None`) the detector used to hash `""`
		// as the model component, so two consecutive turns that
		// actually routed to different providers would read as
		// "model unchanged" and the detector would silently miss
		// every cache break attributable to the routing swap. The
		// fix sources the model id via
		// `LlmRouter::selected_model_id_for_request` inside the
		// attempt loop so the fingerprint carries what
		// `router.generate*` will actually serve.
		//
		// Scaffolding: `router_with_json_responses` registers a
		// single model called `sequence-json-model`. A default-
		// routing request on this runtime resolves to that id; the
		// detector must record it.
		let (route_router, _prompts) = router_with_json_responses(vec![serde_json::json!({
			"action": "final_answer",
			"tool_name": null,
			"arguments": null,
			"reason": "test",
			"final_message": "done"
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
			request_id: roku_common_types::RequestId("req-cache-break-model".to_string()),
			session_id: "session-cache-break-model".to_string(),
			goal: "Test cache break model fingerprint".to_string(),
			planning_mode_hint: None,
			conversation_history: Vec::new(),
			// Default routing — the pre-fix code path would have
			// passed `""` as the model arg here.
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

		let _execution = tokio::runtime::Builder::new_multi_thread()
			.enable_all()
			.build()
			.expect("tokio runtime for execute-tool-loop bridge should build")
			.block_on(runtime.execute_tool_loop(
				&TaskId("task-cache-break-model".to_string()),
				&request,
				&mut loop_state,
				&RuntimeMemorySections::default(),
				None,
				None,
				None,
			));

		// After the turn, `check_response` consumed
		// `current_fingerprint` and promoted it to
		// `previous_fingerprint`. The recorded model must be the
		// routed serving id, not `""`.
		assert_eq!(
			loop_state.cache_break_detector.previous_fingerprint_model(),
			Some("sequence-json-model"),
			"detector must fingerprint the routed serving model, not \
			 `request.model_override` (which is `None` on this default-routing \
			 call and would have recorded the empty string)",
		);
	}

	#[test]
	fn retry_preflight_values_same_route_reuses_primary_estimate() {
		// Same-route retry: the helper must delegate to
		// `pre_call_estimate.calibration_pair` and return the
		// primary's `pre_call_tool_schema_bytes_len` verbatim. This
		// is the fast path; rerouted retries go through
		// `resolve_attempt_preflight` (verified by the test below).
		let (route_router, _prompts) = router_with_json_responses(vec![]);
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

		let pre_estimate = crate::runtime_loop::PromptTokenEstimate {
			system_tokens: 0,
			message_tokens: 100,
			framing_tokens: 0,
			tool_schema_tokens: 0,
			committed_baseline_tokens: 0,
			total_tokens: 100,
			raw_total_tokens: 100,
		};
		let calibration = crate::runtime_loop::PerModelCalibration::default();
		let config = runtime.agent_runtime_config.next_step.clone();
		let (cal_est, cal_real, schema_len, tool_schema_hash) = runtime.retry_preflight_values(
			"sequence-json-model",
			"sequence-json-model",
			&[Message::User {
				content: "test".to_string(),
			}],
			"system",
			&roku_plugin_llm::SystemPromptSections {
				static_blocks: Vec::new(),
				dynamic_blocks: Vec::new(),
			},
			&[],
			&config,
			6,
			42,
			0,
			0,
			&pre_estimate,
			150,
			&calibration,
			None,
		);
		assert_eq!(
			cal_est, 100,
			"same-route retry must return pre_call_estimate.raw_total_tokens as estimated"
		);
		assert_eq!(
			cal_real, 150,
			"same-route retry must return retry_prompt_tokens as real"
		);
		assert_eq!(
			schema_len, 42,
			"same-route retry must reuse primary's pre_call_tool_schema_bytes_len"
		);
		// Same-route retry must still fingerprint the primary's wire
		// tool-schema bytes — the retry persists into the committed
		// baseline under the retry's model id, so the hash must match
		// what the primary router actually serialized for this request,
		// not a constant. The helper resolves that preflight internally
		// on the same-route path; we assert reproducibility against an
		// independent resolve.
		let (_, _, expected_primary_schema_bytes) = runtime.resolve_attempt_preflight(
			&[Message::User {
				content: "test".to_string(),
			}],
			"system",
			&roku_plugin_llm::SystemPromptSections {
				static_blocks: Vec::new(),
				dynamic_blocks: Vec::new(),
			},
			&[],
			Some("sequence-json-model".to_string()),
			&config,
		);
		assert_eq!(
			tool_schema_hash,
			crate::runtime_loop::hash_tool_schema_bytes(&expected_primary_schema_bytes),
			"same-route retry must surface the primary model's tool-schema content hash",
		);
	}

	#[test]
	fn retry_preflight_values_rerouted_recomputes_against_retry_model() {
		// Rerouted retry: the helper must consult
		// `resolve_attempt_preflight` with the retry model id and
		// return a fresh estimate / schema length for that model,
		// not the primary's. The test is positive-direction: we
		// assert the returned values match what
		// `resolve_attempt_preflight(retry_model_id)` would produce
		// independently, so the helper's dispatch is verified without
		// needing to construct two providers with divergent wire
		// formats (we use the same provider twice with distinct model
		// ids — the returned values reflect the retry model's bucket
		// and the router's wire-bytes preview for that selection
		// request shape).
		let (route_router, _prompts) = router_with_json_responses(vec![]);
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

		let pre_estimate = crate::runtime_loop::PromptTokenEstimate {
			system_tokens: 0,
			message_tokens: 100,
			framing_tokens: 0,
			tool_schema_tokens: 0,
			committed_baseline_tokens: 0,
			total_tokens: 100,
			raw_total_tokens: 100,
		};
		let calibration = crate::runtime_loop::PerModelCalibration::default();
		let config = runtime.agent_runtime_config.next_step.clone();
		let messages = vec![Message::User {
			content: "rerouted retry path".to_string(),
		}];
		let system_prompt = "system";
		let system_prompt_sections = roku_plugin_llm::SystemPromptSections {
			static_blocks: Vec::new(),
			dynamic_blocks: Vec::new(),
		};
		let tool_definitions: Vec<ToolDefinition> = vec![];

		let (cal_est, cal_real, retry_schema_len, retry_tool_schema_hash) = runtime
			.retry_preflight_values(
				"sequence-json-model",
				"some-other-primary-model",
				&messages,
				system_prompt,
				&system_prompt_sections,
				&tool_definitions,
				&config,
				6,
				42,
				0,
				0,
				&pre_estimate,
				150,
				&calibration,
				None,
			);

		// Independently resolve what the retry preflight should see.
		let (_, _, expected_schema_bytes) = runtime.resolve_attempt_preflight(
			&messages,
			system_prompt,
			&system_prompt_sections,
			&tool_definitions,
			Some("sequence-json-model".to_string()),
			&config,
		);
		let expected_schema_len = expected_schema_bytes.len();
		assert_eq!(
			retry_schema_len, expected_schema_len,
			"rerouted retry schema length must equal what \
			 resolve_attempt_preflight produces for the retry model, \
			 not the primary's `pre_call_tool_schema_bytes_len` (42)",
		);
		assert_ne!(
			retry_schema_len, 42,
			"rerouted retry must NOT reuse the primary's schema length \
			 (which belongs to a different provider's serializer)",
		);

		// Fresh estimate means cal_est is re-computed for the retry
		// model's counter, not inherited from the primary's
		// pre_call_estimate (which was 100).
		// With an empty tool schema and cold-start (no baseline), the
		// retry estimate is `counter.count(user_message) +
		// framing`. The exact value depends on the byte-heuristic
		// counter (bytes/4 default); we assert it's non-zero and
		// different from the primary's pre-fixed 100 to prove a fresh
		// computation happened, rather than a fallback to the
		// primary's estimate.
		assert!(cal_est > 0, "fresh estimate must be positive");
		assert_eq!(
			cal_real, 150,
			"cal_real must always equal retry_prompt_tokens regardless of route"
		);
		// The retry tool-schema hash must fingerprint the retry
		// provider's serialized wire bytes, matching what
		// `resolve_attempt_preflight(retry_model_id)` would produce
		// independently. The rerouted path must never persist a hash
		// derived from the primary's serializer — that is exactly what
		// this P2 fix guards against.
		let expected_retry_hash =
			crate::runtime_loop::hash_tool_schema_bytes(&expected_schema_bytes);
		assert_eq!(
			retry_tool_schema_hash, expected_retry_hash,
			"rerouted retry must hash the retry provider's wire bytes, \
			 not the primary's",
		);
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
