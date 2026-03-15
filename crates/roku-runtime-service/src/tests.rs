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

use std::env;

use roku_agent_runtime::{
	AskUserPayload, AskUserResumeContract, AskUserResumeDirective, IntentFamily, LoopContext,
	LoopState, RouteDecision, RouteRisk, StepObservation, StepRecord, ToolObservation,
};
use roku_common_types::ResourceSelector;
use roku_common_types::{
	AggregationMode, ApprovalDecision, ApprovalId, ApprovalStatus, ApprovalTicket, JoinPolicy,
	NodeId, PlanningModeHint, RecoveryEligibility, RequestEnvelope, RequestId, ResponseStatus,
	ResultEnvelope, ResultStatus, Task, TaskEdge, TaskGraph, TaskId, TaskNode, TaskNodeKind,
	TaskState,
};

use crate::{RuntimeExecutionMode, RuntimeModeReport, RuntimeService, compact_approval_id};

fn request(goal: &str) -> RequestEnvelope {
	RequestEnvelope {
		request_id: RequestId("req-1".to_string()),
		session_id: "session-1".to_string(),
		goal: goal.to_string(),
		planning_mode_hint: None,
		conversation_history: Vec::new(),
	}
}

fn node(node_id: &str, kind: TaskNodeKind) -> TaskNode {
	TaskNode {
		node_id: NodeId(node_id.to_string()),
		kind,
		description: node_id.to_string(),
		capabilities: Vec::new(),
		join_policy: JoinPolicy::AllParents,
		aggregation_mode: AggregationMode::CollectAll,
		..TaskNode::default()
	}
}

fn execution_result(task_id: &TaskId, node_id: &str, message: &str) -> ResultEnvelope {
	ResultEnvelope {
		task_id: task_id.clone(),
		node_id: NodeId(node_id.to_string()),
		producer: format!("legacy:{node_id}"),
		schema_version: "result.v1".to_string(),
		status: ResultStatus::Ok,
		payload: serde_json::json!({
			"message": message,
		})
		.to_string(),
		evidence: Vec::new(),
		confidence: 0.9,
	}
}

fn graph_task(
	task_id: &str,
	request_id: &str,
	goal: &str,
	state: TaskState,
	graph: TaskGraph,
) -> Task {
	Task {
		task_id: TaskId(task_id.to_string()),
		request_id: RequestId(request_id.to_string()),
		session_id: "session-1".to_string(),
		goal: goal.to_string(),
		state,
		attempts: 0,
		planning_mode_hint: None,
		conversation_history: Vec::new(),
		completed_nodes: Vec::new(),
		next_node_index: 0,
		pending_approval_id: None,
		last_result: None,
		compensation_records: Vec::new(),
		graph: Some(graph),
	}
}

#[test]
fn compact_approval_id_stays_short_for_telegram_callbacks() {
	let approval_id = compact_approval_id("task-tg-919471825", "request-approval");

	assert!(approval_id.len() <= 19);
	assert!(format!("ap:a:{approval_id}").len() <= 64);
}

#[test]
fn new_requests_execute_without_graph_compilation() {
	let service = RuntimeService::default();
	let response = service
		.execute(request("What skills and tools do you have right now?"))
		.expect("direct request should succeed");

	assert_eq!(
		response.status,
		ResponseStatus::Succeeded,
		"unexpected resume response: {response:?}"
	);

	let task = service
		.get_task(&TaskId("task-req-1".to_string()))
		.expect("task lookup should succeed")
		.expect("task should be persisted");
	assert!(task.graph.is_none());
	assert_eq!(task.state, TaskState::Succeeded);
}

#[test]
fn runtime_service_defaults_to_deterministic_runtime_mode() {
	let service = RuntimeService::default();
	let report = service.runtime_mode_report();

	assert_eq!(report.requested, RuntimeExecutionMode::Deterministic);
	assert_eq!(report.effective, RuntimeExecutionMode::Deterministic);
	assert_eq!(report.fallback_reason, None);
}

#[test]
fn runtime_service_can_expose_live_fallback_mode_report() {
	let service = RuntimeService::default().with_runtime_mode_report(
		RuntimeModeReport::live_react_fallback_to_deterministic(
			"openrouter plugin disabled by startup policy",
		),
	);
	let report = service.runtime_mode_report();

	assert_eq!(report.requested, RuntimeExecutionMode::LiveReact);
	assert_eq!(report.effective, RuntimeExecutionMode::Deterministic);
	assert_eq!(
		report.fallback_reason.as_deref(),
		Some("openrouter plugin disabled by startup policy")
	);
}

#[test]
fn planning_mode_hint_returns_compatibility_fallback_without_graph() {
	let service = RuntimeService::default();
	let mut request = request("Read the first part of Cargo.toml.");
	request.planning_mode_hint = Some(PlanningModeHint::TreeSearch);

	let response = service
		.execute(request)
		.expect("compatibility fallback should succeed");

	assert_eq!(response.status, ResponseStatus::Succeeded);
	assert!(response.message.contains("planning-heavy"));

	let task = service
		.get_task(&TaskId("task-req-1".to_string()))
		.expect("task lookup should succeed")
		.expect("task should be persisted");
	assert!(task.graph.is_none());
}

#[test]
fn multistep_requests_enter_the_generic_loop_for_new_requests() {
	let service = RuntimeService::default();
	let response = service
		.execute(request(
			"Compare two migration strategies and then execute the better one.",
		))
		.expect("multistep request should still execute through the direct runtime");

	assert_eq!(response.status, ResponseStatus::Succeeded);
	assert!(!response.message.contains("planning-heavy"));

	let task = service
		.get_task(&TaskId("task-req-1".to_string()))
		.expect("task lookup should succeed")
		.expect("task should be persisted");
	assert!(task.graph.is_none());
	let last_result = task
		.last_result
		.as_ref()
		.expect("direct loop execution should persist a terminal result");
	let payload: serde_json::Value =
		serde_json::from_str(&last_result.payload).expect("payload should be valid json");
	assert_eq!(payload["runtime_loop"], "tool");
}

#[test]
fn pending_filesystem_tool_loops_resume_through_the_generic_loop_driver() {
	let service = RuntimeService::default();
	let cwd = env::current_dir().expect("cwd should resolve");
	let root_manifest = cwd.join("Cargo.toml").display().to_string();
	let nested_manifest = cwd
		.join("crates/roku-agent-runtime/Cargo.toml")
		.display()
		.to_string();
	let context = LoopContext {
		request_id: "req-pending-tool-loop".to_string(),
		session_id: "session-1".to_string(),
		goal: "帮我定位 Cargo.toml，然后告诉我这个 workspace 的 crate 组织".to_string(),
		workspace_root: cwd.display().to_string(),
		working_directory: cwd.display().to_string(),
		visible_tools: vec![
			"fs.read_text".to_string(),
			"fs.find".to_string(),
			"fs.inspect".to_string(),
			"general.execute".to_string(),
		],
		bound_resources: vec![ResourceSelector::tool("fs.read_text".to_string())],
		route_decision: RouteDecision::new(
			IntentFamily::FilesystemRead,
			0.94,
			false,
			RouteRisk::Low,
			vec!["fs.read_text".to_string(), "fs.find".to_string()],
			Vec::new(),
			Vec::new(),
			"filesystem request",
		),
		last_observation: None,
	};
	let mut loop_state = LoopState::new("loop-pending-tool-loop", &context);
	let observation = ToolObservation {
		ok: false,
		tool_name: "fs.read_text".to_string(),
		error_type: Some("multiple_candidates".to_string()),
		terminal: false,
		data: serde_json::json!({
			"path": "Cargo.toml",
			"matches": [root_manifest, nested_manifest],
		}),
		message: "Found 2 matching candidates for `Cargo.toml`.".to_string(),
	};
	let interpreted =
		roku_agent_runtime::interpret_observation(&loop_state, observation.clone(), None);
	loop_state.record_step(StepRecord::tool_call(
		1,
		roku_agent_runtime::NextStepDecision {
			action: roku_agent_runtime::NextStepAction::CallTool,
			tool_name: Some("fs.read_text".to_string()),
			arguments: Some(serde_json::json!({ "path": "Cargo.toml" })),
			reason: "Read the grounded workspace manifest first.".to_string(),
			final_message: None,
		},
		loop_state.visible_tools.clone(),
		serde_json::json!({
			"ok": false,
			"error_type": "multiple_candidates",
			"terminal": false,
			"message": "Found 2 matching candidates for `Cargo.toml`.",
			"data": observation.data.clone(),
		}),
		StepObservation::Tool(observation),
		interpreted.clone(),
		Some(12),
		interpreted.remaining_step_budget,
		interpreted.remaining_recovery_budget,
		cwd.display().to_string(),
	));
	loop_state.record_step(StepRecord::terminal(
		2,
		roku_agent_runtime::NextStepDecision {
			action: roku_agent_runtime::NextStepAction::AskUser,
			tool_name: None,
			arguments: None,
			reason: "Runtime paused for user clarification after the latest tool observation."
				.to_string(),
			final_message: Some("你想看哪一个 Cargo.toml？".to_string()),
		},
		loop_state.visible_tools.clone(),
		Some(StepObservation::AskUser {
			final_message: "你想看哪一个 Cargo.toml？".to_string(),
		}),
		3,
		2,
		cwd.display().to_string(),
	));
	loop_state.awaiting_user = Some(AskUserPayload {
		final_message: "你想看哪一个 Cargo.toml？".to_string(),
		resume_contract: AskUserResumeContract::CandidateSelection {
			candidates: vec![
				cwd.join("Cargo.toml").display().to_string(),
				cwd.join("crates/roku-agent-runtime/Cargo.toml")
					.display()
					.to_string(),
			],
		},
		resume_directive: Some(AskUserResumeDirective::RepeatToolWithSelectedCandidate {
			tool_name: "fs.read_text".to_string(),
			argument_key: "path".to_string(),
		}),
	});
	service
		.restore_pending_loop(loop_state)
		.expect("pending loop should restore");

	let response = service
		.execute(request("Cargo.toml"))
		.expect("pending loop should resume");

	assert_eq!(response.status, ResponseStatus::Failed);
	assert!(
		response
			.message
			.contains("general execution did not use a live runtime")
	);
	assert!(
		service
			.pending_loop("session-1")
			.expect("pending loop lookup should succeed")
			.is_none()
	);

	let task = service
		.get_task(&TaskId("task-req-1".to_string()))
		.expect("task lookup should succeed")
		.expect("task should be persisted");
	assert_eq!(task.state, TaskState::Failed);
	assert!(task.last_result.is_none());
	assert!(!response.artifacts.is_empty());
}

#[test]
fn stale_freeform_pending_loops_are_discarded_before_new_intake() {
	let service = RuntimeService::default();
	let cwd = env::current_dir().expect("cwd should resolve");
	let context = LoopContext {
		request_id: "req-freeform-pending".to_string(),
		session_id: "session-1".to_string(),
		goal: "继续".to_string(),
		workspace_root: cwd.display().to_string(),
		working_directory: cwd.display().to_string(),
		visible_tools: vec![
			"general.execute".to_string(),
			"inventory.describe".to_string(),
			"fs.find".to_string(),
		],
		bound_resources: vec![ResourceSelector::tool("general.execute".to_string())],
		route_decision: RouteDecision::new(
			IntentFamily::Chat,
			0.88,
			false,
			RouteRisk::Low,
			vec!["general.execute".to_string()],
			Vec::new(),
			Vec::new(),
			"freeform clarification request",
		),
		last_observation: None,
	};
	let mut loop_state = LoopState::new("loop-freeform-pending", &context);
	loop_state.status = roku_agent_runtime::LoopStatus::AwaitingUser;
	loop_state.awaiting_user = Some(AskUserPayload::freeform("您想继续什么任务？"));
	service
		.restore_pending_loop(loop_state)
		.expect("pending loop should restore");

	let response = service
		.execute(request("What skills and tools do you have right now?"))
		.expect("fresh intake should succeed");

	assert_eq!(response.status, ResponseStatus::Succeeded);
	assert_ne!(response.message, "您想继续什么任务？");
	assert!(
		service
			.pending_loop("session-1")
			.expect("pending loop lookup should succeed")
			.is_none()
	);
	let task = service
		.get_task(&TaskId("task-req-1".to_string()))
		.expect("task lookup should succeed")
		.expect("task should be persisted");
	assert_eq!(task.state, TaskState::Succeeded);
	assert!(task.last_result.is_some());
}

#[test]
fn historical_graph_tasks_remain_resumable() {
	let service = RuntimeService::default();
	let task_id = TaskId("task-legacy-resume".to_string());
	let graph = TaskGraph {
		task_id: task_id.clone(),
		nodes: vec![node("legacy-step", TaskNodeKind::Execution)],
		edges: Vec::new(),
	};
	let task = graph_task(
		&task_id.0,
		"req-legacy-resume",
		"Resume the old graph task",
		TaskState::Failed,
		graph,
	);

	service
		.start_experiment_run(&task, &task.goal, "legacy_graph")
		.expect("experiment should start");
	service.save_task(task).expect("task should persist");

	let report = service
		.get_task_replay_report(&task_id)
		.expect("replay report lookup should succeed")
		.expect("replay report should exist");
	assert_eq!(
		report.recovery_eligibility,
		RecoveryEligibility::ResumeReady
	);
	assert!(!report.resume_candidates.is_empty());

	let response = service
		.resume_task(&task_id)
		.expect("historical graph task should resume");
	assert_eq!(response.status, ResponseStatus::Succeeded);

	let resumed = service
		.get_task(&task_id)
		.expect("task lookup should succeed")
		.expect("task should exist");
	assert_eq!(resumed.state, TaskState::Succeeded);
	assert!(resumed.graph.is_some());
}

#[test]
fn historical_waiting_approval_graph_tasks_can_finish_after_decision() {
	let service = RuntimeService::default();
	let task_id = TaskId("task-legacy-approval".to_string());
	let request_id = RequestId("req-legacy-approval".to_string());
	let execution_node = node("legacy-step", TaskNodeKind::Execution);
	let approval_node = node("legacy-approval", TaskNodeKind::Approval);
	let approval_id = ApprovalId(compact_approval_id(&task_id.0, &approval_node.node_id.0));
	let graph = TaskGraph {
		task_id: task_id.clone(),
		nodes: vec![execution_node.clone(), approval_node.clone()],
		edges: vec![TaskEdge {
			from: execution_node.node_id.clone(),
			to: approval_node.node_id.clone(),
			condition: roku_common_types::TaskEdgeCondition::OnSuccess,
		}],
	};
	let mut task = graph_task(
		&task_id.0,
		&request_id.0,
		"Approve the historical graph task",
		TaskState::WaitingApproval,
		graph,
	);
	task.completed_nodes = vec![execution_node.node_id.clone()];
	task.next_node_index = 1;
	task.pending_approval_id = Some(approval_id.clone());
	task.last_result = Some(execution_result(
		&task_id,
		&execution_node.node_id.0,
		"execution finished",
	));

	service
		.start_experiment_run(&task, &task.goal, "legacy_graph")
		.expect("experiment should start");
	service.save_task(task).expect("task should persist");
	service
		.save_result(execution_result(
			&task_id,
			&execution_node.node_id.0,
			"execution finished",
		))
		.expect("execution result should persist");
	service
		.save_approval_ticket(ApprovalTicket {
			approval_id: approval_id.clone(),
			task_id: task_id.clone(),
			request_id: request_id.clone(),
			node_id: approval_node.node_id.clone(),
			summary: approval_node.description.clone(),
			status: ApprovalStatus::Pending,
			decided_by: None,
			comment: None,
		})
		.expect("approval ticket should persist");

	let replay = service
		.get_task_replay_report(&task_id)
		.expect("replay report lookup should succeed")
		.expect("replay report should exist");
	assert_eq!(
		replay.recovery_eligibility,
		RecoveryEligibility::PendingApproval
	);

	let response = service
		.decide_approval(
			&approval_id,
			ApprovalDecision {
				actor: "reviewer".to_string(),
				approved: true,
				comment: Some("approved".to_string()),
			},
		)
		.expect("approval should complete the historical graph task");
	assert_eq!(response.status, ResponseStatus::Succeeded);

	let task = service
		.get_task(&task_id)
		.expect("task lookup should succeed")
		.expect("task should exist");
	assert_eq!(task.state, TaskState::Succeeded);
	assert!(task.graph.is_some());
}
