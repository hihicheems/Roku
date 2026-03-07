//! Capability-aware dynamic agent runtime.

use std::sync::Arc;

use roku_common_types::{AgentInstanceSpec, EvidenceItem, ResultEnvelope, ResultStatus, TaskNode};

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
}

impl GenericAgentRuntime {
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
			return ResultEnvelope {
				task_id: spec.context.task_id.clone(),
				node_id: node.node_id.clone(),
				producer: spec.instance_id.clone(),
				schema_version: "result.v1".to_string(),
				status: ResultStatus::Error,
				payload: "policy bindings rejected execution".to_string(),
				evidence: vec![EvidenceItem {
					kind: "policy".to_string(),
					value: "budget-exhausted".to_string(),
				}],
				confidence: 0.0,
			};
		}

		if let Some(result) = self.execute_with_worker(spec, node) {
			return result;
		}

		GenericFallbackWorker.execute(spec, node)
	}
}

impl Default for GenericAgentRuntime {
	fn default() -> Self {
		let mut runtime = Self {
			workers: Vec::new(),
		};
		runtime.register_worker(90, ResearchWorker);
		runtime.register_worker(80, DataWorker);
		runtime.register_worker(70, ReviewWorker);
		runtime.register_worker(10, GenericFallbackWorker);
		runtime
	}
}

struct ResearchWorker;
struct DataWorker;
struct ReviewWorker;
struct GenericFallbackWorker;

impl RuntimeWorker for ResearchWorker {
	fn worker_id(&self) -> &'static str {
		"research-worker"
	}

	fn supports(&self, capabilities: &[String]) -> bool {
		capabilities.iter().any(|capability| {
			capability.starts_with("information.") || capability.starts_with("research.")
		})
	}

	fn execute(&self, spec: &AgentInstanceSpec, node: &TaskNode) -> ResultEnvelope {
		build_result(
			spec,
			node,
			self.worker_id(),
			"research synthesis generated",
			0.86,
		)
	}
}

impl RuntimeWorker for DataWorker {
	fn worker_id(&self) -> &'static str {
		"data-worker"
	}

	fn supports(&self, capabilities: &[String]) -> bool {
		capabilities
			.iter()
			.any(|capability| capability.starts_with("data."))
	}

	fn execute(&self, spec: &AgentInstanceSpec, node: &TaskNode) -> ResultEnvelope {
		build_result(
			spec,
			node,
			self.worker_id(),
			"data pipeline step executed",
			0.88,
		)
	}
}

impl RuntimeWorker for ReviewWorker {
	fn worker_id(&self) -> &'static str {
		"review-worker"
	}

	fn supports(&self, capabilities: &[String]) -> bool {
		capabilities.iter().any(|capability| {
			capability.starts_with("review.") || capability.starts_with("validation.")
		})
	}

	fn execute(&self, spec: &AgentInstanceSpec, node: &TaskNode) -> ResultEnvelope {
		build_result(
			spec,
			node,
			self.worker_id(),
			"review checks completed",
			0.92,
		)
	}
}

impl RuntimeWorker for GenericFallbackWorker {
	fn worker_id(&self) -> &'static str {
		"generic-worker"
	}

	fn supports(&self, _capabilities: &[String]) -> bool {
		true
	}

	fn execute(&self, spec: &AgentInstanceSpec, node: &TaskNode) -> ResultEnvelope {
		build_result(
			spec,
			node,
			self.worker_id(),
			"generic execution completed",
			0.75,
		)
	}
}

fn build_result(
	spec: &AgentInstanceSpec,
	node: &TaskNode,
	worker_id: &str,
	message: &str,
	confidence: f32,
) -> ResultEnvelope {
	ResultEnvelope {
		task_id: spec.context.task_id.clone(),
		node_id: node.node_id.clone(),
		producer: spec.instance_id.clone(),
		schema_version: "result.v1".to_string(),
		status: ResultStatus::Ok,
		payload: format!("{message}: {}", node.description),
		evidence: vec![
			EvidenceItem {
				kind: "runtime".to_string(),
				value: worker_id.to_string(),
			},
			EvidenceItem {
				kind: "policy".to_string(),
				value: format!(
					"budget_tokens={},time_budget_ms={}",
					spec.policy_bindings.budget_tokens, spec.policy_bindings.time_budget_ms
				),
			},
		],
		confidence,
	}
}

#[cfg(test)]
mod tests {
	use roku_common_types::{
		AgentContext, AggregationMode, JoinPolicy, NodeId, PolicyBindings, TaskId, TaskNode,
		TaskNodeKind,
	};

	use super::*;

	fn node_with_capability(capability: &str) -> TaskNode {
		TaskNode {
			node_id: NodeId("node-1".to_string()),
			kind: TaskNodeKind::Execution,
			description: "runtime test node".to_string(),
			capabilities: vec![capability.to_string()],
			join_policy: JoinPolicy::default(),
			aggregation_mode: AggregationMode::default(),
		}
	}

	fn spec_with_capability(capability: &str) -> AgentInstanceSpec {
		AgentInstanceSpec {
			instance_id: "agent-1".to_string(),
			context: AgentContext {
				task_id: TaskId("task-1".to_string()),
				node_id: NodeId("node-1".to_string()),
				summary: "summary".to_string(),
			},
			capabilities: vec![capability.to_string()],
			policy_bindings: PolicyBindings {
				budget_tokens: 10_000,
				time_budget_ms: 30_000,
			},
		}
	}

	#[test]
	fn dispatches_to_data_worker_for_data_capability() {
		let runtime = GenericAgentRuntime::default();
		let node = node_with_capability("data.read");
		let spec = spec_with_capability("data.read");

		let result = runtime.execute(&spec, &node);
		assert_eq!(result.status, ResultStatus::Ok);
		assert_eq!(result.evidence[0].value, "data-worker");
	}

	#[test]
	fn dispatches_to_review_worker_for_review_capability() {
		let runtime = GenericAgentRuntime::default();
		let node = node_with_capability("review.check");
		let spec = spec_with_capability("review.check");

		let result = runtime.execute(&spec, &node);
		assert_eq!(result.status, ResultStatus::Ok);
		assert_eq!(result.evidence[0].value, "review-worker");
	}

	#[test]
	fn rejects_execution_when_policy_budget_is_zero() {
		let runtime = GenericAgentRuntime::default();
		let node = node_with_capability("data.read");
		let mut spec = spec_with_capability("data.read");
		spec.policy_bindings.budget_tokens = 0;

		let result = runtime.execute(&spec, &node);
		assert_eq!(result.status, ResultStatus::Error);
		assert_eq!(result.evidence[0].value, "budget-exhausted");
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
			build_result(
				spec,
				node,
				self.worker_id(),
				"custom quant worker executed",
				0.95,
			)
		}
	}

	#[test]
	fn allows_runtime_worker_extension() {
		let mut runtime = GenericAgentRuntime::default();
		runtime.register_worker(100, CustomWorker);
		let node = node_with_capability("quant.backtest");
		let spec = spec_with_capability("quant.backtest");

		let result = runtime.execute(&spec, &node);
		assert_eq!(result.status, ResultStatus::Ok);
		assert_eq!(result.evidence[0].value, "custom-worker");
	}
}
