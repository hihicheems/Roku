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
use std::sync::{Arc, Mutex};

use crate::result::policy_rejection_result;
use crate::router::{
	DirectRouteExecutionResult, DirectRouteKind, DirectRoutePlan, EscalationAction, IntentFamily,
	RouteClassifierContext, RouteDecisionResult, RouteScratchpad, classify_request,
};
use crate::tool_config::ToolCatalogConfig;
use crate::tools::{
	build_builtin_tool_runtime_with_plugin_snapshot, build_llm_tool_runtime_with_plugin_snapshot,
	build_resource_catalog_with_plugin_snapshot,
};
use crate::workers::{
	data_worker_with_config, generic_worker_with_config, inventory_worker_with_config,
	research_worker_with_config, review_worker_with_config, skill_execute_worker_with_config,
	skill_worker_with_config,
};
use roku_common_types::{
	AgentContext, AggregationMode, EvidenceItem, JoinPolicy, NodeBudgetSnapshot, NodeId,
	PolicyBindings, RequestEnvelope, RerunPolicy, ResourceSelector, ResultStatus, RetryPolicy,
	TaskId, TaskNodeDispatchPolicy, TaskNodeKind,
};
use roku_common_types::{AgentInstanceSpec, ResultEnvelope, TaskNode};
use roku_plugin_catalog::ResourceCatalog;
use roku_plugin_core::PluginRegistrySnapshot;
use roku_plugin_host::ToolRuntime;
use roku_plugin_llm::LlmRouter;
use roku_plugin_skills::SkillRegistry;
use serde_json::json;

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
	scratchpads: Mutex<HashMap<String, RouteScratchpad>>,
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
		let shared_tool_runtime = Arc::new(tool_runtime);
		let mut runtime = Self {
			workers: Vec::new(),
			tool_runtime: Arc::clone(&shared_tool_runtime),
			resource_catalog,
			tool_config: tool_config.clone(),
			plugin_snapshot,
			route_router: None,
			scratchpads: Mutex::new(HashMap::new()),
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
		)
	}

	pub fn with_skill_registry_tool_config_and_plugin_snapshot(
		skill_registry: SkillRegistry,
		tool_config: ToolCatalogConfig,
		plugin_snapshot: PluginRegistrySnapshot,
	) -> Self {
		let resource_catalog = build_resource_catalog_with_plugin_snapshot(
			&skill_registry,
			&tool_config,
			&plugin_snapshot,
		);
		Self::with_tool_runtime_and_plugin_snapshot(
			build_builtin_tool_runtime_with_plugin_snapshot(
				skill_registry,
				&tool_config,
				&plugin_snapshot,
			),
			resource_catalog,
			tool_config,
			plugin_snapshot,
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
		)
	}

	pub fn with_llm_router_skill_registry_tool_config_and_plugin_snapshot(
		router: LlmRouter,
		skill_registry: SkillRegistry,
		tool_config: ToolCatalogConfig,
		plugin_snapshot: PluginRegistrySnapshot,
	) -> Self {
		let shared_router = Arc::new(router);
		Self::with_llm_execution_and_route_routers(
			Arc::clone(&shared_router),
			shared_router,
			skill_registry,
			tool_config,
			plugin_snapshot,
		)
	}

	pub fn with_route_and_execution_routers_skill_registry_tool_config_and_plugin_snapshot(
		route_router: LlmRouter,
		execution_router: LlmRouter,
		skill_registry: SkillRegistry,
		tool_config: ToolCatalogConfig,
		plugin_snapshot: PluginRegistrySnapshot,
	) -> Self {
		Self::with_llm_execution_and_route_routers(
			Arc::new(execution_router),
			Arc::new(route_router),
			skill_registry,
			tool_config,
			plugin_snapshot,
		)
	}

	fn with_llm_execution_and_route_routers(
		execution_router: Arc<LlmRouter>,
		route_router: Arc<LlmRouter>,
		skill_registry: SkillRegistry,
		tool_config: ToolCatalogConfig,
		plugin_snapshot: PluginRegistrySnapshot,
	) -> Self {
		let resource_catalog = build_resource_catalog_with_plugin_snapshot(
			&skill_registry,
			&tool_config,
			&plugin_snapshot,
		);
		Self::with_tool_runtime_and_plugin_snapshot(
			build_llm_tool_runtime_with_plugin_snapshot(
				Arc::clone(&execution_router),
				skill_registry,
				&tool_config,
				&resource_catalog,
				&plugin_snapshot,
			),
			resource_catalog,
			tool_config,
			plugin_snapshot,
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
		session_id: &str,
	) -> RouteDecisionResult {
		let result = classify_request(
			RouteClassifierContext {
				catalog: &self.resource_catalog,
				tool_config: &self.tool_config,
				plugin_snapshot: &self.plugin_snapshot,
				route_router: self.route_router.as_deref(),
			},
			request,
		);
		self.remember_route_decision(session_id, &result);
		result
	}

	pub fn execute_direct_route(
		&self,
		task_id: &TaskId,
		request: &RequestEnvelope,
		plan: &DirectRoutePlan,
	) -> DirectRouteExecutionResult {
		let execution = match &plan.kind {
			DirectRouteKind::Inventory if self.route_router.is_none() => self
				.synthetic_message_result(
					task_id,
					"direct-route",
					"direct-route:inventory",
					render_inventory_message(&self.resource_catalog, &request.goal),
					0.96,
				),
			DirectRouteKind::Conversation if self.route_router.is_none() => self
				.synthetic_message_result(
					task_id,
					"direct-route",
					"direct-route:conversation",
					deterministic_chat_message(&request.goal),
					0.82,
				),
			DirectRouteKind::SkillInstall { source_url } => self.execute_tool_like_route(
				task_id,
				request,
				"direct-route",
				"Install requested skill package directly",
				vec![ResourceSelector::tool(tool_name_for_role(
					&self.tool_config,
					crate::tool_config::BuiltinToolRole::SkillInstall,
				))],
				Some(source_url),
			),
			DirectRouteKind::SkillAdvisory { selector } => self.execute_tool_like_route(
				task_id,
				request,
				"direct-route",
				&format!(
					"Use advisory skill `{}` as authoritative local guidance",
					selector.name()
				),
				vec![selector.clone()],
				None,
			),
			DirectRouteKind::SkillExecutable { selector } => self.execute_tool_like_route(
				task_id,
				request,
				"direct-route",
				&format!(
					"Execute installed skill `{}` using its local scripts",
					selector.name()
				),
				vec![
					ResourceSelector::tool(tool_name_for_role(
						&self.tool_config,
						crate::tool_config::BuiltinToolRole::SkillExecute,
					)),
					selector.clone(),
				],
				None,
			),
			DirectRouteKind::Tool { selector } => self.execute_tool_like_route(
				task_id,
				request,
				"direct-route",
				&format!("Use selected tool `{}` directly", selector.name()),
				vec![selector.clone()],
				None,
			),
			DirectRouteKind::Inventory => self.execute_tool_like_route(
				task_id,
				request,
				"direct-route",
				"Describe current runtime inventory directly",
				vec![ResourceSelector::tool(tool_name_for_role(
					&self.tool_config,
					crate::tool_config::BuiltinToolRole::Inventory,
				))],
				None,
			),
			DirectRouteKind::Conversation => self.execute_tool_like_route(
				task_id,
				request,
				"direct-route",
				"Answer directly without external resource planning",
				vec![ResourceSelector::tool(tool_name_for_role(
					&self.tool_config,
					crate::tool_config::BuiltinToolRole::General,
				))],
				None,
			),
		};
		self.remember_tool_result_summary(&request.session_id, &execution.message);
		execution
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
			EscalationAction::EnterLimitedPlanning => fallback_answer_message(
				&request.goal,
				result.decision.intent_family,
				&result.decision.reason,
			),
		};

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
			EscalationAction::EnterLimitedPlanning => {
				"Explain that the request needs a planning-heavy workflow and will be escalated."
					.to_string()
			}
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

	fn remember_route_decision(&self, session_id: &str, result: &RouteDecisionResult) {
		let decision = match result {
			RouteDecisionResult::Direct(plan) => &plan.decision,
			RouteDecisionResult::Escalate(plan) => &plan.decision,
		};
		let last_explicit_resource = match result {
			RouteDecisionResult::Direct(plan) => match &plan.kind {
				DirectRouteKind::SkillAdvisory { selector }
				| DirectRouteKind::SkillExecutable { selector }
				| DirectRouteKind::Tool { selector } => Some(selector.display_key()),
				DirectRouteKind::Inventory
				| DirectRouteKind::Conversation
				| DirectRouteKind::SkillInstall { .. } => None,
			},
			RouteDecisionResult::Escalate(_) => None,
		};
		if let Ok(mut scratchpads) = self.scratchpads.lock() {
			let pad = scratchpads.entry(session_id.to_string()).or_default();
			pad.last_decision = Some(decision.clone());
			if let Some(resource) = last_explicit_resource {
				pad.last_explicit_resource = Some(resource);
			}
			pad.task_completed = false;
		}
	}

	fn remember_tool_result_summary(&self, session_id: &str, summary: &str) {
		if let Ok(mut scratchpads) = self.scratchpads.lock() {
			let pad = scratchpads.entry(session_id.to_string()).or_default();
			pad.last_tool_result_summary = Some(summary.to_string());
			pad.task_completed = true;
		}
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

fn render_inventory_message(catalog: &ResourceCatalog, goal: &str) -> String {
	let skill_names = catalog
		.entries()
		.iter()
		.filter(|entry| entry.kind == roku_plugin_catalog::ResourceKind::Skill)
		.map(|entry| entry.name.clone())
		.collect::<Vec<_>>();
	let tool_names = catalog
		.entries()
		.iter()
		.filter(|entry| entry.kind == roku_plugin_catalog::ResourceKind::Tool && entry.discoverable)
		.map(|entry| entry.name.clone())
		.collect::<Vec<_>>();
	let capability_families = catalog
		.entries()
		.iter()
		.flat_map(|entry| entry.required_capabilities.iter())
		.filter_map(|capability| capability.split('.').next())
		.collect::<Vec<_>>();
	let capability_families = {
		let mut deduped = Vec::new();
		for family in capability_families {
			if !deduped.contains(&family) {
				deduped.push(family);
			}
		}
		deduped
	};
	if !goal.is_ascii() {
		format!(
			"当前可用的 skills: {}。可发现 tools: {}。能力类别: {}。",
			joined_or_none(&skill_names),
			joined_or_none(&tool_names),
			if capability_families.is_empty() {
				"(none)".to_string()
			} else {
				capability_families.join(", ")
			},
		)
	} else {
		format!(
			"Available skills: {}. Discoverable tools: {}. Capability families: {}.",
			joined_or_none(&skill_names),
			joined_or_none(&tool_names),
			if capability_families.is_empty() {
				"(none)".to_string()
			} else {
				capability_families.join(", ")
			},
		)
	}
}

fn deterministic_chat_message(goal: &str) -> String {
	if !goal.is_ascii() {
		"我是 Roku。当前我会优先走 direct route；复杂请求会升级到兼容的 legacy planning 路径。"
			.to_string()
	} else {
		"I'm Roku. I prefer direct routes for simple requests and escalate complex work into the compatibility planning path.".to_string()
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
			"这个请求目前被识别为 {:?}，但当前运行时还没有对应的 direct tool。{reason} 我还没有执行任何外部操作。",
			intent_family
		)
	} else {
		format!(
			"This request was classified as {:?}, but the current runtime does not expose a matching direct tool yet. {reason} No external action has been executed.",
			intent_family
		)
	}
}

fn joined_or_none(values: &[String]) -> String {
	if values.is_empty() {
		"(none)".to_string()
	} else {
		values.join(", ")
	}
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
		ResultStatus, TaskId, TaskNode, TaskNodeKind,
	};
	use roku_plugin_llm::{
		GenerationRequest, LlmProvider, LlmRouter, ModelProfile, ProviderCallError,
		ProviderResponse, RiskTier, RoutingPolicy,
	};
	use roku_plugin_skills::{
		DownloadedArchive, SkillArchiveFetcher, SkillRegistry, SkillRegistryError, SkillSource,
	};
	use std::io::{Cursor, Write};
	use std::sync::Arc;

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
		assert_eq!(payload["message"], "data pipeline step executed");
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
				output: "live answer from llm".to_string(),
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
				output: r#"First, the user's request is: "今天周几？"

From the trusted runtime context:
- local_weekday: Sunday

So, I'll output: "星期日""#
					.to_string(),
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
		assert!(message.starts_with("星期"));
		assert!(message.ends_with('。'));
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
