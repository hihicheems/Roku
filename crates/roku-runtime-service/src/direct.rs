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

use roku_agent_runtime::{DirectRoutePlan, LoopState, RouteEscalationPlan};
use roku_common_types::{
	ErrorClass, EvidenceItem, RequestEnvelope, ResponseEnvelope, ResponseStatus, ResultEnvelope,
	ResultStatus, RuntimeError, RuntimeMemorySections, Task, TaskEventKind, TaskNode, TaskState,
};

use crate::execution::pending_execution_approval_fact;
use crate::helpers::{bridge_async_to_sync, failure_message, result_message};
use crate::{ContextBundle, RuntimeService};

impl RuntimeService {
	pub(super) fn process_direct_route(
		&self,
		task: &mut Task,
		request: &RequestEnvelope,
		_plan: &DirectRoutePlan,
		loop_state: &mut LoopState,
		context_bundle: &ContextBundle,
		runtime_memory_sections: &RuntimeMemorySections,
	) -> Result<ResponseEnvelope, RuntimeError> {
		let initial_history_len = loop_state.history.len();
		// Unit 3 bridge: execute_tool_loop is now async. Bridge via block_in_place when
		// already inside a runtime (e.g., actix integration tests) or via a fresh multi-thread
		// runtime otherwise. A multi-thread runtime is required because execute_tool_loop uses
		// tokio::task::block_in_place internally for synchronous tool invocations.
		let execution = bridge_async_to_sync(self.runtime.execute_tool_loop(
			&task.task_id,
			request,
			loop_state,
			runtime_memory_sections,
			None,
			None,
		));
		self.record_runtime_loop_history(loop_state, initial_history_len);
		let response =
			self.finalize_direct_path(task, execution.node, execution.result, execution.message)?;
		if let Some(action) = execution.terminal_step_action {
			self.record_runtime_loop_terminal_step_with_action(
				loop_state,
				action,
				response.status,
				&response.message,
			);
		} else {
			self.record_runtime_loop_terminal_step(loop_state, response.status, &response.message);
		}
		self.sync_pending_loop(loop_state)?;
		self.apply_memory_write_back(request, &response, context_bundle);
		self.write_back_compact_summaries(request, loop_state);
		self.clear_runtime_memory_layers(&task.task_id);
		Ok(response)
	}

	pub(super) fn process_direct_escalation(
		&self,
		task: &mut Task,
		request: &RequestEnvelope,
		plan: &RouteEscalationPlan,
		loop_state: &mut LoopState,
		context_bundle: &ContextBundle,
		runtime_memory_sections: &RuntimeMemorySections,
	) -> Result<ResponseEnvelope, RuntimeError> {
		let execution = self.runtime.execute_escalation_action(
			&task.task_id,
			request,
			plan,
			runtime_memory_sections,
		);
		let response =
			self.finalize_direct_path(task, execution.node, execution.result, execution.message)?;
		if matches!(
			plan.action,
			roku_agent_runtime::EscalationAction::AskForMoreInfo
		) && !plan.decision.missing_arguments.is_empty()
		{
			self.record_runtime_loop_ask_user_payload_step(
				loop_state,
				response.status,
				roku_agent_runtime::AskUserPayload::missing_required_input(
					response.message.clone(),
					plan.decision.missing_arguments.clone(),
				),
			);
		} else {
			self.record_runtime_loop_escalation_step(
				loop_state,
				plan.action,
				response.status,
				&response.message,
			);
		}
		self.sync_pending_loop(loop_state)?;
		self.apply_memory_write_back(request, &response, context_bundle);
		self.clear_runtime_memory_layers(&task.task_id);
		Ok(response)
	}

	pub(crate) fn write_back_compact_summaries(
		&self,
		request: &RequestEnvelope,
		loop_state: &LoopState,
	) {
		if loop_state.working_summary.is_empty() {
			return;
		}
		let has_compact_boundary = loop_state
			.history
			.iter()
			.any(|step| step.action == roku_agent_runtime::StepAction::CompactBoundary);
		if !has_compact_boundary {
			return;
		}
		let write_request = roku_memory::MemoryWriteRequest::new(
			roku_memory::MemoryKind::WorkflowInsight,
			roku_memory::MemoryScope::Session,
			&loop_state.working_summary,
			"Context compact summary",
			roku_memory::MemoryWriteReason::CompactSummary,
		);
		let mut write_request = write_request;
		write_request.session_id = Some(request.session_id.clone());
		if let Err(error) = self.memory_backend.write(&write_request) {
			eprintln!("compact summary write-back failed: {error}");
		}
	}

	pub(super) fn finalize_direct_path(
		&self,
		task: &mut Task,
		node: TaskNode,
		mut result: ResultEnvelope,
		message: String,
	) -> Result<ResponseEnvelope, RuntimeError> {
		task.completed_nodes.clear();
		task.next_node_index = 0;
		task.pending_approval_id = None;
		task.last_result = None;

		if task.state == TaskState::Planning {
			self.record_transition(task, TaskState::GraphBuilding, "prepare direct route")?;
			self.record_transition(task, TaskState::Delegating, "dispatch direct route")?;
		}
		self.record_transition(task, TaskState::Executing, "execute direct route")?;
		let artifact = self.persist_result_artifact(&result)?;
		result.evidence.push(EvidenceItem {
			kind: "artifact_ref".to_string(),
			value: artifact.uri.clone(),
		});
		self.save_result(result.clone())?;
		self.attach_artifact_to_experiment(&task.task_id, artifact.artifact_id.clone())?;
		self.metrics.inc_artifacts();

		if matches!(result.status, ResultStatus::Error) {
			if let Some(pending_execution_approval) = pending_execution_approval_fact(&result) {
				return self.freeze_pending_execution_approval(
					task,
					&node,
					pending_execution_approval,
					&result.payload,
					&result.schema_version,
				);
			}
			self.metrics.inc_failures();
			let reason = result_message(&result);
			let terminal_state = self.fail_task(task, &reason, ErrorClass::NonRetriable)?;
			self.fail_experiment_run(task, &reason)?;
			self.save_task(task.clone())?;
			return Ok(ResponseEnvelope {
				request_id: task.request_id.clone(),
				status: ResponseStatus::Failed,
				message: failure_message(&reason, terminal_state),
				artifacts: vec![artifact.uri],
			});
		}

		self.complete_direct_runtime_path(
			task,
			&node,
			result,
			message,
			artifact,
			"direct route execution completed",
		)
	}

	pub(super) fn complete_direct_runtime_path(
		&self,
		task: &mut Task,
		node: &TaskNode,
		result: ResultEnvelope,
		message: String,
		result_artifact: roku_common_types::Artifact,
		node_completion_reason: &str,
	) -> Result<ResponseEnvelope, RuntimeError> {
		task.last_result = Some(result.clone());
		self.mark_node_completed(task, node);
		self.append_node_event(
			task,
			node,
			TaskEventKind::NodeCompleted,
			node_completion_reason,
		)?;

		self.record_transition(task, TaskState::Validating, "validate direct route")?;
		let validation_node = direct_validation_node();
		let validation_report =
			self.validator
				.validate_evidence_set(&roku_common_types::ValidationEvidenceSet {
					result: result.clone(),
					artifacts: vec![result_artifact.clone()],
				});
		let validation_result =
			build_direct_validation_result(task, &validation_node, &validation_report);
		self.save_result(validation_result.clone())?;
		self.mark_node_completed(task, &validation_node);
		self.append_node_event(
			task,
			&validation_node,
			TaskEventKind::NodeCompleted,
			"direct route validation completed",
		)?;

		if !validation_report.accepted {
			self.metrics.inc_failures();
			self.metrics.inc_validation_failures();
			let reason = validation_report.failures.join(", ");
			let terminal_state =
				self.fail_task(task, "validation failed", ErrorClass::Validation)?;
			self.fail_experiment_run(task, &reason)?;
			self.save_task(task.clone())?;
			return Ok(ResponseEnvelope {
				request_id: task.request_id.clone(),
				status: ResponseStatus::Failed,
				message: failure_message(&reason, terminal_state),
				artifacts: vec![result_artifact.uri],
			});
		}

		self.record_transition(task, TaskState::Aggregating, "aggregate direct route")?;
		self.record_transition(task, TaskState::Succeeded, "done")?;
		self.complete_experiment_run(task, self.list_results(&task.task_id)?.len())?;
		self.save_task(task.clone())?;
		let artifacts = self
			.list_artifacts(&task.task_id)?
			.into_iter()
			.map(|artifact| artifact.uri)
			.collect::<Vec<_>>();

		Ok(ResponseEnvelope {
			request_id: task.request_id.clone(),
			status: ResponseStatus::Succeeded,
			message,
			artifacts,
		})
	}
}

fn direct_validation_node() -> TaskNode {
	TaskNode {
		node_id: roku_common_types::NodeId("direct-route-validation".to_string()),
		kind: roku_common_types::TaskNodeKind::Validation,
		description: "Validate direct-route result".to_string(),
		..TaskNode::default()
	}
}

fn build_direct_validation_result(
	task: &Task,
	node: &TaskNode,
	report: &roku_common_types::ValidationReport,
) -> ResultEnvelope {
	let accepted = report.accepted;
	let message = if accepted {
		"validated 1 result(s)".to_string()
	} else {
		format!("validation failed: {}", report.failures.join(", "))
	};
	ResultEnvelope {
		task_id: task.task_id.clone(),
		node_id: node.node_id.clone(),
		producer: format!("validation:{}", node.node_id.0),
		schema_version: "result.v1".to_string(),
		status: if accepted {
			ResultStatus::Ok
		} else {
			ResultStatus::Error
		},
		payload: serde_json::json!({
			"message": message,
			"accepted": accepted,
			"failures": report.failures.clone(),
		})
		.to_string(),
		evidence: vec![EvidenceItem {
			kind: "validation".to_string(),
			value: format!("accepted={accepted}"),
		}],
		confidence: if accepted { 1.0 } else { 0.0 },
	}
}

#[cfg(test)]
mod tests {
	use std::env;

	use roku_common_types::{
		ApprovalDecision, ApprovalRequirement, ApprovalRequirementScope, CanonicalDigest,
		CanonicalExecution, ExecutionActionClass, ExecutionEnvPolicy, ExecutionEnvPolicyMode,
		ExecutionResourceScope, InvocationMode, NodeId, PolicyDecision, PolicyOutcome,
		PolicyReasonCode, RequestId, ResourceSelector, ResponseStatus, RetryPolicy, TaskId,
		TaskNodeKind,
	};
	use serde_json::json;
	use tempfile::tempdir;

	use super::*;

	fn sample_policy_decision() -> PolicyDecision {
		PolicyDecision {
			outcome: PolicyOutcome::RequireApproval,
			reason_code: PolicyReasonCode::ApprovalRequiredByUntrustedProgram,
			approval_requirement: Some(ApprovalRequirement {
				scope: ApprovalRequirementScope::Invocation,
				reason_code: PolicyReasonCode::ApprovalRequiredByUntrustedProgram,
			}),
		}
	}

	fn sample_fs_policy_decision() -> PolicyDecision {
		PolicyDecision {
			outcome: PolicyOutcome::RequireApproval,
			reason_code: PolicyReasonCode::ApprovalRequiredByOutOfScopePath,
			approval_requirement: Some(ApprovalRequirement {
				scope: ApprovalRequirementScope::Invocation,
				reason_code: PolicyReasonCode::ApprovalRequiredByOutOfScopePath,
			}),
		}
	}

	fn sample_canonical_execution(cwd: &str) -> CanonicalExecution {
		CanonicalExecution {
			tool_name: "command.run".to_string(),
			program: "pwd".to_string(),
			argv: vec!["pwd".to_string()],
			invocation_mode: InvocationMode::DirectExec,
			shell_context: None,
			cwd: cwd.to_string(),
			env_policy: ExecutionEnvPolicy {
				mode: ExecutionEnvPolicyMode::InheritSelected,
				allowed_keys: vec!["ROKU_COMMAND_SCOPE_ROOT".to_string()],
			},
			resource_scope: ExecutionResourceScope {
				working_directory: cwd.to_string(),
				resolved_targets: vec![cwd.to_string()],
				effective_read_roots: vec![cwd.to_string()],
				effective_write_roots: Vec::new(),
			},
			action_class: ExecutionActionClass::Exec,
			digest: CanonicalDigest("direct-route-digest".to_string()),
		}
	}

	fn sample_direct_route_result(task_id: &TaskId, node_id: &NodeId, cwd: &str) -> ResultEnvelope {
		ResultEnvelope {
			task_id: task_id.clone(),
			node_id: node_id.clone(),
			producer: "runtime-loop".to_string(),
			schema_version: "result.v1".to_string(),
			status: ResultStatus::Error,
			payload: json!({
				"error_code": "approval_required",
				"message": "approval required",
				"tool_name": "command.run",
				"tool_input": {
					"command": "pwd",
					"cwd": cwd
				},
				"policy_decision": sample_policy_decision(),
				"canonical_execution": sample_canonical_execution(cwd),
				"digest": "direct-route-digest",
				"direct_route": true,
				"runtime_loop": "tool"
			})
			.to_string(),
			evidence: Vec::new(),
			confidence: 0.0,
		}
	}

	fn sample_fs_direct_route_result(
		task_id: &TaskId,
		node_id: &NodeId,
		workspace_cwd: &str,
		target_path: &str,
	) -> ResultEnvelope {
		ResultEnvelope {
			task_id: task_id.clone(),
			node_id: node_id.clone(),
			producer: "runtime-loop".to_string(),
			schema_version: "result.v1".to_string(),
			status: ResultStatus::Error,
			payload: json!({
				"error_code": "approval_required",
				"message": "approval required",
				"tool_name": "fs.list_dir",
				"tool_input": {
					"task_id": task_id.0,
					"node_id": node_id.0,
					"goal": "list an out-of-scope directory after approval",
					"summary": "direct route filesystem review",
					"conversation_history": "",
					"budget_tokens": 8000,
					"time_budget_ms": 60000,
					"path": target_path
				},
				"policy_decision": sample_fs_policy_decision(),
				"canonical_execution": {
					"tool_name": "fs.list_dir",
					"program": "fs.list_dir",
					"argv": ["fs.list_dir", target_path],
					"invocation_mode": "direct_exec",
					"shell_context": null,
					"cwd": workspace_cwd,
					"env_policy": {
						"mode": "clean",
						"allowed_keys": []
					},
					"resource_scope": {
						"working_directory": workspace_cwd,
						"resolved_targets": [target_path],
						"effective_read_roots": [workspace_cwd],
						"effective_write_roots": []
					},
					"action_class": "read",
					"digest": "direct-route-fs-digest"
				},
				"digest": "direct-route-fs-digest",
				"direct_route": true,
				"runtime_loop": "tool"
			})
			.to_string(),
			evidence: Vec::new(),
			confidence: 0.0,
		}
	}

	fn direct_route_task(task_id: &str, request_id: &str, goal: &str) -> Task {
		Task {
			task_id: TaskId(task_id.to_string()),
			request_id: RequestId(request_id.to_string()),
			session_id: "session-direct-route".to_string(),
			goal: goal.to_string(),
			state: TaskState::Planning,
			attempts: 0,
			conversation_history: Vec::new(),
			completed_nodes: Vec::new(),
			next_node_index: 0,
			pending_approval_id: None,
			last_result: None,
			compensation_records: Vec::new(),
		}
	}

	#[test]
	fn finalize_direct_path_resumes_execution_approval_without_synthesized_graph() {
		let service = RuntimeService::default();
		let directory = tempdir().expect("tempdir should succeed");
		let cwd = directory
			.path()
			.canonicalize()
			.expect("tempdir should canonicalize");
		let cwd_text = cwd.display().to_string();
		let mut task = direct_route_task(
			"task-direct-route-approval",
			"req-direct-route-approval",
			"run the approved direct-route command",
		);
		let node = TaskNode {
			node_id: NodeId("direct-route".to_string()),
			kind: TaskNodeKind::Execution,
			description: "direct route command review".to_string(),
			retry_policy: RetryPolicy::default(),
			..TaskNode::default()
		};
		service
			.start_experiment_run(&task, &task.goal, "direct_route")
			.expect("direct route experiment should start");
		let result = sample_direct_route_result(&task.task_id, &node.node_id, &cwd_text);

		let response = service
			.finalize_direct_path(&mut task, node.clone(), result, "ignored".to_string())
			.expect("direct route approval should freeze instead of failing");

		assert_eq!(response.status, ResponseStatus::PendingApproval);
		assert_eq!(
			response.message,
			format!(
				"🛡️ Approval Request\n\nTool: command.run\nAction: Run command pwd from {cwd_text}\nRisk: medium\nReason: the command is outside the constrained built-in allowlist"
			)
		);
		assert_eq!(task.state, TaskState::WaitingApproval);

		let approval_id = task
			.pending_approval_id
			.clone()
			.expect("approval id should be stored on the task");
		let resumed = service
			.decide_approval(
				&approval_id,
				ApprovalDecision {
					actor: "reviewer".to_string(),
					approved: true,
					comment: Some("looks good".to_string()),
				},
			)
			.expect("approved direct-route ticket should resume");

		assert_eq!(resumed.status, ResponseStatus::Succeeded);
		let persisted_task = service
			.get_task(&task.task_id)
			.expect("task lookup should succeed")
			.expect("task should persist");
		assert_eq!(persisted_task.state, TaskState::Succeeded);
		assert_eq!(
			persisted_task.completed_nodes,
			vec![
				node.node_id.clone(),
				NodeId("direct-route-validation".to_string()),
			]
		);

		let experiment = service
			.get_experiment_run(&task.task_id)
			.expect("experiment lookup should succeed")
			.expect("direct route task should keep its experiment run");
		assert_eq!(experiment.strategy, "direct_route");

		let node_id = node.node_id.0.clone();
		let node_events = service
			.list_task_events(&task.task_id)
			.expect("event lookup should succeed")
			.into_iter()
			.filter_map(|event| {
				event
					.node_id
					.map(|event_node_id| (event.kind, event_node_id.0))
			})
			.filter(|(_, event_node_id)| event_node_id == &node_id)
			.collect::<Vec<_>>();
		assert_eq!(
			node_events,
			vec![
				(TaskEventKind::ApprovalPending, node_id.clone()),
				(TaskEventKind::ApprovalApproved, node_id.clone()),
				(TaskEventKind::NodeCompleted, node_id.clone()),
			]
		);

		let result = service
			.list_results(&task.task_id)
			.expect("result lookup should succeed")
			.into_iter()
			.find(|result| result.node_id == node.node_id)
			.expect("execution result should be persisted");
		assert_eq!(result.producer, "approval-resume:direct-route");
		assert!(
			result
				.evidence
				.iter()
				.any(|item| { item.kind == "runtime" && item.value == "approval-resume" })
		);
		assert!(result.evidence.iter().any(|item| {
			item.kind == "execution_digest" && item.value == "direct-route-digest"
		}));
		assert!(
			result
				.evidence
				.iter()
				.any(|item| item.kind == "frozen_payload_ref")
		);
		let payload = serde_json::from_str::<serde_json::Value>(&result.payload)
			.expect("payload should decode");
		assert_eq!(payload["tool_name"], "command.run");
		assert_eq!(payload["output"]["data"]["command"], "pwd");
		assert_eq!(payload["output"]["data"]["digest"], "direct-route-digest");
	}

	#[test]
	fn finalize_direct_path_freezes_and_resumes_filesystem_path_approval() {
		let service = RuntimeService::default();
		let workspace_cwd = env::current_dir()
			.expect("cwd should resolve")
			.canonicalize()
			.expect("cwd should canonicalize");
		let workspace_cwd_text = workspace_cwd.display().to_string();
		let directory = tempdir().expect("tempdir should succeed");
		std::fs::write(directory.path().join("visible.txt"), "hello")
			.expect("tempdir seed file should write");
		let target_path = directory
			.path()
			.canonicalize()
			.expect("tempdir should canonicalize");
		let target_path_text = target_path.display().to_string();
		let mut task = direct_route_task(
			"task-direct-route-fs-approval",
			"req-direct-route-fs-approval",
			"list an out-of-scope directory after approval",
		);
		let node = TaskNode {
			node_id: NodeId("direct-route".to_string()),
			kind: TaskNodeKind::Execution,
			description: "direct route filesystem review".to_string(),
			resources: vec![ResourceSelector::tool("fs.list_dir".to_string())],
			capabilities: vec!["fs.list_dir".to_string()],
			retry_policy: RetryPolicy::default(),
			..TaskNode::default()
		};
		service
			.start_experiment_run(&task, &task.goal, "direct_route")
			.expect("direct route experiment should start");
		let result = sample_fs_direct_route_result(
			&task.task_id,
			&node.node_id,
			&workspace_cwd_text,
			&target_path_text,
		);

		let response = service
			.finalize_direct_path(&mut task, node.clone(), result, "ignored".to_string())
			.expect("filesystem approval should freeze instead of failing");

		assert_eq!(response.status, ResponseStatus::PendingApproval);
		assert_eq!(
			response.message,
			format!(
				"🛡️ Approval Request\n\nTool: fs.list_dir\nAction: List directory {target_path_text}\nRisk: high\nReason: the requested path is outside the current allowed workspace roots"
			)
		);

		let approval_id = task
			.pending_approval_id
			.clone()
			.expect("approval id should be stored on the task");
		let resumed = service
			.decide_approval(
				&approval_id,
				ApprovalDecision {
					actor: "reviewer".to_string(),
					approved: true,
					comment: Some("allow one directory listing".to_string()),
				},
			)
			.expect("approved filesystem ticket should resume");

		assert_eq!(resumed.status, ResponseStatus::Succeeded);
		let result = service
			.list_results(&task.task_id)
			.expect("result lookup should succeed")
			.into_iter()
			.find(|result| result.node_id == node.node_id)
			.expect("execution result should be persisted");
		let payload = serde_json::from_str::<serde_json::Value>(&result.payload)
			.expect("payload should decode");
		assert_eq!(payload["tool_name"], "fs.list_dir");
		assert_eq!(payload["output"]["data"]["path"], target_path_text);
	}
}
