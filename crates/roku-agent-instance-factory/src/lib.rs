//! Build dynamic agent instances from node requirements.

use roku_common_types::{
	AgentContext, AgentInstanceSpec, NodeId, PolicyBindings, TaskId, TaskNode,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefaultProfile {
	Research,
	Data,
	Review,
}

#[derive(Debug, Default)]
pub struct AgentInstanceFactory;

impl AgentInstanceFactory {
	pub fn build_for_node(&self, task_id: &TaskId, node: &TaskNode) -> AgentInstanceSpec {
		AgentInstanceSpec {
			instance_id: format!("agent-{}", node.node_id.0),
			context: AgentContext {
				task_id: task_id.clone(),
				node_id: NodeId(node.node_id.0.clone()),
				summary: node.description.clone(),
			},
			capabilities: node.capabilities.clone(),
			policy_bindings: PolicyBindings {
				budget_tokens: 10_000,
				time_budget_ms: 30_000,
			},
		}
	}

	pub fn build_from_profile(
		&self,
		task_id: &TaskId,
		profile: DefaultProfile,
	) -> AgentInstanceSpec {
		let capabilities = match profile {
			DefaultProfile::Research => vec!["information.read".to_string()],
			DefaultProfile::Data => vec!["data.read".to_string(), "data.write".to_string()],
			DefaultProfile::Review => vec!["review.check".to_string()],
		};

		AgentInstanceSpec {
			instance_id: format!("agent-profile-{profile:?}"),
			context: AgentContext {
				task_id: task_id.clone(),
				node_id: NodeId("profile-node".to_string()),
				summary: "profile-based instance".to_string(),
			},
			capabilities,
			policy_bindings: PolicyBindings {
				budget_tokens: 8_000,
				time_budget_ms: 20_000,
			},
		}
	}
}
