//! Plan outline generation.

use roku_common_types::{PlanOutline, PlanStep, RequestEnvelope};
use roku_planning_engine::PlanningMode;

pub trait TaskPlanner {
	fn build_outline(&self, request: &RequestEnvelope, mode: PlanningMode) -> PlanOutline;
}

#[derive(Debug, Default)]
pub struct SimpleTaskPlanner;

impl TaskPlanner for SimpleTaskPlanner {
	fn build_outline(&self, request: &RequestEnvelope, mode: PlanningMode) -> PlanOutline {
		let mut steps = Vec::new();
		steps.push(PlanStep {
			step_id: "step-1".to_string(),
			summary: format!("Understand request using {:?}", mode),
			required_capabilities: vec!["information.read".to_string()],
			requires_approval: false,
		});
		steps.push(PlanStep {
			step_id: "step-2".to_string(),
			summary: "Execute primary action".to_string(),
			required_capabilities: vec!["tool.invoke".to_string()],
			requires_approval: false,
		});

		PlanOutline {
			goal: request.goal.clone(),
			steps,
		}
	}
}
