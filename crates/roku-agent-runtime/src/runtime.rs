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
use crate::tool_config::{ToolCatalogConfig, ToolsRuntimeConfig};
use crate::tools::{
	build_builtin_tool_runtime_with_plugin_snapshot_and_runtime_capabilities_and_runtime_config,
	build_llm_tool_runtime_with_plugin_snapshot_and_runtime_config,
	build_resource_catalog_with_plugin_snapshot_and_runtime_capabilities_and_runtime_config,
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
	GenerationRequest, LlmRouter, Message, RiskTier, StreamChunk, ToolCallBlock,
};
use roku_plugin_skills::SkillRegistry;
use roku_plugin_tools::{ResourceCatalog, ResourceKind};
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
	agent_runtime_config: AgentRuntimeConfig,
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
		let shared_tool_runtime = Arc::new(tool_runtime);
		let mut runtime = Self {
			workers: Vec::new(),
			tool_runtime: Arc::clone(&shared_tool_runtime),
			resource_catalog,
			runtime_visible_tool_availability_snapshot,
			tool_config: tool_config.clone(),
			plugin_snapshot,
			route_router: None,
			agent_runtime_config,
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
		.with_route_router(Arc::new(route_router));
		runtime._mcp_runtime = mcp_runtime;
		runtime
	}

	pub fn resource_catalog(&self) -> &ResourceCatalog {
		&self.resource_catalog
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
		ToolObservation::from_runtime_error(tool_name, error)
	}

	/// Returns `(prompt_tokens, output_tokens)` consumed by compaction LLM calls.
	async fn maybe_compact(
		&self,
		loop_state: &mut LoopState,
		messages: &mut Vec<roku_plugin_llm::Message>,
		current_step_index: u32,
		event_sender: Option<&crate::runtime_loop::LoopEventSender>,
	) -> (u64, u64) {
		// Truncate oversized tool results in messages.
		let tool_result_max_chars = self.agent_runtime_config.r#loop.working_summary_max_chars;
		crate::runtime_loop::truncate_large_tool_results(messages, tool_result_max_chars);

		let threshold = self.agent_runtime_config.r#loop.compact_threshold_tokens();
		let msg_tokens = crate::runtime_loop::estimate_message_tokens(messages);
		let state_tokens = crate::runtime_loop::estimate_context_tokens(loop_state);
		let estimated = msg_tokens.max(state_tokens);
		if estimated > threshold {
			eprintln!(
				"Context compact triggered: estimated {estimated} tokens exceeds threshold {threshold}"
			);
			if let Some(sender) = event_sender {
				let _ = sender.send(crate::runtime_loop::LoopEvent::CompactTriggered {
					step: current_step_index,
					estimated_tokens: estimated,
				});
			}
			let compact_config = crate::runtime_loop::CompactConfig {
				retain_tail_steps: self.agent_runtime_config.r#loop.retain_tail_steps,
				working_summary_max_chars: self
					.agent_runtime_config
					.r#loop
					.working_summary_max_chars,
				..Default::default()
			};
			// Compact conversation messages (preserve recent 5).
			let retain_messages = 5_usize.max(compact_config.retain_tail_steps);
			if let Some(router) = self.route_router.as_deref() {
				let (_, msg_pt, msg_ot) = crate::runtime_loop::compact_messages_with_llm(
					messages,
					retain_messages,
					router,
				)
				.await;
				crate::runtime_loop::compact_history_with_llm(loop_state, &compact_config, router)
					.await;
				return (msg_pt, msg_ot);
			} else {
				crate::runtime_loop::compact_messages(messages, retain_messages);
				crate::runtime_loop::compact_history(loop_state, &compact_config);
			}
		}
		(0, 0)
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

		// Cost constants: rough estimate for Claude Sonnet tier.
		const COST_PER_M_INPUT_TOKENS_USD: f64 = 3.0;
		const COST_PER_M_OUTPUT_TOKENS_USD: f64 = 15.0;

		loop {
			// Refresh visible tools at the start of each turn.
			self.refresh_tool_loop_visible_tools(loop_state);
			let tool_definitions =
				build_tool_definitions(&loop_state.visible_tools, Some(&self.resource_catalog));

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
			let env_snapshot = crate::runtime_loop::environment::probe_environment();
			let system_prompt = crate::runtime_loop::system_prompt::build_system_prompt(
				env_snapshot,
				&loop_state.working_directory,
				project_instruction.as_deref(),
			);

			let config = &self.agent_runtime_config.next_step;
			let gen_request = GenerationRequest {
				system_prompt: Some(system_prompt),
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
					Some(tool_definitions)
				},
			};

			let current_step_index = loop_state.step_index + 1;

			// Stream the LLM response, accumulating text and tool_calls.
			let (accumulated_text, accumulated_tool_calls) = if let Some(sender) = event_sender {
				let step = current_step_index;
				let (tx, mut rx) = tokio::sync::mpsc::channel::<StreamChunk>(64);
				let event_tx = sender.clone();
				let accumulator = tokio::spawn(async move {
					let mut text = String::new();
					let mut tool_calls: Vec<ToolCallBlock> = Vec::new();
					let mut pending_by_id: HashMap<String, (String, String)> = HashMap::new();
					while let Some(chunk) = rx.recv().await {
						match chunk {
							StreamChunk::TextDelta { text: delta } => {
								text.push_str(&delta);
								let _ =
									event_tx.send(crate::runtime_loop::LoopEvent::LlmTextDelta {
										step,
										text: delta,
									});
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
									let arguments =
										serde_json::from_str(&args_str).unwrap_or(Value::Null);
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
				let (text, tool_calls) = accumulator.await.unwrap_or_default();
				let _ = sender.send(crate::runtime_loop::LoopEvent::LlmDecisionComplete {
					step: current_step_index,
				});
				match llm_result {
					Ok(ref resp) => {
						total_prompt_tokens =
							total_prompt_tokens.saturating_add(resp.prompt_tokens);
						total_output_tokens =
							total_output_tokens.saturating_add(resp.output_tokens);
					}
					Err(_) => {
						let message =
							format!("LLM streaming call failed for goal: {}", loop_state.goal);
						self.record_terminal_step(
							loop_state,
							StepAction::Fail,
							"LLM streaming call failed.",
							Some(message.clone()),
						);
						emit_token_usage(
							event_sender,
							current_step_index,
							total_prompt_tokens,
							total_output_tokens,
							COST_PER_M_INPUT_TOKENS_USD,
							COST_PER_M_OUTPUT_TOKENS_USD,
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
				(text, tool_calls)
			} else {
				// Non-streaming path.
				match router.generate(&gen_request).await {
					Ok(resp) => {
						total_prompt_tokens =
							total_prompt_tokens.saturating_add(resp.prompt_tokens);
						total_output_tokens =
							total_output_tokens.saturating_add(resp.output_tokens);
						let tool_calls = resp.tool_calls.unwrap_or_default();
						(resp.output, tool_calls)
					}
					Err(_) => {
						let message = format!("LLM call failed for goal: {}", loop_state.goal);
						self.record_terminal_step(
							loop_state,
							StepAction::Fail,
							"LLM call failed.",
							Some(message.clone()),
						);
						emit_token_usage(
							event_sender,
							current_step_index,
							total_prompt_tokens,
							total_output_tokens,
							COST_PER_M_INPUT_TOKENS_USD,
							COST_PER_M_OUTPUT_TOKENS_USD,
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
			// Agent is intercepted here before the normal tool dispatch path.
			for tc in &accumulated_tool_calls {
				let tool_name = &tc.name;
				let arguments = tc.arguments.clone();

				// Intercept Agent before the normal tool dispatch path.
				// Agent is a pseudo-tool: it spawns a sub-agent and returns the result as a
				// ToolResult. Budget is deducted from the parent before the sub-agent runs so that
				// a failing sub-agent still consumes budget (Class G: skip path resource accounting).
				if tool_name == "Agent" {
					// Deduct one step from parent budget unconditionally (Class G).
					loop_state.remaining_step_budget =
						loop_state.remaining_step_budget.saturating_sub(1);

					// Emit ToolStart for symmetry with other tools (Class C).
					if let Some(sender) = event_sender {
						let _ = sender.send(crate::runtime_loop::LoopEvent::ToolStart {
							step: current_step_index,
							tool_name: tool_name.clone(),
						});
					}
					let start_ms = std::time::Instant::now();

					let sub_result = self
						.execute_sub_agent(
							task_id,
							request,
							loop_state,
							&arguments,
							runtime_memory_sections,
							event_sender,
							approval_gate,
						)
						.await;

					let elapsed_ms = start_ms.elapsed().as_millis() as u64;

					// Emit ToolEnd for symmetry with other tools (Class C).
					if let Some(sender) = event_sender {
						let _ = sender.send(crate::runtime_loop::LoopEvent::ToolEnd {
							step: current_step_index,
							tool_name: tool_name.clone(),
							elapsed_ms: Some(elapsed_ms),
						});
					}

					messages.push(Message::ToolResult {
						tool_use_id: tc.id.clone(),
						content: sub_result,
						is_error: false,
					});
					continue;
				}

				// Check approval gate before executing the tool.
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

				// Emit ToolStart.
				if let Some(sender) = event_sender {
					let _ = sender.send(crate::runtime_loop::LoopEvent::ToolStart {
						step: current_step_index,
						tool_name: tool_name.clone(),
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

				// Emit ToolEnd.
				if let Some(sender) = event_sender {
					let _ = sender.send(crate::runtime_loop::LoopEvent::ToolEnd {
						step: current_step_index,
						tool_name: tool_name_owned.clone(),
						elapsed_ms: elapsed,
					});
				}

				let raw_tool_output = raw_tool_output_from_result(&execution.result);
				let observation =
					self.loop_observation_from_execution(&tool_name_owned, &execution.result);
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

				// Build ToolResult message for the next LLM turn.
				let tool_result_content = if raw_tool_output.is_null() {
					observation.message.clone()
				} else if let Some(s) = raw_tool_output.as_str() {
					s.to_string()
				} else {
					serde_json::to_string(&raw_tool_output)
						.unwrap_or_else(|_| observation.message.clone())
				};
				let tool_result_content =
					truncate_tool_result_for_message(&tool_result_content, MAX_TOOL_RESULT_CHARS);
				let is_error = !observation.ok;
				messages.push(Message::ToolResult {
					tool_use_id: tc.id.clone(),
					content: tool_result_content,
					is_error,
				});

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

			let (compact_pt, compact_ot) = self
				.maybe_compact(loop_state, &mut messages, current_step_index, event_sender)
				.await;
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

	/// Execute a sub-agent for an `Agent` tool call.
	///
	/// The sub-agent runs with a fresh message history and an independent budget drawn
	/// from the parent loop's remaining budget. The parent budget is deducted **before**
	/// this method is called (caller responsibility, Class G guard).
	///
	/// Recursion is blocked at depth 1: a sub-agent cannot spawn further sub-agents.
	async fn execute_sub_agent(
		&self,
		parent_task_id: &TaskId,
		parent_request: &RequestEnvelope,
		parent_loop_state: &mut LoopState,
		arguments: &Value,
		runtime_memory_sections: &RuntimeMemorySections,
		event_sender: Option<&crate::runtime_loop::LoopEventSender>,
		approval_gate: Option<&dyn crate::runtime_loop::approval::ToolApprovalGate>,
	) -> String {
		// Recursion guard: sub-agents cannot spawn sub-sub-agents.
		if parent_loop_state.sub_agent_depth >= 1 {
			return "[Sub-agent error] Sub-agents cannot spawn further sub-agents.".to_string();
		}

		let task = arguments
			.get("task")
			.and_then(Value::as_str)
			.unwrap_or("")
			.trim()
			.to_string();
		if task.is_empty() {
			return "[Sub-agent error] No task provided.".to_string();
		}

		// Allocate budget for the sub-agent from the parent's remaining budget.
		// The parent budget was already decremented by 1 in the caller (Class G).
		// Give the sub-agent up to 10 steps, capped at what the parent has left.
		let sub_budget = parent_loop_state.remaining_step_budget.min(10);
		// Deduct sub-agent budget from parent so total consumption is bounded.
		parent_loop_state.remaining_step_budget = parent_loop_state
			.remaining_step_budget
			.saturating_sub(sub_budget);

		// Build a fresh sub-request with independent message history.
		let sub_request = RequestEnvelope {
			request_id: roku_common_types::RequestId(format!(
				"{}-sub",
				parent_request.request_id.0
			)),
			session_id: parent_request.session_id.clone(),
			goal: task.clone(),
			planning_mode_hint: None,
			conversation_history: Vec::new(),
		};

		// Build a minimal LoopContext for the sub-agent.
		let sub_route_decision = parent_loop_state.route_decision.clone();
		let sub_context = crate::runtime_loop::LoopContext {
			request_id: sub_request.request_id.0.clone(),
			session_id: sub_request.session_id.clone(),
			goal: task.clone(),
			workspace_root: parent_loop_state.working_directory.clone(),
			working_directory: parent_loop_state.working_directory.clone(),
			visible_tools: parent_loop_state.visible_tools.clone(),
			bound_resources: parent_loop_state.bound_resources.clone(),
			route_decision: sub_route_decision,
			last_observation: None,
		};

		let recovery_budget = self.agent_runtime_config.r#loop.initial_recovery_budget;
		let mut sub_loop_state = LoopState::with_budgets(
			format!("sub-{}", sub_request.request_id.0),
			&sub_context,
			sub_budget,
			recovery_budget,
		);
		sub_loop_state.sub_agent_depth = parent_loop_state.sub_agent_depth + 1;

		let result = Box::pin(self.execute_tool_loop(
			parent_task_id,
			&sub_request,
			&mut sub_loop_state,
			runtime_memory_sections,
			None,
			event_sender,
			approval_gate, // inherit parent's approval gate
		))
		.await;

		// Truncate to avoid bloating the parent's context window.
		// Use char_indices for safe UTF-8 truncation (CJK/emoji safe).
		const MAX_SUB_AGENT_RESULT_CHARS: usize = 4000;
		let message = result.message;
		let char_count = message.chars().count();
		if char_count > MAX_SUB_AGENT_RESULT_CHARS {
			let byte_end = message
				.char_indices()
				.nth(MAX_SUB_AGENT_RESULT_CHARS)
				.map(|(i, _)| i)
				.unwrap_or(message.len());
			format!(
				"{}...\n[Sub-agent response truncated to {} chars]",
				&message[..byte_end],
				MAX_SUB_AGENT_RESULT_CHARS
			)
		} else {
			message
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
		let visible_tools = self.visible_tools_for_loop_state(loop_state);
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

	fn loop_observation_from_execution(
		&self,
		tool_name: &str,
		result: &ResultEnvelope,
	) -> ToolObservation {
		let payload = serde_json::from_str::<Value>(&result.payload)
			.unwrap_or_else(|_| json!({ "message": result.payload.clone() }));
		let observation = if result.status == ResultStatus::Ok {
			ToolObservation::from_result_payload(tool_name, &payload)
		} else {
			ToolObservation::from_error_payload(tool_name, &payload)
		};
		normalize_tool_loop_observation(observation)
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
fn emit_token_usage(
	event_sender: Option<&crate::runtime_loop::LoopEventSender>,
	step: u32,
	prompt_tokens: u64,
	output_tokens: u64,
	cost_per_m_input_usd: f64,
	cost_per_m_output_usd: f64,
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

fn tool_selector_by_name(catalog: &ResourceCatalog, tool_name: &str) -> Option<ResourceSelector> {
	catalog
		.entries()
		.iter()
		.find(|entry| entry.kind == ResourceKind::Tool && entry.name == tool_name)
		.map(|entry| entry.selector.clone())
}

fn execution_elapsed_ms(result: &ResultEnvelope) -> Option<u64> {
	serde_json::from_str::<Value>(&result.payload)
		.ok()
		.and_then(|payload| payload.get("elapsed_ms").and_then(Value::as_u64))
}

fn raw_tool_output_from_result(result: &ResultEnvelope) -> Value {
	let payload = serde_json::from_str::<Value>(&result.payload)
		.unwrap_or_else(|_| json!({ "message": result.payload.clone() }));
	if result.status == ResultStatus::Ok {
		return payload
			.get("output")
			.cloned()
			.unwrap_or_else(|| payload.clone());
	}
	payload
}

fn loop_probe_trace_payload(loop_state: &LoopState) -> Value {
	serde_json::to_value(runtime_loop_trace(loop_state))
		.unwrap_or_else(|_| json!({ "schema_version": "runtime_loop_trace.v1" }))
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

fn normalize_tool_loop_observation(observation: ToolObservation) -> ToolObservation {
	// general.execute has been removed; observations from all tools are returned as-is.
	observation
}

/// Maximum characters for a tool result before truncation in the turn loop.
/// This protects the LLM context window from oversized single tool outputs.
const MAX_TOOL_RESULT_CHARS: usize = 80_000;

/// Truncate a tool result to head + tail with an informative note.
fn truncate_tool_result_for_message(content: &str, max_chars: usize) -> String {
	if content.len() <= max_chars {
		return content.to_string();
	}
	let head_chars = max_chars * 4 / 5;
	let tail_chars = max_chars / 5;
	let head_end = content
		.char_indices()
		.nth(head_chars)
		.map(|(i, _)| i)
		.unwrap_or(content.len());
	let tail_start = content
		.char_indices()
		.rev()
		.nth(tail_chars.saturating_sub(1))
		.map(|(i, _)| i)
		.unwrap_or(0);
	let total_lines = content.lines().count();
	format!(
		"{}\n\n[Output truncated: showing first and last portions of {} total lines ({} chars). \
		 Ask the user or use a more specific query to narrow results.]\n\n{}",
		&content[..head_end],
		total_lines,
		content.len(),
		&content[tail_start..],
	)
}

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

	#[test]
	fn normalize_tool_loop_observation_passes_through_unchanged() {
		// general.execute has been removed; normalize_tool_loop_observation is now a no-op
		// that returns observations from all tools without modification.
		let observation = ToolObservation {
			ok: true,
			tool_name: "inventory.describe".to_string(),
			error_type: None,
			terminal: true,
			data: json!({}),
			message: "some result".to_string(),
		};
		let normalized = normalize_tool_loop_observation(observation.clone());
		assert_eq!(normalized.ok, observation.ok);
		assert_eq!(normalized.tool_name, observation.tool_name);
		assert_eq!(normalized.terminal, observation.terminal);
		assert_eq!(normalized.message, observation.message);
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
