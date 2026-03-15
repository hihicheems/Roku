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

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use crate::result::{policy_rejection_result, tool_failure_result, tool_success_result};
use crate::router::{
	DirectRouteExecutionResult, EscalationAction, IntentFamily, RouteClassifierContext,
	RouteDecisionResult,
};
use crate::runtime_config::AgentRuntimeConfig;
use crate::runtime_loop::{
	AskUserPayload, ContextProjection, LoopContext, LoopState, StepAction, StepObservation,
	StepRecord, ToolObservation, VisibleToolHint, attachments_for_tool, build_context_projection,
	build_loop_context, decide_tool_loop_next_step, effective_ask_user_payload, intake_request,
	interpret_observation, next_working_directory_from_observation, runtime_loop_trace,
	summarize_observation, tool_required_argument_keys,
};
use crate::tool_config::{ToolCatalogConfig, ToolsRuntimeConfig};
use crate::tools::{
	build_builtin_tool_runtime_with_plugin_snapshot_and_runtime_capabilities_and_runtime_config,
	build_llm_tool_runtime_with_plugin_snapshot_and_runtime_config,
	build_resource_catalog_with_plugin_snapshot_and_runtime_capabilities_and_runtime_config,
};
use crate::workers::{
	data_worker_with_config, generic_worker_with_config, inventory_worker_with_config,
	research_worker_with_config, review_worker_with_config, skill_execute_worker_with_config,
	skill_worker_with_config,
};
use roku_common_types::{
	AgentContext, AggregationMode, ConversationRole, ConversationTurn, EvidenceItem,
	GeneralExecuteCompletion, JoinPolicy, NodeBudgetSnapshot, NodeId, PolicyBindings,
	RequestEnvelope, RerunPolicy, ResourceSelector, ResultStatus, RetryPolicy, TaskId,
	TaskNodeDispatchPolicy, TaskNodeKind,
};
use roku_common_types::{AgentInstanceSpec, ResultEnvelope, TaskNode};
use roku_plugin_catalog::{ResourceCatalog, ResourceKind};
use roku_plugin_core::PluginRegistrySnapshot;
use roku_plugin_host::{ToolExecutionResult, ToolInvocation, ToolRuntime, ToolRuntimeError};
use roku_plugin_llm::LlmRouter;
use roku_plugin_skills::SkillRegistry;
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

pub struct GenericAgentRuntime {
	workers: Vec<WorkerRegistryEntry>,
	tool_runtime: Arc<ToolRuntime>,
	resource_catalog: ResourceCatalog,
	tool_config: ToolCatalogConfig,
	plugin_snapshot: PluginRegistrySnapshot,
	route_router: Option<Arc<LlmRouter>>,
	skill_execution_available: bool,
	agent_runtime_config: AgentRuntimeConfig,
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
		let skill_execution_available = resource_catalog
			.entries()
			.iter()
			.any(|entry| entry.name == "skill.execute");
		let shared_tool_runtime = Arc::new(tool_runtime);
		let mut runtime = Self {
			workers: Vec::new(),
			tool_runtime: Arc::clone(&shared_tool_runtime),
			resource_catalog,
			tool_config: tool_config.clone(),
			plugin_snapshot,
			route_router: None,
			skill_execution_available,
			agent_runtime_config,
		};
		runtime.register_worker(
			96,
			skill_execute_worker_with_config(Arc::clone(&shared_tool_runtime), &tool_config),
		);
		runtime.register_worker(
			95,
			skill_worker_with_config(Arc::clone(&shared_tool_runtime), &tool_config),
		);
		runtime.register_worker(
			90,
			research_worker_with_config(Arc::clone(&shared_tool_runtime), &tool_config),
		);
		runtime.register_worker(
			85,
			inventory_worker_with_config(Arc::clone(&shared_tool_runtime), &tool_config),
		);
		runtime.register_worker(
			80,
			data_worker_with_config(Arc::clone(&shared_tool_runtime), &tool_config),
		);
		runtime.register_worker(
			70,
			review_worker_with_config(Arc::clone(&shared_tool_runtime), &tool_config),
		);
		runtime.register_worker(
			10,
			generic_worker_with_config(shared_tool_runtime, &tool_config),
		);
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

	pub fn resource_catalog(&self) -> &ResourceCatalog {
		&self.resource_catalog
	}

	pub fn tool_config(&self) -> &ToolCatalogConfig {
		&self.tool_config
	}

	pub fn plugin_snapshot(&self) -> &PluginRegistrySnapshot {
		&self.plugin_snapshot
	}

	pub fn classify_route(
		&self,
		request: &RequestEnvelope,
		_session_id: &str,
	) -> RouteDecisionResult {
		crate::runtime_loop::classify_existing_route(
			RouteClassifierContext {
				catalog: &self.resource_catalog,
				tool_config: &self.tool_config,
				agent_runtime_config: &self.agent_runtime_config,
				plugin_snapshot: &self.plugin_snapshot,
				route_router: self.route_router.as_deref(),
				skill_execution_available: self.skill_execution_available,
			},
			request,
		)
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
			StepAction::FinalAnswer | StepAction::Fail | StepAction::CallTool => {
				StepObservation::FinalMessage {
					final_message: message,
				}
			}
		});
		let reason = reason.into();
		let step = StepRecord::terminal(
			loop_state.step_index + 1,
			terminal_decision(action, &reason, observation.as_ref()),
			loop_state.visible_tools.clone(),
			observation,
			match action {
				StepAction::AskUser => loop_state.remaining_step_budget,
				StepAction::FinalAnswer | StepAction::Fail | StepAction::CallTool => {
					loop_state.remaining_step_budget.saturating_sub(1)
				}
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
			StepAction::CallTool => crate::runtime_loop::LoopStatus::LoopRunning,
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
			terminal_decision(
				StepAction::AskUser,
				&reason,
				Some(&StepObservation::AskUser {
					final_message: payload.final_message.clone(),
				}),
			),
			loop_state.visible_tools.clone(),
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

	pub fn execute_tool_loop(
		&self,
		task_id: &TaskId,
		request: &RequestEnvelope,
		loop_state: &mut LoopState,
		user_reply: Option<&str>,
	) -> DirectRouteExecutionResult {
		let grounding_input = user_reply.unwrap_or(&loop_state.goal).to_string();
		loop_state.note_grounding_input(&grounding_input);
		loop {
			let context_projection = self.refresh_tool_loop_projection(loop_state);
			let next_step = decide_tool_loop_next_step(
				loop_state,
				&context_projection,
				self.route_router.as_deref(),
				user_reply,
				&self.agent_runtime_config.next_step,
			);
			match next_step.action {
				crate::runtime_loop::NextStepAction::CallTool => {
					let Some(tool_name) = next_step.tool_name.as_deref() else {
						return self.synthetic_loop_terminal_result(
							task_id,
							"tool",
							next_step.reason,
							StepAction::Fail,
							ResultStatus::Error,
							Some(loop_state),
						);
					};
					let attachments =
						attachments_for_tool(tool_name, user_reply.unwrap_or(&loop_state.goal));
					let execution = self.execute_loop_tool_invocation(
						task_id,
						request,
						loop_state,
						&context_projection,
						tool_name,
						next_step.arguments.clone().unwrap_or_else(|| json!({})),
						&attachments,
					);
					let raw_tool_output = raw_tool_output_from_result(&execution.result);
					let observation =
						self.loop_observation_from_execution(tool_name, &execution.result);
					let interpreted = interpret_observation(
						loop_state,
						observation.clone(),
						next_working_directory_from_observation(
							&observation,
							&loop_state.working_directory,
						),
					);
					let step = StepRecord::tool_call(
						loop_state.step_index + 1,
						next_step.clone(),
						loop_state.visible_tools.clone(),
						raw_tool_output,
						StepObservation::Tool(observation.clone()),
						interpreted.clone(),
						execution_elapsed_ms(&execution.result),
						interpreted.remaining_step_budget,
						interpreted.remaining_recovery_budget,
						interpreted
							.new_working_directory
							.clone()
							.unwrap_or_else(|| loop_state.working_directory.clone()),
					);
					loop_state.record_step(step);
					if interpreted.should_ask_user {
						let payload = effective_ask_user_payload(
							&loop_state.goal,
							loop_state.last_observation.as_ref(),
							None,
						);
						let message = payload.final_message.clone();
						self.record_ask_user_step(
							loop_state,
							"Runtime paused for user clarification after the latest tool observation.",
							payload,
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
					if interpreted.should_emit_final_answer {
						let message = summarized_tool_loop_message(&loop_state.goal, &observation);
						self.record_terminal_step(
							loop_state,
							StepAction::FinalAnswer,
							"Runtime completed after a terminal tool observation.",
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
					}
					if interpreted.should_fail || !interpreted.continue_allowed {
						let message = tool_loop_failure_message(&loop_state.goal, &interpreted);
						self.record_terminal_step(
							loop_state,
							StepAction::Fail,
							"Runtime could not continue after the latest tool observation.",
							Some(message.clone()),
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
				crate::runtime_loop::NextStepAction::AskUser => {
					let payload = effective_ask_user_payload(
						&loop_state.goal,
						loop_state.last_observation.as_ref(),
						next_step.final_message.map(AskUserPayload::freeform),
					);
					let message = payload.final_message.clone();
					self.record_ask_user_step(loop_state, next_step.reason, payload);
					return self.synthetic_loop_terminal_result(
						task_id,
						"tool",
						message,
						StepAction::AskUser,
						ResultStatus::Ok,
						Some(loop_state),
					);
				}
				crate::runtime_loop::NextStepAction::FinalAnswer => {
					let message = next_step.final_message.unwrap_or_else(|| {
						loop_state
							.last_observation
							.as_ref()
							.map(|observation| {
								summarized_tool_loop_message(&loop_state.goal, observation)
							})
							.unwrap_or_else(|| "Runtime loop completed.".to_string())
					});
					self.record_terminal_step(
						loop_state,
						StepAction::FinalAnswer,
						next_step.reason,
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
				}
				crate::runtime_loop::NextStepAction::Fail => {
					let reason = next_step.reason;
					let message = next_step.final_message.unwrap_or_else(|| reason.clone());
					self.record_terminal_step(
						loop_state,
						StepAction::Fail,
						reason,
						Some(message.clone()),
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
		}
	}

	pub fn execute_escalation_action(
		&self,
		task_id: &TaskId,
		request: &RequestEnvelope,
		result: &crate::router::RouteEscalationPlan,
	) -> DirectRouteExecutionResult {
		let fallback_message = match result.action {
			EscalationAction::AskForMoreInfo => {
				ask_for_more_info_message(&request.goal, &result.decision.missing_arguments)
			}
			EscalationAction::FallbackAnswer => fallback_answer_message(
				&request.goal,
				result.decision.intent_family,
				&result.decision.reason,
			),
			EscalationAction::EnterLimitedPlanning => {
				limited_planning_compatibility_message(&request.goal, &result.decision.reason)
			}
		};

		if matches!(result.action, EscalationAction::EnterLimitedPlanning) {
			return self.synthetic_message_result(
				task_id,
				"direct-route",
				"direct-route:compatibility-fallback",
				fallback_message,
				0.74,
			);
		}

		if matches!(result.action, EscalationAction::AskForMoreInfo) {
			return self.synthetic_message_result(
				task_id,
				"direct-route",
				"direct-route:ask-for-more-info",
				fallback_message,
				0.72,
			);
		}

		if self.route_router.is_none() {
			return self.synthetic_message_result(
				task_id,
				"direct-route",
				"direct-route:escalation",
				fallback_message,
				0.70,
			);
		}

		let action_hint = match result.action {
			EscalationAction::AskForMoreInfo => format!(
				"Ask the user for the missing arguments: {}. Do not claim any execution happened.",
				result.decision.missing_arguments.join(", ")
			),
			EscalationAction::FallbackAnswer => format!(
				"Explain that the requested capability is not currently available in the runtime inventory. Intent family: {:?}. Do not claim execution success.",
				result.decision.intent_family
			),
			EscalationAction::EnterLimitedPlanning =>
				"Explain that the request would need a planning-heavy workflow, but this runtime only exposes direct routes and compatibility fallback responses for new requests. Do not claim any execution happened.".to_string(),
		};
		self.execute_tool_like_route(
			task_id,
			request,
			"direct-route",
			&action_hint,
			vec![ResourceSelector::tool(tool_name_for_role(
				&self.tool_config,
				crate::tool_config::BuiltinToolRole::General,
			))],
			None,
		)
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

	fn refresh_tool_loop_projection(&self, loop_state: &mut LoopState) -> ContextProjection {
		loop_state.visible_tools = self.visible_tools_for_loop_state(loop_state);
		let mut projection = build_context_projection(loop_state);
		projection.visible_tool_hints = self.visible_tool_hints_for(&loop_state.visible_tools);
		projection
	}

	fn visible_tool_hints_for(
		&self,
		visible_tools: &[String],
	) -> BTreeMap<String, VisibleToolHint> {
		visible_tools
			.iter()
			.filter_map(|tool_name| {
				self.resource_catalog
					.entries()
					.iter()
					.find(|entry| entry.kind == ResourceKind::Tool && entry.name == *tool_name)
					.map(|entry| {
						(
							tool_name.clone(),
							VisibleToolHint {
								selection_hint: compact_selection_hint(
									entry.effective_selection_hint(),
									self.agent_runtime_config
										.prompts
										.visible_tool_hint_max_chars,
								),
								required_argument_keys: tool_required_argument_keys(tool_name)
									.iter()
									.map(|key| (*key).to_string())
									.collect(),
							},
						)
					})
			})
			.collect()
	}

	fn compose_visible_tools(
		&self,
		route_decision: &crate::router::RouteDecision,
		_loop_state: Option<&LoopState>,
	) -> Vec<String> {
		let enabled_tools = self
			.resource_catalog
			.descriptors_for_kind(ResourceKind::Tool)
			.into_iter()
			.map(|descriptor| descriptor.name)
			.collect::<HashSet<_>>();
		let mut visible_tools = Vec::new();
		append_enabled_tool_names(
			&mut visible_tools,
			&enabled_tools,
			route_decision.candidate_tools.iter().map(String::as_str),
		);
		append_enabled_tool_names(
			&mut visible_tools,
			&enabled_tools,
			safe_baseline_tool_pool().iter().copied(),
		);
		if visible_tools.is_empty() {
			let mut fallback_tools = enabled_tools.into_iter().collect::<Vec<_>>();
			fallback_tools.sort();
			return fallback_tools;
		}
		visible_tools
	}

	fn execute_tool_like_route(
		&self,
		task_id: &TaskId,
		request: &RequestEnvelope,
		node_id: &str,
		step_summary: &str,
		resources: Vec<ResourceSelector>,
		explicit_source_url: Option<&String>,
	) -> DirectRouteExecutionResult {
		let capabilities = route_capabilities(&self.resource_catalog, &resources);
		let node = TaskNode {
			node_id: NodeId(node_id.to_string()),
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
		let mut spec = AgentInstanceSpec {
			instance_id: format!("direct-route:{}", node.node_id.0),
			context: AgentContext {
				task_id: task_id.clone(),
				node_id: node.node_id.clone(),
				summary: node.description.clone(),
				resources,
				conversation_history: request.conversation_history.clone(),
			},
			capabilities,
			capability_tokens: Vec::new(),
			policy_bindings: PolicyBindings {
				budget_tokens: 8_000,
				time_budget_ms: 60_000,
			},
		};
		if let Some(source_url) = explicit_source_url {
			spec.context.summary = format!("{}\nSource URL: {source_url}", spec.context.summary);
		}
		let result = self.execute(&spec, &node);
		let message = extract_result_message(&result);
		DirectRouteExecutionResult {
			node,
			result,
			message,
			terminal_step_action: None,
		}
	}

	fn synthetic_message_result(
		&self,
		task_id: &TaskId,
		node_id: &str,
		producer: &str,
		message: String,
		confidence: f32,
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
			status: ResultStatus::Ok,
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

	fn execute_tool_invocation_with_resources_and_summary(
		&self,
		task_id: &TaskId,
		request: &RequestEnvelope,
		selector: &ResourceSelector,
		arguments: Value,
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
			"budget_tokens": spec.policy_bindings.budget_tokens,
			"time_budget_ms": spec.policy_bindings.time_budget_ms,
		});
		merge_json_object(&mut input, arguments);
		let invocation = ToolInvocation {
			tool_name: selector.name().to_string(),
			input,
			granted_capabilities: spec.capabilities.clone(),
			invocation_key: Some(format!(
				"{}:{}:{}",
				task_id.0,
				node.node_id.0,
				selector.display_key()
			)),
			attachments: attachments.to_vec(),
		};
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
				let result =
					tool_failure_result(&spec, &node, "direct-route", selector.name(), error);
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
		context_projection: &ContextProjection,
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
			attachments,
			&loop_state.bound_resources,
			&tool_loop_step_summary(context_projection, tool_name),
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

		generic_worker_with_config(Arc::clone(&self.tool_runtime), &self.tool_config)
			.execute(spec, node)
	}
}

impl Default for GenericAgentRuntime {
	fn default() -> Self {
		Self::with_skill_registry(SkillRegistry::disabled())
	}
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
			StepAction::CallTool => crate::runtime_loop::NextStepAction::CallTool,
			StepAction::AskUser => crate::runtime_loop::NextStepAction::AskUser,
			StepAction::FinalAnswer => crate::runtime_loop::NextStepAction::FinalAnswer,
			StepAction::Fail => crate::runtime_loop::NextStepAction::Fail,
		},
		tool_name: None,
		arguments: None,
		reason: reason.to_string(),
		final_message,
	}
}

fn ask_for_more_info_message(goal: &str, missing_arguments: &[String]) -> String {
	if !goal.is_ascii() {
		format!(
			"我还缺少继续处理所需的信息：{}。请补充后我再继续。",
			missing_arguments.join(", ")
		)
	} else {
		format!(
			"I still need more information before I can continue: {}.",
			missing_arguments.join(", ")
		)
	}
}

fn fallback_answer_message(goal: &str, intent_family: IntentFamily, reason: &str) -> String {
	if !goal.is_ascii() {
		format!(
			"这个请求目前被识别为 {:?}，但当前运行时还没有对应的 direct tool。{reason} 当前也没有启用 planning-heavy workflow，所以我只返回兼容降级说明，没有执行任何外部操作。",
			intent_family
		)
	} else {
		format!(
			"This request was classified as {:?}, but the current runtime does not expose a matching direct tool and does not enable planning-heavy workflows for new requests. {reason} No external action has been executed.",
			intent_family
		)
	}
}

fn limited_planning_compatibility_message(goal: &str, reason: &str) -> String {
	if !goal.is_ascii() {
		format!(
			"当前 runtime 没有启用 planning-heavy workflow，所以这个请求不会进入旧 planner。{reason} 请把请求缩小成单步可执行操作，或先明确你要读取的文件、表格、网页查询或 Python 代码。"
		)
	} else {
		format!(
			"This runtime does not enable planning-heavy workflows for new requests, so the request will not enter the legacy planner. {reason} Please narrow it to a single executable step or specify the exact file, table, web query, or Python code you want."
		)
	}
}

fn normalize_tool_loop_observation(observation: ToolObservation) -> ToolObservation {
	if observation.tool_name != "general.execute" {
		return observation;
	}

	let Some(completion) = observation
		.data
		.get("completion")
		.cloned()
		.and_then(|value| serde_json::from_value::<GeneralExecuteCompletion>(value).ok())
	else {
		return observation;
	};

	ToolObservation {
		ok: completion.completion_kind.ok(),
		tool_name: observation.tool_name,
		error_type: completion.completion_kind.error_type().map(str::to_string),
		terminal: completion.completion_kind.terminal(true),
		data: observation.data,
		message: completion.final_message,
	}
}

fn summarized_tool_loop_message(goal: &str, observation: &ToolObservation) -> String {
	summarize_observation(goal, observation).final_message
}

fn tool_loop_failure_message(
	goal: &str,
	interpreted: &crate::runtime_loop::InterpretedObservation,
) -> String {
	if interpreted.budget_exhausted {
		return if !goal.is_ascii() {
			"运行时循环在产出最终答案前已经耗尽 step budget。".to_string()
		} else {
			"The runtime loop exhausted its step budget before it produced a final answer."
				.to_string()
		};
	}

	if interpreted.recovery_exhausted {
		return if !goal.is_ascii() {
			"运行时循环在恢复失败后已经耗尽 recovery budget。".to_string()
		} else {
			"The runtime loop exhausted its recovery budget after repeated tool failures."
				.to_string()
		};
	}

	summarized_tool_loop_message(goal, &interpreted.raw_observation)
}

fn append_enabled_tool_names<'a>(
	visible_tools: &mut Vec<String>,
	enabled_tools: &HashSet<String>,
	tool_names: impl IntoIterator<Item = &'a str>,
) {
	for tool_name in tool_names {
		if enabled_tools.contains(tool_name)
			&& !visible_tools.iter().any(|existing| existing == tool_name)
		{
			visible_tools.push(tool_name.to_string());
		}
	}
}

fn compact_selection_hint(selection_hint: &str, max_chars: usize) -> String {
	let trimmed = selection_hint.trim();
	if trimmed.chars().count() <= max_chars {
		return trimmed.to_string();
	}
	let truncated = trimmed
		.chars()
		.take(max_chars.saturating_sub(3))
		.collect::<String>();
	format!("{truncated}...")
}

fn safe_baseline_tool_pool() -> &'static [&'static str] {
	&[
		"general.execute",
		"inventory.describe",
		"fs.find",
		"fs.read_text",
		"fs.list_dir",
		"fs.inspect",
		"fs.exists",
		"fs.glob",
		"table.preview",
		"table.inspect",
		"table.list_sheets",
		"table.schema",
		"web.search",
		"command.run",
		"python.run",
	]
}

fn tool_loop_step_summary(context_projection: &ContextProjection, tool_name: &str) -> String {
	let projection_json =
		serde_json::to_string_pretty(context_projection).unwrap_or_else(|_| "{}".to_string());
	format!(
		"Continue the generic runtime loop with tool `{tool_name}`.\nCurrent context projection:\n{projection_json}"
	)
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

fn tool_name_for_role(
	tool_config: &ToolCatalogConfig,
	role: crate::tool_config::BuiltinToolRole,
) -> String {
	tool_config
		.tool_for_role(role)
		.map(|tool| tool.name.clone())
		.unwrap_or_else(|| role.as_str().to_string())
}

#[cfg(test)]
mod tests {
	use roku_common_types::{
		AgentContext, AggregationMode, EvidenceItem, JoinPolicy, NodeId, PolicyBindings,
		ResultStatus, RuntimeLoopTrace, TaskId, TaskNode, TaskNodeKind,
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
	use std::io::{Cursor, Read, Write};
	use std::net::TcpListener;
	use std::sync::{Arc, Mutex};
	use std::thread;
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

	fn runtime_with_fixed_general_llm() -> GenericAgentRuntime {
		runtime_with_fixed_general_llm_and_tools_config(ToolsRuntimeConfig::default())
	}

	fn runtime_with_fixed_general_llm_and_tools_config(
		tools_runtime_config: ToolsRuntimeConfig,
	) -> GenericAgentRuntime {
		let mut router = LlmRouter::new(RoutingPolicy {
			max_request_cost_usd: 1.0,
			max_latency_ms: 5_000,
		});
		router.register_provider(FixedLlmProvider);
		router.register_model(ModelProfile {
			model_id: "test-model".to_string(),
			provider: "test-provider".to_string(),
			max_context_tokens: 16_000,
			cost_per_1k_tokens_usd: 0.0,
			max_risk_tier: RiskTier::Critical,
			route_priority: 100,
		});
		GenericAgentRuntime::with_llm_router_skill_registry_tool_config_and_plugin_snapshot(
			router,
			SkillRegistry::disabled(),
			ToolCatalogConfig::default(),
			roku_plugin_core::PluginRegistrySnapshot::permissive(),
			tools_runtime_config,
		)
	}

	fn runtime_loop_trace_for_goal(runtime: &GenericAgentRuntime, goal: &str) -> RuntimeLoopTrace {
		let request = RequestEnvelope {
			request_id: roku_common_types::RequestId(format!(
				"req-{}",
				goal.chars()
					.filter(|character| character.is_ascii_alphanumeric())
					.collect::<String>()
					.to_ascii_lowercase()
			)),
			session_id: "session-regression".to_string(),
			goal: goal.to_string(),
			planning_mode_hint: None,
			conversation_history: Vec::new(),
		};
		let route = runtime.classify_route(&request, &request.session_id);
		let crate::router::RouteDecisionResult::Direct(plan) = route else {
			panic!("expected direct route for regression goal `{goal}`");
		};
		let mut loop_state = runtime.initialize_runtime_loop(
			&request,
			&request.session_id,
			&plan.decision,
			plan.bound_resources.clone(),
		);
		let task_id = TaskId(format!("task-{}", request.request_id.0));
		let _ = runtime.execute_tool_loop(&task_id, &request, &mut loop_state, None);
		crate::runtime_loop::runtime_loop_trace(&loop_state)
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

	fn spawn_mock_web_search_server(body: &'static str) -> String {
		let listener = TcpListener::bind("127.0.0.1:0").expect("mock listener should bind");
		let address = listener
			.local_addr()
			.expect("mock listener should expose an address");
		thread::spawn(move || {
			let Ok((mut stream, _)) = listener.accept() else {
				return;
			};
			let mut buffer = [0_u8; 1024];
			let _ = stream.read(&mut buffer);
			let response = format!(
				"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
				body.len(),
				body
			);
			let _ = stream.write_all(response.as_bytes());
		});
		format!("http://{address}/search")
	}

	fn assert_regression_case(
		case_id: &str,
		suite: crate::runtime_loop::RegressionSuiteKind,
		trace: &RuntimeLoopTrace,
		expectation: crate::runtime_loop::RuntimeLoopRegressionExpectation,
	) {
		let report = crate::runtime_loop::evaluate_runtime_loop_regression_case(
			case_id,
			suite,
			trace,
			&expectation,
		);
		assert!(report.passed, "regression case failed: {report:?}");
	}

	#[test]
	fn dispatches_to_data_worker_through_tool_runtime() {
		let runtime = GenericAgentRuntime::default();
		let node = node_with_capability("data.read");
		let spec = spec_with_capabilities(vec!["data.read"]);

		let result = runtime.execute(&spec, &node);
		assert_eq!(result.status, ResultStatus::Ok);
		assert_eq!(result.evidence[0].value, "data-worker");
		assert_eq!(result.evidence[1].value, "data.execute");

		let payload = payload_value(&result);
		assert_eq!(payload["worker_id"], "data-worker");
		assert_eq!(payload["tool_name"], "data.execute");
		assert_eq!(
			payload["message"],
			"deterministic placeholder only: data processing was not executed by a live runtime"
		);
		assert_eq!(payload["output"]["data"]["runtime_mode"], "deterministic");
		assert_eq!(payload["output"]["data"]["placeholder"], true);
	}

	#[test]
	fn dispatches_to_review_worker_through_tool_runtime() {
		let runtime = GenericAgentRuntime::default();
		let node = node_with_capability("review.check");
		let spec = spec_with_capabilities(vec!["review.check"]);

		let result = runtime.execute(&spec, &node);
		assert_eq!(result.status, ResultStatus::Ok);
		assert_eq!(result.evidence[0].value, "review-worker");
		assert_eq!(result.evidence[1].value, "review.assess");
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
		let node = node_with_capability("skill.ensure_installed");
		let mut spec = spec_with_capabilities(vec!["skill.ensure_installed"]);
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
		assert_eq!(result.evidence[1].value, "skill.ensure_installed");
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

	#[test]
	fn reports_tool_runtime_capability_denial_as_error_result() {
		let runtime = GenericAgentRuntime::default();
		let node = node_with_capability("research.analyze");
		let spec = spec_with_capabilities(vec!["research.analyze"]);

		let result = runtime.execute(&spec, &node);
		assert_eq!(result.status, ResultStatus::Error);
		assert!(
			result
				.evidence
				.iter()
				.any(|item| item.kind == "tool_error" && item.value == "capability_denied")
		);
		assert_eq!(payload_value(&result)["error_code"], "capability_denied");
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

	struct FixedLlmProvider;

	impl LlmProvider for FixedLlmProvider {
		fn provider_name(&self) -> &'static str {
			"test-provider"
		}

		fn complete(
			&self,
			_model: &ModelProfile,
			_request: &GenerationRequest,
		) -> Result<ProviderResponse, ProviderCallError> {
			Ok(ProviderResponse {
				output: general_completion_json(
					"live answer from llm",
					"grounded_answer",
					"grounded",
				),
				finish_reason: None,
				prompt_tokens: 32,
				output_tokens: 8,
				latency_ms: 50,
			})
		}
	}

	#[test]
	fn llm_router_runtime_returns_live_message() {
		let mut router = LlmRouter::new(RoutingPolicy {
			max_request_cost_usd: 1.0,
			max_latency_ms: 5_000,
		});
		router.register_provider(FixedLlmProvider);
		router.register_model(ModelProfile {
			model_id: "test-model".to_string(),
			provider: "test-provider".to_string(),
			max_context_tokens: 16_000,
			cost_per_1k_tokens_usd: 0.0,
			max_risk_tier: RiskTier::Critical,
			route_priority: 100,
		});

		let runtime = GenericAgentRuntime::with_llm_router(router);
		let node = TaskNode {
			node_id: NodeId("node-llm".to_string()),
			kind: TaskNodeKind::Execution,
			description: "Goal: say hello\nStep: Execute primary action".to_string(),
			capabilities: vec!["tool.invoke".to_string()],
			join_policy: JoinPolicy::default(),
			aggregation_mode: AggregationMode::default(),
			..TaskNode::default()
		};
		let spec = spec_with_capabilities(vec!["tool.invoke"]);

		let result = runtime.execute(&spec, &node);
		assert_eq!(result.status, ResultStatus::Ok);
		assert_eq!(payload_value(&result)["message"], "live answer from llm");
		assert_eq!(result.evidence[1].value, "general.execute");
	}

	struct MetaLlmProvider;

	impl LlmProvider for MetaLlmProvider {
		fn provider_name(&self) -> &'static str {
			"meta-provider"
		}

		fn complete(
			&self,
			_model: &ModelProfile,
			_request: &GenerationRequest,
		) -> Result<ProviderResponse, ProviderCallError> {
			Ok(ProviderResponse {
				output: general_completion_json("星期日", "grounded_answer", "grounded"),
				finish_reason: None,
				prompt_tokens: 48,
				output_tokens: 64,
				latency_ms: 50,
			})
		}
	}

	#[test]
	fn llm_router_runtime_sanitizes_prompt_leakage_for_final_reply() {
		let mut router = LlmRouter::new(RoutingPolicy {
			max_request_cost_usd: 1.0,
			max_latency_ms: 5_000,
		});
		router.register_provider(MetaLlmProvider);
		router.register_model(ModelProfile {
			model_id: "meta-model".to_string(),
			provider: "meta-provider".to_string(),
			max_context_tokens: 16_000,
			cost_per_1k_tokens_usd: 0.0,
			max_risk_tier: RiskTier::Critical,
			route_priority: 100,
		});

		let runtime = GenericAgentRuntime::with_llm_router(router);
		let node = TaskNode {
			node_id: NodeId("node-meta".to_string()),
			kind: TaskNodeKind::Execution,
			description: "Goal: 今天周几？\nStep: Execute primary action".to_string(),
			capabilities: vec!["tool.invoke".to_string()],
			join_policy: JoinPolicy::default(),
			aggregation_mode: AggregationMode::default(),
			..TaskNode::default()
		};
		let spec = spec_with_capabilities(vec!["tool.invoke"]);

		let result = runtime.execute(&spec, &node);
		assert_eq!(result.status, ResultStatus::Ok);
		let payload = payload_value(&result);
		let message = payload["message"]
			.as_str()
			.expect("message should be a string");
		assert_eq!(message, "星期日");
	}

	struct SequenceJsonProvider {
		prompts: Arc<Mutex<Vec<String>>>,
		responses: Arc<Mutex<VecDeque<String>>>,
	}

	impl LlmProvider for SequenceJsonProvider {
		fn provider_name(&self) -> &'static str {
			"sequence-json-provider"
		}

		fn complete(
			&self,
			_model: &ModelProfile,
			request: &GenerationRequest,
		) -> Result<ProviderResponse, ProviderCallError> {
			self.prompts
				.lock()
				.expect("prompt lock should succeed")
				.push(request.prompt.clone());
			let output = self
				.responses
				.lock()
				.expect("response lock should succeed")
				.pop_front()
				.expect("a canned response should be available");
			Ok(ProviderResponse {
				output,
				finish_reason: None,
				prompt_tokens: 24,
				output_tokens: 18,
				latency_ms: 10,
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

	impl LlmProvider for StaticTextProvider {
		fn provider_name(&self) -> &'static str {
			self.name
		}

		fn complete(
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
				"tool_name": "fs.inspect",
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
			vec!["fs.inspect".to_string()],
			Vec::new(),
			Vec::new(),
			"filesystem request",
		);
		let mut loop_state =
			runtime.initialize_runtime_loop(&request, &request.session_id, &decision, Vec::new());

		let execution = runtime.execute_tool_loop(
			&TaskId("task-loop".to_string()),
			&request,
			&mut loop_state,
			None,
		);

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
		assert_eq!(
			loop_state.history[0].tool_name.as_deref(),
			Some("fs.inspect")
		);
		assert_eq!(loop_state.history[1].action, StepAction::FinalAnswer);
		assert_eq!(
			loop_state.status,
			crate::runtime_loop::LoopStatus::Succeeded
		);
		assert_eq!(
			loop_state.visible_tools.first().map(String::as_str),
			Some("fs.inspect")
		);
		assert!(
			loop_state
				.visible_tools
				.contains(&"general.execute".to_string())
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
		assert!(prompts[0].contains("\"visible_tools\": ["));
		assert!(prompts[0].contains("\"visible_tool_hints\": {"));
		assert!(prompts[0].contains("\"fs.inspect\""));
		assert!(prompts[1].contains("\"history_digest\":"));
		assert!(prompts[1].contains("step 1"));
		assert!(prompts[1].contains("fs.inspect"));
		assert!(!prompts[1].contains("\"started_at\""));
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
			vec!["general.execute".to_string()],
			Vec::new(),
			Vec::new(),
			"chat request",
		);
		let mut loop_state =
			runtime.initialize_runtime_loop(&request, &request.session_id, &decision, Vec::new());

		let execution = runtime.execute_tool_loop(
			&TaskId("task-ask-user".to_string()),
			&request,
			&mut loop_state,
			None,
		);

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
	fn classify_route_routes_unknown_requests_into_generic_loop() {
		let (route_router, _prompts) = router_with_json_responses(vec![serde_json::json!({
			"intent_family": "unknown",
			"confidence": 0.91,
			"requires_multi_step": false,
			"risk": "low",
			"candidate_tools": [],
			"candidate_plugins": [],
			"missing_arguments": [],
			"reason": "The request is too underspecified to classify more narrowly."
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
			request_id: roku_common_types::RequestId("req-unknown".to_string()),
			session_id: "session-unknown".to_string(),
			goal: "帮我做这个事情".to_string(),
			planning_mode_hint: None,
			conversation_history: Vec::new(),
		};

		let route = runtime.classify_route(&request, &request.session_id);

		match route {
			crate::router::RouteDecisionResult::Direct(plan) => {
				assert_eq!(plan.decision.intent_family, IntentFamily::Unknown);
				assert!(plan.decision.candidate_tools.is_empty());
			}
			other => panic!("expected direct loop route, got {other:?}"),
		}
	}

	#[test]
	fn classify_route_demotes_low_confidence_to_tool_loop() {
		let (route_router, _prompts) = router_with_json_responses(vec![serde_json::json!({
			"intent_family": "table_read",
			"confidence": 0.21,
			"requires_multi_step": false,
			"risk": "low",
			"candidate_tools": ["table.schema"],
			"candidate_plugins": [],
			"missing_arguments": [],
			"reason": "Weak signal that the user may want a table operation."
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
			request_id: roku_common_types::RequestId("req-low-confidence".to_string()),
			session_id: "session-low-confidence".to_string(),
			goal: "Maybe inspect a table for me".to_string(),
			planning_mode_hint: None,
			conversation_history: Vec::new(),
		};

		let route = runtime.classify_route(&request, &request.session_id);

		match route {
			crate::router::RouteDecisionResult::Direct(plan) => {
				assert_eq!(plan.decision.intent_family, IntentFamily::TableRead);
				assert!(
					plan.decision
						.candidate_tools
						.contains(&"table.schema".to_string())
				);
				assert_eq!(plan.decision.candidate_tools.len(), 1);
			}
			other => panic!("expected low-confidence request to enter tool loop, got {other:?}"),
		}
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
			vec!["fs.read_text".to_string(), "not.enabled".to_string()],
			vec!["core-fs".to_string()],
			Vec::new(),
			"filesystem request",
		);

		let loop_state =
			runtime.initialize_runtime_loop(&request, &request.session_id, &decision, Vec::new());

		assert_eq!(loop_state.run_id, "loop-req-loop");
		assert_eq!(
			loop_state.visible_tools.first().map(String::as_str),
			Some("fs.read_text")
		);
		assert!(
			loop_state
				.visible_tools
				.contains(&"general.execute".to_string())
		);
		assert!(
			loop_state
				.visible_tools
				.contains(&"inventory.describe".to_string())
		);
		assert!(
			loop_state
				.visible_tools
				.contains(&"table.preview".to_string())
		);
		assert!(loop_state.visible_tools.contains(&"python.run".to_string()));
		assert_eq!(loop_state.history.len(), 0);
	}

	#[test]
	fn classify_route_starts_fs_read_text_for_grounded_filesystem_reads() {
		let runtime = GenericAgentRuntime::default();
		let request = RequestEnvelope {
			request_id: roku_common_types::RequestId("req-fs-tool-loop".to_string()),
			session_id: "session-fs-tool-loop".to_string(),
			goal: "Read the first part of Cargo.toml.".to_string(),
			planning_mode_hint: None,
			conversation_history: Vec::new(),
		};

		let route = runtime.classify_route(&request, &request.session_id);

		match route {
			crate::router::RouteDecisionResult::Direct(plan) => {
				assert_eq!(plan.decision.intent_family, IntentFamily::FilesystemRead);
				assert_eq!(
					plan.decision.candidate_tools,
					vec!["fs.read_text".to_string()]
				);
			}
			other => {
				panic!(
					"expected grounded filesystem read to start with fs.read_text, got {other:?}"
				)
			}
		}
	}

	#[test]
	fn classify_route_prefers_fs_read_text_for_explicit_relative_file_paths() {
		let runtime = GenericAgentRuntime::default();
		let request = RequestEnvelope {
			request_id: roku_common_types::RequestId("req-fs-explicit-path".to_string()),
			session_id: "session-fs-explicit-path".to_string(),
			goal: "Read tmp/phase3-check-read.txt.".to_string(),
			planning_mode_hint: None,
			conversation_history: Vec::new(),
		};

		let route = runtime.classify_route(&request, &request.session_id);

		match route {
			crate::router::RouteDecisionResult::Direct(plan) => {
				assert_eq!(plan.decision.intent_family, IntentFamily::FilesystemRead);
				assert_eq!(
					plan.decision.candidate_tools,
					vec!["fs.read_text".to_string()]
				);
			}
			other => {
				panic!(
					"expected explicit relative file path read to shortlist fs.read_text, got {other:?}"
				)
			}
		}
	}

	#[test]
	fn classify_route_uses_controlled_family_seed_for_explicit_paths_without_action() {
		let runtime = GenericAgentRuntime::default();
		let request = RequestEnvelope {
			request_id: roku_common_types::RequestId("req-fs-broad-explicit-path".to_string()),
			session_id: "session-fs-broad-explicit-path".to_string(),
			goal: "Cargo.toml 这个文件帮我看看情况。".to_string(),
			planning_mode_hint: None,
			conversation_history: Vec::new(),
		};

		let route = runtime.classify_route(&request, &request.session_id);

		match route {
			crate::router::RouteDecisionResult::Direct(plan) => {
				assert_eq!(plan.decision.intent_family, IntentFamily::FilesystemRead);
				assert_eq!(
					plan.decision.candidate_tools,
					vec![
						"fs.inspect".to_string(),
						"fs.read_text".to_string(),
						"fs.list_dir".to_string()
					]
				);
			}
			other => panic!(
				"expected explicit path without a concrete action to keep a controlled filesystem starter set, got {other:?}"
			),
		}
	}

	#[test]
	fn classify_route_uses_lookup_first_family_seed_for_non_concrete_basenames() {
		let runtime = GenericAgentRuntime::default();
		let request = RequestEnvelope {
			request_id: roku_common_types::RequestId("req-fs-non-concrete-basename".to_string()),
			session_id: "session-fs-non-concrete-basename".to_string(),
			goal: "我是说，帮我看看cmd那个crate下的runtime.rs，里面的第100行是什么内容？输出出来"
				.to_string(),
			planning_mode_hint: None,
			conversation_history: Vec::new(),
		};

		let route = runtime.classify_route(&request, &request.session_id);

		match route {
			crate::router::RouteDecisionResult::Direct(plan) => {
				assert_eq!(plan.decision.intent_family, IntentFamily::FilesystemRead);
				assert_eq!(
					plan.decision.candidate_tools,
					vec![
						"fs.find".to_string(),
						"fs.glob".to_string(),
						"fs.inspect".to_string()
					]
				);
			}
			other => panic!(
				"expected non-concrete basename requests to keep a lookup-first starter set, got {other:?}"
			),
		}
	}

	#[test]
	fn classify_route_starts_fs_inspect_for_explicit_inspect_actions() {
		let runtime = GenericAgentRuntime::default();
		let request = RequestEnvelope {
			request_id: roku_common_types::RequestId("req-fs-inspect".to_string()),
			session_id: "session-fs-inspect".to_string(),
			goal: "Inspect Cargo.toml.".to_string(),
			planning_mode_hint: None,
			conversation_history: Vec::new(),
		};

		let route = runtime.classify_route(&request, &request.session_id);

		match route {
			crate::router::RouteDecisionResult::Direct(plan) => {
				assert_eq!(plan.decision.intent_family, IntentFamily::FilesystemRead);
				assert_eq!(
					plan.decision.candidate_tools,
					vec!["fs.inspect".to_string()]
				);
			}
			other => {
				panic!("expected explicit inspect action to start with fs.inspect, got {other:?}")
			}
		}
	}

	#[test]
	fn classify_route_shortlists_inventory_describe_for_inventory_questions() {
		let runtime = GenericAgentRuntime::default();
		let request = RequestEnvelope {
			request_id: roku_common_types::RequestId("req-inventory-tool-loop".to_string()),
			session_id: "session-inventory-tool-loop".to_string(),
			goal: "What skills and tools do you have right now?".to_string(),
			planning_mode_hint: None,
			conversation_history: Vec::new(),
		};

		let route = runtime.classify_route(&request, &request.session_id);

		match route {
			crate::router::RouteDecisionResult::Direct(plan) => {
				assert_eq!(plan.decision.intent_family, IntentFamily::Chat);
				assert_eq!(
					plan.decision.candidate_tools,
					vec!["inventory.describe".to_string()]
				);
			}
			other => {
				panic!("expected inventory question to shortlist inventory.describe, got {other:?}")
			}
		}
	}

	#[test]
	fn explanatory_shell_command_requests_do_not_shortlist_command_run() {
		let runtime = GenericAgentRuntime::default();
		let request = RequestEnvelope {
			request_id: roku_common_types::RequestId("req-command-explain".to_string()),
			session_id: "session-command-explain".to_string(),
			goal: "Explain what the shell command `pwd` does, but do not run it.".to_string(),
			planning_mode_hint: None,
			conversation_history: Vec::new(),
		};

		let route = runtime.classify_route(&request, &request.session_id);

		match route {
			crate::router::RouteDecisionResult::Direct(plan) => {
				assert_eq!(plan.decision.intent_family, IntentFamily::Chat);
				assert_eq!(
					plan.decision.candidate_tools,
					vec!["general.execute".to_string()]
				);
			}
			other => {
				panic!(
					"expected explanatory shell command request to stay on chat route, got {other:?}"
				)
			}
		}
	}

	#[test]
	fn executable_shell_command_requests_can_shortlist_command_run() {
		let runtime = GenericAgentRuntime::default();
		let request = RequestEnvelope {
			request_id: roku_common_types::RequestId("req-command-run".to_string()),
			session_id: "session-command-run".to_string(),
			goal: "Run this command: `pwd`".to_string(),
			planning_mode_hint: None,
			conversation_history: Vec::new(),
		};

		let route = runtime.classify_route(&request, &request.session_id);

		match route {
			crate::router::RouteDecisionResult::Direct(plan) => {
				assert_eq!(plan.decision.intent_family, IntentFamily::CodeExec);
				assert_eq!(
					plan.decision.candidate_tools.first().map(String::as_str),
					Some("command.run")
				);
			}
			other => {
				panic!("expected runnable shell command to shortlist command.run, got {other:?}")
			}
		}
	}

	#[test]
	fn explanatory_python_code_requests_do_not_shortlist_python_run() {
		let runtime = GenericAgentRuntime::default();
		let request = RequestEnvelope {
			request_id: roku_common_types::RequestId("req-python-explain".to_string()),
			session_id: "session-python-explain".to_string(),
			goal: "Explain what this Python code does: `print(1)`".to_string(),
			planning_mode_hint: None,
			conversation_history: Vec::new(),
		};

		let route = runtime.classify_route(&request, &request.session_id);

		match route {
			crate::router::RouteDecisionResult::Direct(plan) => {
				assert_eq!(plan.decision.intent_family, IntentFamily::Chat);
				assert_eq!(
					plan.decision.candidate_tools,
					vec!["general.execute".to_string()]
				);
			}
			other => {
				panic!(
					"expected explanatory Python code request to stay on chat route, got {other:?}"
				)
			}
		}
	}

	#[test]
	fn executable_python_code_requests_can_shortlist_python_run() {
		let runtime = GenericAgentRuntime::default();
		let request = RequestEnvelope {
			request_id: roku_common_types::RequestId("req-python-run".to_string()),
			session_id: "session-python-run".to_string(),
			goal: "Run this Python code: `print(1)`".to_string(),
			planning_mode_hint: None,
			conversation_history: Vec::new(),
		};

		let route = runtime.classify_route(&request, &request.session_id);

		match route {
			crate::router::RouteDecisionResult::Direct(plan) => {
				assert_eq!(plan.decision.intent_family, IntentFamily::CodeExec);
				assert_eq!(
					plan.decision.candidate_tools.first().map(String::as_str),
					Some("python.run")
				);
			}
			other => {
				panic!("expected runnable Python code to shortlist python.run, got {other:?}")
			}
		}
	}

	#[test]
	fn explicit_web_queries_can_shortlist_web_search() {
		let runtime = GenericAgentRuntime::default();
		let request = RequestEnvelope {
			request_id: roku_common_types::RequestId("req-web-search".to_string()),
			session_id: "session-web-search".to_string(),
			goal: "Search the web for the latest Rust edition.".to_string(),
			planning_mode_hint: None,
			conversation_history: Vec::new(),
		};

		let route = runtime.classify_route(&request, &request.session_id);

		match route {
			crate::router::RouteDecisionResult::Direct(plan) => {
				assert_eq!(plan.decision.intent_family, IntentFamily::WebLookup);
				assert_eq!(
					plan.decision.candidate_tools.first().map(String::as_str),
					Some("web.search")
				);
			}
			other => panic!("expected explicit web query to shortlist web.search, got {other:?}"),
		}
	}

	#[test]
	fn incomplete_python_execution_requests_ask_for_code() {
		let runtime = GenericAgentRuntime::default();
		let request = RequestEnvelope {
			request_id: roku_common_types::RequestId("req-python-missing-code".to_string()),
			session_id: "session-python-missing-code".to_string(),
			goal: "Run this Python code.".to_string(),
			planning_mode_hint: None,
			conversation_history: Vec::new(),
		};

		let route = runtime.classify_route(&request, &request.session_id);

		match route {
			crate::router::RouteDecisionResult::Escalate(plan) => {
				assert_eq!(plan.action, crate::router::EscalationAction::AskForMoreInfo);
				assert_eq!(plan.decision.intent_family, IntentFamily::CodeExec);
				assert_eq!(plan.decision.missing_arguments, vec!["code".to_string()]);
			}
			other => {
				panic!(
					"expected incomplete Python execution request to ask for code, got {other:?}"
				)
			}
		}
	}

	#[test]
	fn incomplete_web_lookup_requests_ask_for_query() {
		let runtime = GenericAgentRuntime::default();
		let request = RequestEnvelope {
			request_id: roku_common_types::RequestId("req-web-missing-query".to_string()),
			session_id: "session-web-missing-query".to_string(),
			goal: "Search the web.".to_string(),
			planning_mode_hint: None,
			conversation_history: Vec::new(),
		};

		let route = runtime.classify_route(&request, &request.session_id);

		match route {
			crate::router::RouteDecisionResult::Escalate(plan) => {
				assert_eq!(plan.action, crate::router::EscalationAction::AskForMoreInfo);
				assert_eq!(plan.decision.intent_family, IntentFamily::WebLookup);
				assert_eq!(plan.decision.missing_arguments, vec!["query".to_string()]);
			}
			other => {
				panic!("expected incomplete web lookup request to ask for query, got {other:?}")
			}
		}
	}

	#[test]
	fn classify_route_shortlists_table_preview_for_grounded_table_requests() {
		let runtime = GenericAgentRuntime::default();
		let request = RequestEnvelope {
			request_id: roku_common_types::RequestId("req-table-tool-loop".to_string()),
			session_id: "session-table-tool-loop".to_string(),
			goal: "Preview the first rows of tmp/example.csv.".to_string(),
			planning_mode_hint: None,
			conversation_history: Vec::new(),
		};

		let route = runtime.classify_route(&request, &request.session_id);

		match route {
			crate::router::RouteDecisionResult::Direct(plan) => {
				assert_eq!(plan.decision.intent_family, IntentFamily::TableRead);
				assert_eq!(
					plan.decision.candidate_tools,
					vec!["table.preview".to_string()]
				);
			}
			other => {
				panic!("expected grounded table request to shortlist table.preview, got {other:?}")
			}
		}
	}

	#[test]
	fn classify_route_uses_controlled_family_seed_for_explicit_tables_without_action() {
		let runtime = GenericAgentRuntime::default();
		let request = RequestEnvelope {
			request_id: roku_common_types::RequestId("req-table-broad-explicit-path".to_string()),
			session_id: "session-table-broad-explicit-path".to_string(),
			goal: "tmp/example.csv 这个文件帮我处理一下。".to_string(),
			planning_mode_hint: None,
			conversation_history: Vec::new(),
		};

		let route = runtime.classify_route(&request, &request.session_id);

		match route {
			crate::router::RouteDecisionResult::Direct(plan) => {
				assert_eq!(plan.decision.intent_family, IntentFamily::TableRead);
				assert_eq!(
					plan.decision.candidate_tools,
					vec![
						"table.inspect".to_string(),
						"table.preview".to_string(),
						"table.list_sheets".to_string()
					]
				);
			}
			other => panic!(
				"expected explicit table path without a concrete action to keep a controlled table starter set, got {other:?}"
			),
		}
	}

	#[test]
	fn classify_route_shortlists_fs_glob_for_explicit_glob_requests() {
		let runtime = GenericAgentRuntime::default();
		let request = RequestEnvelope {
			request_id: roku_common_types::RequestId("req-glob-tool-loop".to_string()),
			session_id: "session-glob-tool-loop".to_string(),
			goal: "Match `src/*.rs` in this workspace.".to_string(),
			planning_mode_hint: None,
			conversation_history: Vec::new(),
		};

		let route = runtime.classify_route(&request, &request.session_id);

		match route {
			crate::router::RouteDecisionResult::Direct(plan) => {
				assert_eq!(plan.decision.intent_family, IntentFamily::FilesystemRead);
				assert_eq!(plan.decision.candidate_tools, vec!["fs.glob".to_string()]);
			}
			other => panic!("expected explicit glob request to shortlist fs.glob, got {other:?}"),
		}
	}

	#[test]
	fn runtime_loop_confusion_suite_covers_grounded_and_quoted_requests() {
		let runtime = runtime_with_fixed_general_llm();
		let deterministic_runtime = GenericAgentRuntime::default();
		let csv_path = regression_fixture_path(".csv", "name,count\nalpha,1\nbeta,2\n");

		let inventory_trace = runtime_loop_trace_for_goal(
			&deterministic_runtime,
			"What skills and tools do you have right now?",
		);
		assert_regression_case(
			"inventory-question",
			crate::runtime_loop::RegressionSuiteKind::Confusion,
			&inventory_trace,
			crate::runtime_loop::RuntimeLoopRegressionExpectation {
				expected_tool: Some("inventory.describe".to_string()),
				forbidden_tools: vec!["general.execute".to_string()],
				expected_terminal_action: Some("final_answer".to_string()),
				expected_error_type: None,
				interpreted_flags: vec![crate::runtime_loop::InterpretedFlagExpectation {
					field: "should_emit_final_answer".to_string(),
					expected: true,
				}],
			},
		);

		let quoted_trace = runtime_loop_trace_for_goal(
			&runtime,
			"你说的这个是什么意思？ task failed: The runtime loop needs an explicit next-step decision after the non-terminal fs.list_dir observation.",
		);
		let quoted_trace_ref = &quoted_trace;
		assert_regression_case(
			"quoted-tool-name",
			crate::runtime_loop::RegressionSuiteKind::Confusion,
			quoted_trace_ref,
			crate::runtime_loop::RuntimeLoopRegressionExpectation {
				expected_tool: Some("general.execute".to_string()),
				forbidden_tools: vec!["inventory.describe".to_string()],
				expected_terminal_action: Some("final_answer".to_string()),
				expected_error_type: None,
				interpreted_flags: Vec::new(),
			},
		);
		let do_not_run_trace = runtime_loop_trace_for_goal(
			&runtime,
			"Explain what the shell command `pwd` does, but do not run it.",
		);
		assert_regression_case(
			"quoted-shell-command-explanation",
			crate::runtime_loop::RegressionSuiteKind::Confusion,
			&do_not_run_trace,
			crate::runtime_loop::RuntimeLoopRegressionExpectation {
				expected_tool: Some("general.execute".to_string()),
				forbidden_tools: vec!["command.run".to_string()],
				expected_terminal_action: Some("final_answer".to_string()),
				expected_error_type: None,
				interpreted_flags: vec![crate::runtime_loop::InterpretedFlagExpectation {
					field: "should_emit_final_answer".to_string(),
					expected: true,
				}],
			},
		);
		let python_explanation_trace =
			runtime_loop_trace_for_goal(&runtime, "Explain what this Python code does: `print(1)`");
		assert_regression_case(
			"quoted-python-code-explanation",
			crate::runtime_loop::RegressionSuiteKind::Confusion,
			&python_explanation_trace,
			crate::runtime_loop::RuntimeLoopRegressionExpectation {
				expected_tool: Some("general.execute".to_string()),
				forbidden_tools: vec!["python.run".to_string()],
				expected_terminal_action: Some("final_answer".to_string()),
				expected_error_type: None,
				interpreted_flags: vec![crate::runtime_loop::InterpretedFlagExpectation {
					field: "should_emit_final_answer".to_string(),
					expected: true,
				}],
			},
		);
		let web_tool_explanation_trace =
			runtime_loop_trace_for_goal(&runtime, "Explain what the tool web.search does.");
		assert_regression_case(
			"web-tool-explanation",
			crate::runtime_loop::RegressionSuiteKind::Confusion,
			&web_tool_explanation_trace,
			crate::runtime_loop::RuntimeLoopRegressionExpectation {
				expected_tool: Some("general.execute".to_string()),
				forbidden_tools: vec!["web.search".to_string()],
				expected_terminal_action: Some("final_answer".to_string()),
				expected_error_type: None,
				interpreted_flags: vec![crate::runtime_loop::InterpretedFlagExpectation {
					field: "should_emit_final_answer".to_string(),
					expected: true,
				}],
			},
		);

		let table_trace = runtime_loop_trace_for_goal(
			&runtime,
			&format!("Preview the first rows of {csv_path}."),
		);
		assert_regression_case(
			"grounded-table-preview",
			crate::runtime_loop::RegressionSuiteKind::Confusion,
			&table_trace,
			crate::runtime_loop::RuntimeLoopRegressionExpectation {
				expected_tool: Some("table.preview".to_string()),
				forbidden_tools: Vec::new(),
				expected_terminal_action: Some("final_answer".to_string()),
				expected_error_type: None,
				interpreted_flags: vec![crate::runtime_loop::InterpretedFlagExpectation {
					field: "continue_allowed".to_string(),
					expected: true,
				}],
			},
		);

		cleanup_fixture(&csv_path);
	}

	#[test]
	fn runtime_loop_boundary_suite_covers_contract_edges() {
		let runtime = GenericAgentRuntime::default();
		let duplicate_root = env::current_dir()
			.expect("cwd should resolve for regression fixtures")
			.join("tmp");
		fs::create_dir_all(&duplicate_root).expect("tmp fixture directory should exist");
		let duplicate_scope = Builder::new()
			.prefix("phase3-duplicate-")
			.tempdir_in(&duplicate_root)
			.expect("duplicate fixture root should exist");
		let scope_name = duplicate_scope
			.path()
			.file_name()
			.and_then(|value| value.to_str())
			.expect("duplicate fixture directory should expose a name");
		let duplicate_name = format!("{scope_name}-target.txt");
		let duplicate_a_dir = duplicate_scope.path().join("a");
		let duplicate_b_dir = duplicate_scope.path().join("b");
		fs::create_dir_all(&duplicate_a_dir).expect("duplicate fixture dir A should exist");
		fs::create_dir_all(&duplicate_b_dir).expect("duplicate fixture dir B should exist");
		let duplicate_a_path = duplicate_a_dir.join(&duplicate_name);
		let duplicate_b_path = duplicate_b_dir.join(&duplicate_name);
		fs::write(&duplicate_a_path, "duplicate a\n").expect("duplicate fixture A should write");
		fs::write(&duplicate_b_path, "duplicate b\n").expect("duplicate fixture B should write");

		let command_trace =
			runtime_loop_trace_for_goal(&runtime, "Run this command: `touch phase3-boundary.tmp`");
		assert_regression_case(
			"command-write-boundary",
			crate::runtime_loop::RegressionSuiteKind::Boundary,
			&command_trace,
			crate::runtime_loop::RuntimeLoopRegressionExpectation {
				expected_tool: Some("command.run".to_string()),
				forbidden_tools: Vec::new(),
				expected_terminal_action: Some("fail".to_string()),
				expected_error_type: Some("command_not_allowed".to_string()),
				interpreted_flags: vec![crate::runtime_loop::InterpretedFlagExpectation {
					field: "should_fail".to_string(),
					expected: true,
				}],
			},
		);

		let ambiguous_find_trace = runtime_loop_trace_for_goal(
			&runtime,
			&format!("Find {duplicate_name} in this workspace."),
		);
		assert_regression_case(
			"ambiguous-fs-find",
			crate::runtime_loop::RegressionSuiteKind::Boundary,
			&ambiguous_find_trace,
			crate::runtime_loop::RuntimeLoopRegressionExpectation {
				expected_tool: Some("fs.find".to_string()),
				forbidden_tools: Vec::new(),
				expected_terminal_action: Some("ask_user".to_string()),
				expected_error_type: Some("multiple_candidates".to_string()),
				interpreted_flags: vec![crate::runtime_loop::InterpretedFlagExpectation {
					field: "continue_allowed".to_string(),
					expected: true,
				}],
			},
		);

		let missing_find_trace = runtime_loop_trace_for_goal(
			&runtime,
			"Find definitely-no-such-file-42.txt in this workspace.",
		);
		assert_regression_case(
			"missing-fs-find",
			crate::runtime_loop::RegressionSuiteKind::Boundary,
			&missing_find_trace,
			crate::runtime_loop::RuntimeLoopRegressionExpectation {
				expected_tool: Some("fs.find".to_string()),
				forbidden_tools: Vec::new(),
				expected_terminal_action: Some("fail".to_string()),
				expected_error_type: Some("path_not_found".to_string()),
				interpreted_flags: vec![crate::runtime_loop::InterpretedFlagExpectation {
					field: "should_fail".to_string(),
					expected: true,
				}],
			},
		);
		let python_non_zero_trace =
			runtime_loop_trace_for_goal(&runtime, "Run this Python code: `raise SystemExit(3)`");
		assert_regression_case(
			"python-non-zero-exit",
			crate::runtime_loop::RegressionSuiteKind::Boundary,
			&python_non_zero_trace,
			crate::runtime_loop::RuntimeLoopRegressionExpectation {
				expected_tool: Some("python.run".to_string()),
				forbidden_tools: Vec::new(),
				expected_terminal_action: Some("fail".to_string()),
				expected_error_type: Some("non_zero_exit".to_string()),
				interpreted_flags: vec![crate::runtime_loop::InterpretedFlagExpectation {
					field: "should_fail".to_string(),
					expected: true,
				}],
			},
		);
		let web_missing_endpoint_trace =
			runtime_loop_trace_for_goal(&runtime, "Search the web for the latest Rust edition.");
		assert_regression_case(
			"web-search-endpoint-missing",
			crate::runtime_loop::RegressionSuiteKind::Boundary,
			&web_missing_endpoint_trace,
			crate::runtime_loop::RuntimeLoopRegressionExpectation {
				expected_tool: Some("web.search".to_string()),
				forbidden_tools: Vec::new(),
				expected_terminal_action: Some("fail".to_string()),
				expected_error_type: Some("endpoint_not_configured".to_string()),
				interpreted_flags: vec![crate::runtime_loop::InterpretedFlagExpectation {
					field: "should_fail".to_string(),
					expected: true,
				}],
			},
		);

		cleanup_fixture(duplicate_a_path.to_string_lossy().as_ref());
		cleanup_fixture(duplicate_b_path.to_string_lossy().as_ref());
	}

	#[test]
	fn runtime_loop_output_interpretation_suite_covers_terminal_and_non_terminal_success() {
		let runtime = runtime_with_fixed_general_llm();
		let deterministic_runtime = GenericAgentRuntime::default();
		let text_path = regression_fixture_path(".txt", "phase3 interpretation smoke\n");

		let command_trace = runtime_loop_trace_for_goal(&runtime, "Run this command: `pwd`");
		assert_regression_case(
			"command-non-terminal-success",
			crate::runtime_loop::RegressionSuiteKind::OutputInterpretation,
			&command_trace,
			crate::runtime_loop::RuntimeLoopRegressionExpectation {
				expected_tool: Some("command.run".to_string()),
				forbidden_tools: Vec::new(),
				expected_terminal_action: Some("final_answer".to_string()),
				expected_error_type: None,
				interpreted_flags: vec![
					crate::runtime_loop::InterpretedFlagExpectation {
						field: "continue_allowed".to_string(),
						expected: true,
					},
					crate::runtime_loop::InterpretedFlagExpectation {
						field: "should_emit_final_answer".to_string(),
						expected: false,
					},
				],
			},
		);
		let python_trace = runtime_loop_trace_for_goal(
			&runtime,
			"Run this Python code: `print(sum(range(1, 6)))`",
		);
		assert_regression_case(
			"python-run-non-terminal-success",
			crate::runtime_loop::RegressionSuiteKind::OutputInterpretation,
			&python_trace,
			crate::runtime_loop::RuntimeLoopRegressionExpectation {
				expected_tool: Some("python.run".to_string()),
				forbidden_tools: Vec::new(),
				expected_terminal_action: Some("final_answer".to_string()),
				expected_error_type: None,
				interpreted_flags: vec![
					crate::runtime_loop::InterpretedFlagExpectation {
						field: "continue_allowed".to_string(),
						expected: true,
					},
					crate::runtime_loop::InterpretedFlagExpectation {
						field: "should_emit_final_answer".to_string(),
						expected: false,
					},
				],
			},
		);

		let inventory_trace = runtime_loop_trace_for_goal(
			&deterministic_runtime,
			"What skills and tools do you have right now?",
		);
		assert_regression_case(
			"inventory-terminal-success",
			crate::runtime_loop::RegressionSuiteKind::OutputInterpretation,
			&inventory_trace,
			crate::runtime_loop::RuntimeLoopRegressionExpectation {
				expected_tool: Some("inventory.describe".to_string()),
				forbidden_tools: Vec::new(),
				expected_terminal_action: Some("final_answer".to_string()),
				expected_error_type: None,
				interpreted_flags: vec![
					crate::runtime_loop::InterpretedFlagExpectation {
						field: "continue_allowed".to_string(),
						expected: false,
					},
					crate::runtime_loop::InterpretedFlagExpectation {
						field: "should_emit_final_answer".to_string(),
						expected: true,
					},
				],
			},
		);
		let endpoint = spawn_mock_web_search_server(
			r#"{"results":[{"title":"Rust Edition","url":"https://example.test/rust","snippet":"2024 edition"}]}"#,
		);
		let mut tools_runtime_config = ToolsRuntimeConfig::default();
		tools_runtime_config.web.endpoint = Some(endpoint);
		let web_runtime = runtime_with_fixed_general_llm_and_tools_config(tools_runtime_config);
		let web_trace = runtime_loop_trace_for_goal(
			&web_runtime,
			"Search the web for the latest Rust edition.",
		);
		assert_regression_case(
			"web-search-non-terminal-success",
			crate::runtime_loop::RegressionSuiteKind::OutputInterpretation,
			&web_trace,
			crate::runtime_loop::RuntimeLoopRegressionExpectation {
				expected_tool: Some("web.search".to_string()),
				forbidden_tools: Vec::new(),
				expected_terminal_action: Some("final_answer".to_string()),
				expected_error_type: None,
				interpreted_flags: vec![
					crate::runtime_loop::InterpretedFlagExpectation {
						field: "continue_allowed".to_string(),
						expected: true,
					},
					crate::runtime_loop::InterpretedFlagExpectation {
						field: "should_emit_final_answer".to_string(),
						expected: false,
					},
				],
			},
		);

		let read_trace =
			runtime_loop_trace_for_goal(&runtime, &format!("Read the first part of {text_path}."));
		assert_regression_case(
			"fs-read-text-non-terminal-success",
			crate::runtime_loop::RegressionSuiteKind::OutputInterpretation,
			&read_trace,
			crate::runtime_loop::RuntimeLoopRegressionExpectation {
				expected_tool: Some("fs.read_text".to_string()),
				forbidden_tools: Vec::new(),
				expected_terminal_action: Some("final_answer".to_string()),
				expected_error_type: None,
				interpreted_flags: vec![
					crate::runtime_loop::InterpretedFlagExpectation {
						field: "continue_allowed".to_string(),
						expected: true,
					},
					crate::runtime_loop::InterpretedFlagExpectation {
						field: "should_emit_final_answer".to_string(),
						expected: false,
					},
				],
			},
		);
		let plain_read_trace = runtime_loop_trace_for_goal(&runtime, &format!("Read {text_path}."));
		assert_regression_case(
			"plain-read-text",
			crate::runtime_loop::RegressionSuiteKind::OutputInterpretation,
			&plain_read_trace,
			crate::runtime_loop::RuntimeLoopRegressionExpectation {
				expected_tool: Some("fs.read_text".to_string()),
				forbidden_tools: vec!["fs.find".to_string()],
				expected_terminal_action: Some("final_answer".to_string()),
				expected_error_type: None,
				interpreted_flags: vec![crate::runtime_loop::InterpretedFlagExpectation {
					field: "continue_allowed".to_string(),
					expected: true,
				}],
			},
		);

		cleanup_fixture(&text_path);
	}

	#[test]
	fn execute_tool_loop_can_switch_tools_after_an_insufficient_lookup_observation() {
		let runtime = runtime_with_fixed_general_llm();
		let text_path = regression_fixture_path(".txt", "react recovery fixture\n");
		let file_name = PathBuf::from(&text_path)
			.file_name()
			.and_then(|value| value.to_str())
			.expect("fixture file name should resolve")
			.to_string();
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
			vec!["fs.find".to_string(), "fs.read_text".to_string()],
			vec!["core-fs".to_string()],
			Vec::new(),
			"seed the loop with a lookup-first filesystem hint",
		);
		let mut loop_state =
			runtime.initialize_runtime_loop(&request, &request.session_id, &decision, Vec::new());
		let task_id = TaskId("task-react-recovery".to_string());
		let result = runtime.execute_tool_loop(&task_id, &request, &mut loop_state, None);

		let tool_sequence = loop_state
			.history
			.iter()
			.filter_map(|step| step.tool_name.clone())
			.collect::<Vec<_>>();

		assert_eq!(
			tool_sequence,
			vec![
				"fs.find".to_string(),
				"fs.read_text".to_string(),
				"general.execute".to_string(),
			]
		);
		assert_eq!(result.terminal_step_action, Some(StepAction::FinalAnswer));
		cleanup_fixture(&text_path);
	}

	#[test]
	fn refresh_tool_loop_projection_includes_semantic_tool_hints() {
		let runtime = GenericAgentRuntime::default();
		let request = RequestEnvelope {
			request_id: roku_common_types::RequestId("req-tool-hints".to_string()),
			session_id: "session-tool-hints".to_string(),
			goal: "Count Cargo.toml files in the workspace".to_string(),
			planning_mode_hint: None,
			conversation_history: Vec::new(),
		};
		let decision = crate::router::RouteDecision::new(
			IntentFamily::FilesystemRead,
			0.9,
			false,
			crate::router::RouteRisk::Low,
			vec!["fs.glob".to_string()],
			vec!["core-fs".to_string()],
			Vec::new(),
			"filesystem request",
		);
		let mut loop_state =
			runtime.initialize_runtime_loop(&request, &request.session_id, &decision, Vec::new());

		let projection = runtime.refresh_tool_loop_projection(&mut loop_state);

		let glob_hint = projection
			.visible_tool_hints
			.get("fs.glob")
			.expect("fs.glob hint should be present");
		assert!(glob_hint.selection_hint.contains("glob pattern"));
		assert!(
			glob_hint
				.required_argument_keys
				.contains(&"pattern".to_string())
		);
		assert!(glob_hint.selection_hint.chars().count() <= 180);
	}

	#[test]
	fn quoted_tool_names_do_not_force_a_contract_hint_without_grounded_arguments() {
		let runtime = GenericAgentRuntime::default();
		let request = RequestEnvelope {
			request_id: roku_common_types::RequestId("req-tool-quote".to_string()),
			session_id: "session-tool-quote".to_string(),
			goal: "你说的这个是什么意思？ task failed: The runtime loop needs an explicit next-step decision after the non-terminal fs.list_dir observation.".to_string(),
			planning_mode_hint: None,
			conversation_history: Vec::new(),
		};

		let route = runtime.classify_route(&request, &request.session_id);

		match route {
			crate::router::RouteDecisionResult::Direct(plan) => {
				assert_eq!(plan.decision.intent_family, IntentFamily::Chat);
				assert_eq!(
					plan.decision.candidate_tools,
					vec!["general.execute".to_string()]
				);
			}
			other => {
				panic!("expected explanatory tool-name quote to stay on chat route, got {other:?}")
			}
		}
	}

	#[test]
	fn visible_tools_recompute_keeps_shortlist_and_safe_baseline_after_tool_steps() {
		let runtime = GenericAgentRuntime::default();
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
			vec!["fs.read_text".to_string()],
			vec!["core-fs".to_string()],
			Vec::new(),
			"filesystem request",
		);
		let mut loop_state =
			runtime.initialize_runtime_loop(&request, &request.session_id, &decision, Vec::new());
		let observation = ToolObservation {
			ok: true,
			tool_name: "fs.read_text".to_string(),
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
				tool_name: Some("fs.read_text".to_string()),
				arguments: Some(serde_json::json!({ "path": "Cargo.toml" })),
				reason: "Read the grounded workspace manifest first.".to_string(),
				final_message: None,
			},
			loop_state.visible_tools.clone(),
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

		assert_eq!(
			visible_tools.first().map(String::as_str),
			Some("fs.read_text")
		);
		assert!(visible_tools.contains(&"general.execute".to_string()));
		assert!(visible_tools.contains(&"table.preview".to_string()));
		assert!(visible_tools.contains(&"python.run".to_string()));
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
			vec!["general.execute".to_string()],
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
	fn general_execute_structured_insufficient_evidence_becomes_non_terminal() {
		let normalized = normalize_tool_loop_observation(ToolObservation {
			ok: true,
			tool_name: "general.execute".to_string(),
			error_type: None,
			terminal: true,
			data: json!({
				"completion": {
					"final_message": "当前还没有得到完成这个请求所需的实际执行证据。",
					"completion_kind": "insufficient_evidence",
					"evidence_status": "missing_execution_evidence",
					"missing_information": [],
				}
			}),
			message: "placeholder".to_string(),
		});

		assert!(!normalized.ok);
		assert_eq!(
			normalized.error_type.as_deref(),
			Some("insufficient_evidence")
		);
		assert!(!normalized.terminal);
	}

	#[test]
	fn general_execute_structured_grounded_answer_stays_terminal() {
		let normalized = normalize_tool_loop_observation(ToolObservation {
			ok: false,
			tool_name: "general.execute".to_string(),
			error_type: Some("insufficient_evidence".to_string()),
			terminal: false,
			data: json!({
				"completion": {
					"final_message": "它负责从当前请求里提取显式资源线索并做参数对齐。",
					"completion_kind": "grounded_answer",
					"evidence_status": "grounded",
					"missing_information": [],
				}
			}),
			message: "placeholder".to_string(),
		});

		assert!(normalized.ok);
		assert!(normalized.terminal);
		assert_eq!(
			normalized.message,
			"它负责从当前请求里提取显式资源线索并做参数对齐。"
		);
	}

	#[test]
	fn general_execute_structured_clarification_becomes_needs_more_information() {
		let normalized = normalize_tool_loop_observation(ToolObservation {
			ok: true,
			tool_name: "general.execute".to_string(),
			error_type: None,
			terminal: true,
			data: json!({
				"completion": {
					"final_message": "我需要知道您想统计哪个项目的代码行数。请提供项目的目录路径或项目名称。",
					"completion_kind": "needs_more_information",
					"evidence_status": "missing_required_input",
					"missing_information": ["project_path"],
				}
			}),
			message: "placeholder".to_string(),
		});

		assert!(!normalized.ok);
		assert_eq!(
			normalized.error_type.as_deref(),
			Some("needs_more_information")
		);
		assert!(!normalized.terminal);
	}

	fn general_completion_json(
		final_message: &str,
		completion_kind: &str,
		evidence_status: &str,
	) -> String {
		json!({
			"final_message": final_message,
			"completion_kind": completion_kind,
			"evidence_status": evidence_status,
			"missing_information": [],
		})
		.to_string()
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
