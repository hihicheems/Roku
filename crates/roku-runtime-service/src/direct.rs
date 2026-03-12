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
	ResultStatus, RuntimeError, Task, TaskEventKind, TaskNode, TaskState,
};

use crate::RuntimeService;
use crate::helpers::{failure_message, result_message};

impl RuntimeService {
	pub(super) fn process_direct_route(
		&self,
		task: &mut Task,
		request: &RequestEnvelope,
		plan: &DirectRoutePlan,
		loop_state: &mut LoopState,
	) -> Result<ResponseEnvelope, RuntimeError> {
		let execution = self
			.runtime
			.execute_direct_route(&task.task_id, request, plan);
		let response =
			self.finalize_direct_path(task, execution.node, execution.result, execution.message)?;
		self.record_runtime_loop_terminal_step(loop_state, response.status, &response.message);
		Ok(response)
	}

	pub(super) fn process_direct_escalation(
		&self,
		task: &mut Task,
		request: &RequestEnvelope,
		plan: &RouteEscalationPlan,
		loop_state: &mut LoopState,
	) -> Result<ResponseEnvelope, RuntimeError> {
		let execution = self
			.runtime
			.execute_escalation_action(&task.task_id, request, plan);
		let response =
			self.finalize_direct_path(task, execution.node, execution.result, execution.message)?;
		self.record_runtime_loop_escalation_step(
			loop_state,
			plan.action,
			response.status,
			&response.message,
		);
		Ok(response)
	}

	fn finalize_direct_path(
		&self,
		task: &mut Task,
		node: TaskNode,
		mut result: ResultEnvelope,
		message: String,
	) -> Result<ResponseEnvelope, RuntimeError> {
		task.graph = None;
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

		task.last_result = Some(result.clone());
		self.mark_node_completed(task, &node);
		self.append_node_event(
			task,
			&node,
			TaskEventKind::NodeCompleted,
			"direct route execution completed",
		)?;

		self.record_transition(task, TaskState::Validating, "validate direct route")?;
		let validation_node = TaskNode {
			node_id: roku_common_types::NodeId("direct-route-validation".to_string()),
			kind: roku_common_types::TaskNodeKind::Validation,
			description: "Validate direct-route result".to_string(),
			..TaskNode::default()
		};
		let validation_report =
			self.validator
				.validate_evidence_set(&roku_common_types::ValidationEvidenceSet {
					result: result.clone(),
					artifacts: vec![artifact.clone()],
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
				artifacts: vec![artifact.uri],
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
