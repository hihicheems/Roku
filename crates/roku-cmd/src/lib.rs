//! Roku command runtime bootstrap.

use roku_agent_instance_factory::AgentInstanceFactory;
use roku_agent_runtime::{AgentWorker, GenericAgentRuntime};
use roku_api_gateway::{Gateway, RawRequest};
use roku_capability_auth::{CapabilityAuthority, CapabilityRequest};
use roku_common_types::{ResponseEnvelope, ResponseStatus, RuntimeError, TaskState};
use roku_execution_graph_builder::{ExecutionGraphBuilder, GraphBuildConfig};
use roku_observability::{AuditRecord, AuditSink, InMemoryAuditSink, Metrics};
use roku_orchestrator::Orchestrator;
use roku_planning_engine::{DefaultPlanningEngine, PlanningInput, RiskLevel, StrategySelector};
use roku_state_store::{
	EventRepository, InMemoryEventRepository, InMemoryTaskRepository, TaskRepository,
};
use roku_task_planner::{SimpleTaskPlanner, TaskPlanner};
use roku_validation_plane::ValidationPipeline;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RunMode {
	#[default]
	Normal,
	MissingEvidence,
	CapabilityDenied,
}

pub fn run_once(goal: &str) -> Result<ResponseEnvelope, RuntimeError> {
	run_with_mode(goal, RunMode::Normal)
}

pub fn run_with_mode(goal: &str, mode: RunMode) -> Result<ResponseEnvelope, RuntimeError> {
	let gateway = Gateway;
	let orchestrator = Orchestrator::default();
	let planning_engine = DefaultPlanningEngine;
	let planner = SimpleTaskPlanner;
	let builder = ExecutionGraphBuilder;
	let factory = AgentInstanceFactory;
	let runtime = GenericAgentRuntime;
	let validator = ValidationPipeline::default();
	let mut capability_auth = CapabilityAuthority::default();
	let metrics = Metrics::default();
	let audit_sink = InMemoryAuditSink::default();

	let mut task_store = InMemoryTaskRepository::default();
	let mut event_store = InMemoryEventRepository::default();

	metrics.inc_requests();
	let request = gateway.normalize(
		RawRequest {
			session_id: "session-1".to_string(),
			goal: goal.to_string(),
		},
		1,
	);

	let mut task = orchestrator.create_task(&request);
	let e1 = orchestrator.transition(&mut task, TaskState::Planning, "start planning", None)?;
	event_store
		.append_event(e1)
		.map_err(|error| RuntimeError::new(error.to_string()))?;

	let decision = planning_engine.select(&PlanningInput {
		complexity_score: 5,
		uncertainty_score: 4,
		risk_level: RiskLevel::Low,
		budget_tokens: 10_000,
	});
	let outline = planner.build_outline(&request, decision.mode);

	let e2 = orchestrator.transition(&mut task, TaskState::GraphBuilding, "build graph", None)?;
	event_store
		.append_event(e2)
		.map_err(|error| RuntimeError::new(error.to_string()))?;

	let graph = builder.compile(task.task_id.clone(), &outline, &GraphBuildConfig::default());
	task.graph = Some(graph.clone());

	let e3 = orchestrator.transition(&mut task, TaskState::Delegating, "delegate", None)?;
	event_store
		.append_event(e3)
		.map_err(|error| RuntimeError::new(error.to_string()))?;

	let node = graph
		.nodes
		.first()
		.ok_or_else(|| RuntimeError::new("graph has no nodes"))?;
	let spec = factory.build_for_node(&task.task_id, node);

	let token = capability_auth.issue(CapabilityRequest {
		subject: spec.instance_id.clone(),
		resource: "tool.runtime".to_string(),
		actions: if matches!(mode, RunMode::CapabilityDenied) {
			vec!["read".to_string()]
		} else {
			vec!["invoke".to_string()]
		},
		expires_at_unix: 999_999,
	});
	let capability_allowed = capability_auth.verify(&token, "invoke", 100);
	if !capability_allowed {
		metrics.inc_failures();
		audit_sink
			.record(AuditRecord {
				actor: spec.instance_id,
				action: "invoke".to_string(),
				resource: token.resource,
				outcome: "denied".to_string(),
			})
			.map_err(|error| RuntimeError::new(error.to_string()))?;
		orchestrator
			.transition(&mut task, TaskState::Failed, "capability denied", None)
			.map(|_| ())?;
		task_store
			.save_task(task)
			.map_err(|error| RuntimeError::new(error.to_string()))?;
		return Ok(ResponseEnvelope {
			request_id: request.request_id,
			status: ResponseStatus::Failed,
			message: "task failed: capability denied".to_string(),
			artifacts: Vec::new(),
		});
	}

	let e4 = orchestrator.transition(&mut task, TaskState::Executing, "execute", None)?;
	event_store
		.append_event(e4)
		.map_err(|error| RuntimeError::new(error.to_string()))?;

	let mut result = runtime.execute(&spec, node);
	if matches!(mode, RunMode::MissingEvidence) {
		result.evidence.clear();
	}

	let e5 = orchestrator.transition(&mut task, TaskState::Validating, "validate", None)?;
	event_store
		.append_event(e5)
		.map_err(|error| RuntimeError::new(error.to_string()))?;

	let report = validator.validate(&result);
	let response = if report.accepted {
		let e6 = orchestrator.transition(&mut task, TaskState::Aggregating, "aggregate", None)?;
		event_store
			.append_event(e6)
			.map_err(|error| RuntimeError::new(error.to_string()))?;
		let e7 = orchestrator.transition(&mut task, TaskState::Succeeded, "done", None)?;
		event_store
			.append_event(e7)
			.map_err(|error| RuntimeError::new(error.to_string()))?;

		audit_sink
			.record(AuditRecord {
				actor: result.producer,
				action: "validate".to_string(),
				resource: result.schema_version,
				outcome: "accepted".to_string(),
			})
			.map_err(|error| RuntimeError::new(error.to_string()))?;

		ResponseEnvelope {
			request_id: request.request_id,
			status: ResponseStatus::Succeeded,
			message: "task succeeded".to_string(),
			artifacts: vec!["artifact://result/1".to_string()],
		}
	} else {
		metrics.inc_failures();
		metrics.inc_validation_failures();
		orchestrator
			.transition(&mut task, TaskState::Failed, "validation failed", None)
			.map(|_| ())?;
		ResponseEnvelope {
			request_id: request.request_id,
			status: ResponseStatus::Failed,
			message: format!("task failed: {}", report.failures.join(", ")),
			artifacts: Vec::new(),
		}
	};

	let _ = metrics.snapshot();
	task_store
		.save_task(task)
		.map_err(|error| RuntimeError::new(error.to_string()))?;

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

	#[test]
	fn run_with_missing_evidence_fails_validation() {
		let response = run_with_mode("analyze market", RunMode::MissingEvidence)
			.expect("pipeline should execute and fail validation");
		assert!(matches!(response.status, ResponseStatus::Failed));
		assert!(response.message.contains("evidence is required"));
	}

	#[test]
	fn run_with_capability_denied_fails() {
		let response = run_with_mode("analyze market", RunMode::CapabilityDenied)
			.expect("pipeline should execute and fail with capability denial");
		assert!(matches!(response.status, ResponseStatus::Failed));
		assert!(response.message.contains("capability denied"));
	}
}
