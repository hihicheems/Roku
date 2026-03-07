use roku_common_types::{
	AggregationMode, ApprovalDecision, ApprovalId, EvidenceItem, JoinPolicy, NodeId,
	RequestEnvelope, RequestId, ResponseStatus, ResultEnvelope, ResultStatus, Task, TaskEdge,
	TaskGraph, TaskId, TaskNode, TaskNodeKind, TaskState,
};

use crate::{RunMode, RuntimeService};

fn sample_request() -> RequestEnvelope {
	RequestEnvelope {
		request_id: RequestId("req-1".to_string()),
		session_id: "session-1".to_string(),
		goal: "analyze market".to_string(),
	}
}

#[test]
fn service_succeeds_for_happy_path() {
	let service = RuntimeService::default();
	let response = service
		.execute(sample_request())
		.expect("runtime service should succeed");

	assert_eq!(response.status, ResponseStatus::Succeeded);
	assert_eq!(response.artifacts.len(), 2);
	let artifacts = service
		.list_artifacts(&TaskId("task-req-1".to_string()))
		.expect("artifacts should load");
	assert_eq!(artifacts.len(), 2);
	let experiment = service
		.get_experiment_run(&TaskId("task-req-1".to_string()))
		.expect("experiment load should succeed")
		.expect("experiment should exist");
	assert_eq!(experiment.summary.as_deref(), Some("task succeeded"));
	assert_eq!(experiment.artifact_ids.len(), 2);
}

#[test]
fn service_persists_tool_runtime_evidence_for_execution_nodes() {
	let service = RuntimeService::default();
	service
		.execute(sample_request())
		.expect("runtime service should succeed");

	let results = service
		.list_results(&TaskId("task-req-1".to_string()))
		.expect("results should load");
	assert!(!results.is_empty());
	assert!(results.iter().all(|result| {
		result
			.evidence
			.iter()
			.any(|item| item.kind == "tool" && !item.value.is_empty())
	}));
	assert!(results.iter().all(|result| {
		result
			.evidence
			.iter()
			.any(|item| item.kind == "output_fingerprint" && !item.value.is_empty())
	}));
	assert!(
		results
			.iter()
			.all(|result| { serde_json::from_str::<serde_json::Value>(&result.payload).is_ok() })
	);
}

#[test]
fn service_exposes_artifact_content_by_task_and_artifact() {
	let service = RuntimeService::default();
	let response = service
		.execute(sample_request())
		.expect("runtime service should succeed");
	assert_eq!(response.status, ResponseStatus::Succeeded);
	let task_id = TaskId("task-req-1".to_string());
	let artifacts = service
		.list_artifacts(&task_id)
		.expect("artifacts should load");
	let artifact = artifacts.first().expect("artifact should exist");
	let content = service
		.get_artifact_content(&task_id, &artifact.artifact_id)
		.expect("artifact content lookup should succeed")
		.expect("artifact content should exist");
	assert!(!content.is_empty());
}

#[test]
fn service_rejects_cross_task_artifact_content_access() {
	let service = RuntimeService::default();
	service
		.execute(sample_request())
		.expect("runtime service should succeed");
	let task_id = TaskId("task-req-1".to_string());
	let artifacts = service
		.list_artifacts(&task_id)
		.expect("artifacts should load");
	let artifact = artifacts.first().expect("artifact should exist");

	let error = service
		.get_artifact_content(&TaskId("task-other".to_string()), &artifact.artifact_id)
		.expect_err("cross-task artifact content should be rejected");
	assert!(error.message.contains("does not belong to task"));
}

#[test]
fn service_reports_validation_failure() {
	let service = RuntimeService::default();
	let response = service
		.execute_with_mode(sample_request(), RunMode::MissingEvidence)
		.expect("runtime service should return failed response");

	assert_eq!(response.status, ResponseStatus::Failed);
	assert!(response.message.contains("evidence is required"));
	let experiment = service
		.get_experiment_run(&TaskId("task-req-1".to_string()))
		.expect("experiment load should succeed")
		.expect("experiment should exist");
	assert!(
		experiment
			.failure_reason
			.as_deref()
			.is_some_and(|reason| reason.contains("evidence is required"))
	);
}

#[test]
fn service_reports_capability_denied() {
	let service = RuntimeService::default();
	let response = service
		.execute_with_mode(sample_request(), RunMode::CapabilityDenied)
		.expect("runtime service should return failed response");

	assert_eq!(response.status, ResponseStatus::Failed);
	assert!(response.message.contains("capability denied"));
	let metrics = service.metrics_snapshot();
	assert_eq!(metrics.experiments_failed_total, 1);
}

#[test]
fn service_tracks_planning_metrics() {
	let service = RuntimeService::default();
	service
		.execute(sample_request())
		.expect("runtime service should succeed");

	let metrics = service.metrics_snapshot();
	assert_eq!(metrics.planning_runs_total, 1);
	assert_eq!(metrics.planning_react_total, 1);
}

#[test]
fn service_returns_pending_approval_when_graph_contains_approval_gate() {
	let service = RuntimeService::default();
	let response = service
		.execute_with_mode(sample_request(), RunMode::ApprovalRequired)
		.expect("runtime service should stop at approval gate");

	assert_eq!(response.status, ResponseStatus::PendingApproval);
	assert!(response.message.contains("approval required"));
	assert_eq!(response.artifacts.len(), 1);
}

#[test]
fn service_dead_letters_when_retry_budget_is_exhausted() {
	let service = RuntimeService::default();
	let response = service
		.execute_with_mode(sample_request(), RunMode::RetryExhausted)
		.expect("runtime service should report dead-letter");

	assert_eq!(response.status, ResponseStatus::Failed);
	assert!(response.message.contains("dead-lettered"));
}

#[test]
fn service_resumes_after_approval_is_granted() {
	let service = RuntimeService::default();
	let response = service
		.execute_with_mode(sample_request(), RunMode::ApprovalRequired)
		.expect("runtime service should create approval ticket");
	let approval_id = ApprovalId(
		response.artifacts[0]
			.trim_start_matches("approval://")
			.to_string(),
	);

	let resumed = service
		.decide_approval(
			&approval_id,
			ApprovalDecision {
				actor: "reviewer".to_string(),
				approved: true,
				comment: Some("approved".to_string()),
			},
		)
		.expect("approval grant should resume task");

	assert_eq!(resumed.status, ResponseStatus::Succeeded);
}

#[test]
fn service_fails_when_approval_is_rejected() {
	let service = RuntimeService::default();
	let response = service
		.execute_with_mode(sample_request(), RunMode::ApprovalRequired)
		.expect("runtime service should create approval ticket");
	let approval_id = ApprovalId(
		response.artifacts[0]
			.trim_start_matches("approval://")
			.to_string(),
	);

	let resumed = service
		.decide_approval(
			&approval_id,
			ApprovalDecision {
				actor: "reviewer".to_string(),
				approved: false,
				comment: Some("rejected".to_string()),
			},
		)
		.expect("approval rejection should finish task");

	assert_eq!(resumed.status, ResponseStatus::Failed);
	assert!(resumed.message.contains("approval rejected"));
}

#[test]
fn approval_ticket_cannot_be_decided_twice() {
	let service = RuntimeService::default();
	let response = service
		.execute_with_mode(sample_request(), RunMode::ApprovalRequired)
		.expect("runtime service should create approval ticket");
	let approval_id = ApprovalId(
		response.artifacts[0]
			.trim_start_matches("approval://")
			.to_string(),
	);

	service
		.decide_approval(
			&approval_id,
			ApprovalDecision {
				actor: "reviewer".to_string(),
				approved: true,
				comment: None,
			},
		)
		.expect("first decision should succeed");

	let duplicate = service.decide_approval(
		&approval_id,
		ApprovalDecision {
			actor: "reviewer".to_string(),
			approved: true,
			comment: None,
		},
	);
	assert!(duplicate.is_err());
}

#[test]
fn validation_collects_results_through_approval_nodes() {
	let service = RuntimeService::default();
	let task = Task {
		task_id: TaskId("task-1".to_string()),
		request_id: RequestId("req-1".to_string()),
		state: TaskState::Executing,
		attempts: 0,
		completed_nodes: vec![NodeId("extract".to_string())],
		next_node_index: 1,
		pending_approval_id: None,
		last_result: None,
		graph: Some(TaskGraph {
			task_id: TaskId("task-1".to_string()),
			nodes: vec![
				TaskNode {
					node_id: NodeId("extract".to_string()),
					kind: TaskNodeKind::Execution,
					description: "extract".to_string(),
					capabilities: Vec::new(),
					join_policy: JoinPolicy::AllParents,
					aggregation_mode: AggregationMode::CollectAll,
				},
				TaskNode {
					node_id: NodeId("extract-approval".to_string()),
					kind: TaskNodeKind::Approval,
					description: "approval".to_string(),
					capabilities: Vec::new(),
					join_policy: JoinPolicy::AllParents,
					aggregation_mode: AggregationMode::CollectAll,
				},
				TaskNode {
					node_id: NodeId("validate".to_string()),
					kind: TaskNodeKind::Validation,
					description: "validate".to_string(),
					capabilities: Vec::new(),
					join_policy: JoinPolicy::AllParents,
					aggregation_mode: AggregationMode::CollectAll,
				},
			],
			edges: vec![
				TaskEdge {
					from: NodeId("extract".to_string()),
					to: NodeId("extract-approval".to_string()),
				},
				TaskEdge {
					from: NodeId("extract-approval".to_string()),
					to: NodeId("validate".to_string()),
				},
			],
		}),
	};
	service
		.save_result(ResultEnvelope {
			task_id: TaskId("task-1".to_string()),
			node_id: NodeId("extract".to_string()),
			producer: "agent".to_string(),
			schema_version: "result.v1".to_string(),
			status: ResultStatus::Ok,
			payload: "payload".to_string(),
			evidence: vec![EvidenceItem {
				kind: "artifact_ref".to_string(),
				value: "artifact://1".to_string(),
			}],
			confidence: 0.9,
		})
		.expect("result should be persisted");

	let results = service
		.collect_upstream_results(&task, &NodeId("validate".to_string()))
		.expect("upstream results should resolve through approval nodes");

	assert_eq!(results.len(), 1);
	assert_eq!(results[0].node_id.0, "extract");
}

#[test]
fn validation_evidence_resolves_persisted_artifacts() {
	let service = RuntimeService::default();
	service
		.execute(sample_request())
		.expect("runtime service should succeed");

	let task_id = TaskId("task-req-1".to_string());
	let task = {
		let state = service.lock_state().expect("state lock should succeed");
		state
			.task_repo
			.load_task(&task_id)
			.expect("task load should succeed")
			.expect("task should exist")
	};
	let evidence_sets = service
		.collect_validation_evidence(
			&task,
			&task.graph.as_ref().expect("graph should exist").nodes[2],
		)
		.expect("validation evidence should load");

	assert_eq!(evidence_sets.len(), 1);
	assert!(
		evidence_sets
			.iter()
			.all(|evidence_set| !evidence_set.artifacts.is_empty())
	);
}

#[test]
fn node_result_set_applies_highest_confidence_aggregation() {
	let service = RuntimeService::default();
	let task = Task {
		task_id: TaskId("task-aggregation".to_string()),
		request_id: RequestId("req-aggregation".to_string()),
		state: TaskState::Executing,
		attempts: 0,
		completed_nodes: vec![
			NodeId("branch-a".to_string()),
			NodeId("branch-b".to_string()),
		],
		next_node_index: 2,
		pending_approval_id: None,
		last_result: None,
		graph: Some(TaskGraph {
			task_id: TaskId("task-aggregation".to_string()),
			nodes: vec![
				TaskNode {
					node_id: NodeId("branch-a".to_string()),
					kind: TaskNodeKind::Execution,
					description: "branch a".to_string(),
					capabilities: Vec::new(),
					join_policy: JoinPolicy::AllParents,
					aggregation_mode: AggregationMode::CollectAll,
				},
				TaskNode {
					node_id: NodeId("branch-b".to_string()),
					kind: TaskNodeKind::Execution,
					description: "branch b".to_string(),
					capabilities: Vec::new(),
					join_policy: JoinPolicy::AllParents,
					aggregation_mode: AggregationMode::CollectAll,
				},
				TaskNode {
					node_id: NodeId("aggregate".to_string()),
					kind: TaskNodeKind::Aggregation,
					description: "aggregate".to_string(),
					capabilities: Vec::new(),
					join_policy: JoinPolicy::AllParents,
					aggregation_mode: AggregationMode::HighestConfidence,
				},
			],
			edges: vec![
				TaskEdge {
					from: NodeId("branch-a".to_string()),
					to: NodeId("aggregate".to_string()),
				},
				TaskEdge {
					from: NodeId("branch-b".to_string()),
					to: NodeId("aggregate".to_string()),
				},
			],
		}),
	};
	service
		.save_result(ResultEnvelope {
			task_id: TaskId("task-aggregation".to_string()),
			node_id: NodeId("branch-a".to_string()),
			producer: "agent-a".to_string(),
			schema_version: "result.v1".to_string(),
			status: ResultStatus::Ok,
			payload: "low".to_string(),
			evidence: Vec::new(),
			confidence: 0.4,
		})
		.expect("branch a result should persist");
	service
		.save_result(ResultEnvelope {
			task_id: TaskId("task-aggregation".to_string()),
			node_id: NodeId("branch-b".to_string()),
			producer: "agent-b".to_string(),
			schema_version: "result.v1".to_string(),
			status: ResultStatus::Ok,
			payload: "high".to_string(),
			evidence: Vec::new(),
			confidence: 0.9,
		})
		.expect("branch b result should persist");

	let result_set = service
		.collect_node_result_set(
			&task,
			&task.graph.as_ref().expect("graph should exist").nodes[2],
		)
		.expect("result set should collect");

	assert_eq!(result_set.source_node_ids.len(), 2);
	assert_eq!(result_set.results.len(), 1);
	assert_eq!(result_set.results[0].node_id.0, "branch-b");
}

#[test]
fn node_result_set_enforces_quorum_policy() {
	let service = RuntimeService::default();
	let task = Task {
		task_id: TaskId("task-quorum".to_string()),
		request_id: RequestId("req-quorum".to_string()),
		state: TaskState::Executing,
		attempts: 0,
		completed_nodes: vec![NodeId("branch-a".to_string())],
		next_node_index: 1,
		pending_approval_id: None,
		last_result: None,
		graph: Some(TaskGraph {
			task_id: TaskId("task-quorum".to_string()),
			nodes: vec![
				TaskNode {
					node_id: NodeId("branch-a".to_string()),
					kind: TaskNodeKind::Execution,
					description: "branch a".to_string(),
					capabilities: Vec::new(),
					join_policy: JoinPolicy::AllParents,
					aggregation_mode: AggregationMode::CollectAll,
				},
				TaskNode {
					node_id: NodeId("branch-b".to_string()),
					kind: TaskNodeKind::Execution,
					description: "branch b".to_string(),
					capabilities: Vec::new(),
					join_policy: JoinPolicy::AllParents,
					aggregation_mode: AggregationMode::CollectAll,
				},
				TaskNode {
					node_id: NodeId("aggregate".to_string()),
					kind: TaskNodeKind::Aggregation,
					description: "aggregate".to_string(),
					capabilities: Vec::new(),
					join_policy: JoinPolicy::Quorum(2),
					aggregation_mode: AggregationMode::CollectAll,
				},
			],
			edges: vec![
				TaskEdge {
					from: NodeId("branch-a".to_string()),
					to: NodeId("aggregate".to_string()),
				},
				TaskEdge {
					from: NodeId("branch-b".to_string()),
					to: NodeId("aggregate".to_string()),
				},
			],
		}),
	};
	service
		.save_result(ResultEnvelope {
			task_id: TaskId("task-quorum".to_string()),
			node_id: NodeId("branch-a".to_string()),
			producer: "agent-a".to_string(),
			schema_version: "result.v1".to_string(),
			status: ResultStatus::Ok,
			payload: "only branch".to_string(),
			evidence: Vec::new(),
			confidence: 0.8,
		})
		.expect("branch a result should persist");

	let error = service
		.collect_node_result_set(
			&task,
			&task.graph.as_ref().expect("graph should exist").nodes[2],
		)
		.expect_err("quorum should reject incomplete branches");
	assert!(error.message.contains("join policy"));
}
