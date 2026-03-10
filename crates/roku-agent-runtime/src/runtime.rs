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

use std::sync::Arc;

use crate::result::policy_rejection_result;
use crate::tool_config::ToolCatalogConfig;
use crate::tools::{build_builtin_tool_runtime, build_llm_tool_runtime, build_resource_catalog};
use crate::workers::{
	data_worker_with_config, generic_worker_with_config, inventory_worker_with_config,
	research_worker_with_config, review_worker_with_config, skill_execute_worker_with_config,
	skill_worker_with_config,
};
use roku_common_types::{AgentInstanceSpec, ResultEnvelope, TaskNode};
use roku_llm_adapter::LlmRouter;
use roku_resource_catalog::ResourceCatalog;
use roku_skill_registry::SkillRegistry;
use roku_tool_runtime::ToolRuntime;

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
}

impl GenericAgentRuntime {
	pub fn with_tool_runtime(
		tool_runtime: ToolRuntime,
		resource_catalog: ResourceCatalog,
		tool_config: ToolCatalogConfig,
	) -> Self {
		let shared_tool_runtime = Arc::new(tool_runtime);
		let mut runtime = Self {
			workers: Vec::new(),
			tool_runtime: Arc::clone(&shared_tool_runtime),
			resource_catalog,
			tool_config: tool_config.clone(),
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
		let resource_catalog = build_resource_catalog(&skill_registry, &tool_config);
		Self::with_tool_runtime(
			build_builtin_tool_runtime(skill_registry, &tool_config),
			resource_catalog,
			tool_config,
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
		let resource_catalog = build_resource_catalog(&skill_registry, &tool_config);
		Self::with_tool_runtime(
			build_llm_tool_runtime(
				Arc::new(router),
				skill_registry,
				&tool_config,
				&resource_catalog,
			),
			resource_catalog,
			tool_config,
		)
	}

	pub fn resource_catalog(&self) -> &ResourceCatalog {
		&self.resource_catalog
	}

	pub fn tool_config(&self) -> &ToolCatalogConfig {
		&self.tool_config
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

#[cfg(test)]
mod tests {
	use roku_common_types::{
		AgentContext, AggregationMode, EvidenceItem, JoinPolicy, NodeId, PolicyBindings,
		ResultStatus, TaskId, TaskNode, TaskNodeKind,
	};
	use roku_llm_adapter::{
		GenerationRequest, LlmProvider, LlmRouter, ModelProfile, ProviderCallError,
		ProviderResponse, RiskTier, RoutingPolicy,
	};
	use roku_skill_registry::{
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
