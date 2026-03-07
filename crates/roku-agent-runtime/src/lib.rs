//! Dynamic agent runtime.

use roku_common_types::{AgentInstanceSpec, EvidenceItem, ResultEnvelope, ResultStatus, TaskNode};

pub trait AgentWorker {
	fn execute(&self, spec: &AgentInstanceSpec, node: &TaskNode) -> ResultEnvelope;
}

#[derive(Debug, Default)]
pub struct GenericAgentRuntime;

impl AgentWorker for GenericAgentRuntime {
	fn execute(&self, spec: &AgentInstanceSpec, node: &TaskNode) -> ResultEnvelope {
		ResultEnvelope {
			task_id: spec.context.task_id.clone(),
			node_id: node.node_id.clone(),
			producer: spec.instance_id.clone(),
			schema_version: "result.v1".to_string(),
			status: ResultStatus::Ok,
			payload: format!("executed: {}", node.description),
			evidence: vec![EvidenceItem {
				kind: "runtime".to_string(),
				value: "generic".to_string(),
			}],
			confidence: 0.9,
		}
	}
}
