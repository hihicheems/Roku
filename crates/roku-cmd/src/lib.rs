//! Roku command runtime bootstrap.

use roku_agent_instance_factory::AgentInstanceFactory;
use roku_agent_runtime::{AgentWorker, GenericAgentRuntime};
use roku_api_gateway::{Gateway, RawRequest};
use roku_common_types::{ResponseEnvelope, ResponseStatus, RuntimeError, TaskState};
use roku_execution_graph_builder::{ExecutionGraphBuilder, GraphBuildConfig};
use roku_orchestrator::Orchestrator;
use roku_planning_engine::{DefaultPlanningEngine, PlanningInput, RiskLevel, StrategySelector};
use roku_state_store::{EventStore, InMemoryEventStore, InMemoryTaskStore, TaskStore};
use roku_task_planner::{SimpleTaskPlanner, TaskPlanner};
use roku_validation_plane::ValidationPipeline;

pub fn run_once(goal: &str) -> Result<ResponseEnvelope, RuntimeError> {
	let gateway = Gateway;
	let orchestrator = Orchestrator::default();
	let planning_engine = DefaultPlanningEngine;
	let planner = SimpleTaskPlanner;
	let builder = ExecutionGraphBuilder;
	let factory = AgentInstanceFactory;
	let runtime = GenericAgentRuntime;
	let validator = ValidationPipeline::default();

	let mut task_store = InMemoryTaskStore::default();
	let mut event_store = InMemoryEventStore::default();

	let request = gateway.normalize(
		RawRequest {
			session_id: "session-1".to_string(),
			goal: goal.to_string(),
		},
		1,
	);

	let mut task = orchestrator.create_task(&request);
	let e1 = orchestrator.transition(&mut task, TaskState::Planning, "start planning", None)?;
	event_store.append_event(e1);

	let decision = planning_engine.select(&PlanningInput {
		complexity_score: 5,
		uncertainty_score: 4,
		risk_level: RiskLevel::Low,
		budget_tokens: 10_000,
	});
	let outline = planner.build_outline(&request, decision.mode);

	let e2 = orchestrator.transition(&mut task, TaskState::GraphBuilding, "build graph", None)?;
	event_store.append_event(e2);

	let graph = builder.compile(task.task_id.clone(), &outline, &GraphBuildConfig::default());
	task.graph = Some(graph.clone());

	let e3 = orchestrator.transition(&mut task, TaskState::Delegating, "delegate", None)?;
	event_store.append_event(e3);

	let node = graph
		.nodes
		.first()
		.ok_or_else(|| RuntimeError::new("graph has no nodes"))?;
	let spec = factory.build_for_node(&task.task_id, node);

	let e4 = orchestrator.transition(&mut task, TaskState::Executing, "execute", None)?;
	event_store.append_event(e4);

	let result = runtime.execute(&spec, node);

	let e5 = orchestrator.transition(&mut task, TaskState::Validating, "validate", None)?;
	event_store.append_event(e5);

	let report = validator.validate(&result);
	let response = if report.accepted {
		let e6 = orchestrator.transition(&mut task, TaskState::Aggregating, "aggregate", None)?;
		event_store.append_event(e6);
		let e7 = orchestrator.transition(&mut task, TaskState::Succeeded, "done", None)?;
		event_store.append_event(e7);

		ResponseEnvelope {
			request_id: request.request_id,
			status: ResponseStatus::Succeeded,
			message: "task succeeded".to_string(),
			artifacts: vec!["artifact://result/1".to_string()],
		}
	} else {
		let _ = orchestrator.transition(&mut task, TaskState::Failed, "validation failed", None)?;
		ResponseEnvelope {
			request_id: request.request_id,
			status: ResponseStatus::Failed,
			message: format!("task failed: {}", report.failures.join(", ")),
			artifacts: Vec::new(),
		}
	};

	task_store.upsert_task(task);

	Ok(response)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn run_once_returns_success() {
		let response = run_once("analyze market").expect("pipeline should succeed");
		assert!(matches!(response.status, ResponseStatus::Succeeded));
	}
}
