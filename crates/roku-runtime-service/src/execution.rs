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

use roku_agent_runtime::AgentWorker;
use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::time::Instant;

use roku_common_types::{
	ApprovalId, ApprovalStatus, ApprovalTicket, ApprovedExecutionRef, CanonicalExecution,
	CapabilityToken, CompensationAction, CompensationRecord, CompensationStatus, ErrorClass,
	EvidenceItem, ExecutionEnvPolicyMode, ExecutionPreview, InvocationMode,
	PendingExecutionApproval, PolicyDecision, PolicyOutcome, RecoveryEligibility, ResponseEnvelope,
	ResponseStatus, ResultEnvelope, ResultStatus, RuntimeError, Task, TaskEventKind, TaskId,
	TaskNode, TaskNodeKind, TaskState, ToolOutputEnvelope, project_execution_preview,
};
use roku_observability::{AuditCorrelation, AuditRecord};
use serde_json::{Value, json};

use crate::helpers::{approval_artifact, failure_message, result_message};
use crate::legacy_graph::{
	LegacyTaskGraphScheduler, assess_graph_completion, build_agent_instance_for_node_with_history,
};
use crate::{RunMode, RuntimeService, compact_approval_id};

#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingExecutionApprovalFact {
	policy_decision: PolicyDecision,
	canonical_execution: CanonicalExecution,
}

#[derive(Debug, Clone)]
pub(super) struct ValidatedExecutionResume {
	node: TaskNode,
	pending_execution: PendingExecutionApproval,
	frozen_payload_ref: String,
}

impl RuntimeService {
	pub(super) fn process_task(
		&self,
		task: &mut Task,
		mode: RunMode,
	) -> Result<ResponseEnvelope, RuntimeError> {
		let request_id = task.request_id.clone();
		let graph = task
			.graph
			.clone()
			.ok_or_else(|| RuntimeError::new("task graph is missing"))?;
		let scheduler = LegacyTaskGraphScheduler;

		while !scheduler
			.is_complete(&graph, &task.completed_nodes)
			.map_err(|error| RuntimeError::new(error.to_string()))?
		{
			let ready_nodes = scheduler
				.ready_nodes(&graph, &task.completed_nodes)
				.map_err(|error| RuntimeError::new(error.to_string()))?;
			if ready_nodes.is_empty() {
				return Err(RuntimeError::new(
					"task graph has no ready nodes but is not complete",
				));
			}
			let ready_nodes_by_id = ready_nodes
				.iter()
				.cloned()
				.map(|node| (node.node_id.0.clone(), node))
				.collect::<HashMap<_, _>>();
			self.enqueue_ready_nodes(task, &ready_nodes)?;

			while let Some(claim) = self.claim_dispatched_node(&task.task_id)? {
				let node = ready_nodes_by_id
					.get(&claim.envelope.node_id.0)
					.ok_or_else(|| {
						RuntimeError::new(format!(
							"dispatched node {} is not ready for task {}",
							claim.envelope.node_id.0, task.task_id.0
						))
					})?;
				if task
					.completed_nodes
					.iter()
					.any(|completed| completed == &node.node_id)
				{
					self.ack_dispatched_node(&claim.lease)?;
					continue;
				}
				match node.kind {
					TaskNodeKind::Execution => {
						if let Some(response) = self.process_execution_node(task, node, mode)? {
							self.ack_dispatched_node(&claim.lease)?;
							return Ok(response);
						}
						self.ack_dispatched_node(&claim.lease)?;
					}
					TaskNodeKind::Approval => {
						let response = self.process_approval_node(task, node)?;
						self.ack_dispatched_node(&claim.lease)?;
						return Ok(response);
					}
					TaskNodeKind::Validation => {
						let report = self.process_validation_node(task, node, mode)?;
						self.ack_dispatched_node(&claim.lease)?;
						if let Some(response) = report {
							return Ok(response);
						}
					}
					TaskNodeKind::Aggregation => {
						self.process_aggregation_node(task, node)?;
						self.ack_dispatched_node(&claim.lease)?;
					}
					TaskNodeKind::Retry | TaskNodeKind::DeadLetter => {
						return Err(RuntimeError::new(format!(
							"manual recovery helper node {} was dispatched on the automatic path",
							node.node_id.0
						)));
					}
				}
			}
		}

		self.finalize_task(task, request_id)
	}

	pub fn resume_task(&self, task_id: &TaskId) -> Result<ResponseEnvelope, RuntimeError> {
		let task = self
			.get_task(task_id)?
			.ok_or_else(|| RuntimeError::new(format!("task not found: {}", task_id.0)))?;
		let analysis = self.analyze_task_recovery(&task)?;
		let mut task = analysis.reconstructed_task.clone();

		match task.state {
			TaskState::Succeeded | TaskState::Cancelled | TaskState::DeadLetter => {
				return Err(RuntimeError::new(format!(
					"task is not resumable from state {:?}",
					task.state
				)));
			}
			TaskState::WaitingApproval => {
				let approval_id = task.pending_approval_id.clone().ok_or_else(|| {
					RuntimeError::new("task is waiting for approval but no approval id is stored")
				})?;
				let message = self
					.get_approval(&approval_id)?
					.map(|ticket| pending_approval_message(&ticket))
					.unwrap_or_else(|| format!("approval required for {}", approval_id.0));
				return Ok(ResponseEnvelope {
					request_id: task.request_id.clone(),
					status: ResponseStatus::PendingApproval,
					message,
					artifacts: vec![approval_artifact(&approval_id)],
				});
			}
			TaskState::Aggregating => {
				let request_id = task.request_id.clone();
				return self.finalize_task(&mut task, request_id);
			}
			_ => {}
		}

		let target_state = match analysis.recovery_eligibility {
			RecoveryEligibility::FinalizeReady if analysis.is_complete => TaskState::Validating,
			RecoveryEligibility::ResumeReady => classify_resume_state(&analysis.ready_nodes),
			RecoveryEligibility::RequiresManualResume => {
				return Err(RuntimeError::new(
					"task requires manual resume before automatic execution can continue",
				));
			}
			RecoveryEligibility::Blocked => {
				return Err(RuntimeError::new(
					"task graph has no replay-ready nodes and cannot be resumed",
				));
			}
			RecoveryEligibility::NotRecoverable => {
				return Err(RuntimeError::new(format!(
					"task is not resumable from state {:?}",
					task.state
				)));
			}
			RecoveryEligibility::PendingApproval => {
				return Err(RuntimeError::new(
					"task is waiting for approval and must be resumed through approval flow",
				));
			}
			RecoveryEligibility::FinalizeReady => TaskState::Validating,
		};
		self.normalize_task_for_resume(&mut task, target_state)?;
		self.process_task(&mut task, RunMode::Normal)
	}

	pub fn cancel_task(&self, task_id: &TaskId, actor: &str) -> Result<Task, RuntimeError> {
		let mut task = self
			.get_task(task_id)?
			.ok_or_else(|| RuntimeError::new(format!("task not found: {}", task_id.0)))?;

		match task.state {
			TaskState::Succeeded | TaskState::DeadLetter | TaskState::Cancelled => {
				return Err(RuntimeError::new(format!(
					"task cannot be cancelled from state {:?}",
					task.state
				)));
			}
			TaskState::CancelRequested | TaskState::Compensating => {
				return Err(RuntimeError::new(
					"task cancellation is already in progress",
				));
			}
			_ => {}
		}

		if let Some(approval_id) = task.pending_approval_id.clone() {
			self.cancel_pending_approval_ticket(&approval_id, actor)?;
			task.pending_approval_id = None;
		}

		task.compensation_records = self.plan_compensation_records(&task);
		let has_compensation_work = !task.compensation_records.is_empty();
		let disposition = self.orchestrator.request_cancellation(
			&mut task,
			format!("cancel requested by {actor}"),
			has_compensation_work,
		)?;
		let mut state = self.lock_state()?;
		for event in disposition {
			state
				.event_repo
				.append_event(event)
				.map_err(|error| RuntimeError::new(error.to_string()))?;
		}
		drop(state);

		self.complete_compensation_records(&mut task, actor);
		if self.get_experiment_run(&task.task_id)?.is_some() {
			self.fail_experiment_run(&task, "task cancelled")?;
		}
		self.save_task(task.clone())?;
		Ok(task)
	}

	pub fn recover_timed_out_task(
		&self,
		task_id: &TaskId,
	) -> Result<ResponseEnvelope, RuntimeError> {
		let task = self
			.get_task(task_id)?
			.ok_or_else(|| RuntimeError::new(format!("task not found: {}", task_id.0)))?;
		if task.state != TaskState::TimeoutRecovering {
			return Err(RuntimeError::new(format!(
				"task is not waiting for timeout recovery from state {:?}",
				task.state
			)));
		}

		self.resume_task(task_id)
	}

	pub(super) fn fail_task(
		&self,
		task: &mut Task,
		reason: &str,
		error_class: roku_common_types::ErrorClass,
	) -> Result<TaskState, RuntimeError> {
		let disposition = self
			.orchestrator
			.register_failure(task, reason, Some(error_class))?;
		let mut state = self.lock_state()?;
		for event in disposition.events {
			state
				.event_repo
				.append_event(event)
				.map_err(|error| RuntimeError::new(error.to_string()))?;
		}
		if disposition.terminal_state == TaskState::DeadLetter {
			self.metrics.inc_dead_letters();
		}
		Ok(disposition.terminal_state)
	}

	pub(super) fn process_execution_node(
		&self,
		task: &mut Task,
		node: &TaskNode,
		mode: RunMode,
	) -> Result<Option<ResponseEnvelope>, RuntimeError> {
		let memory_context = self.task_memory_context(&task.task_id);
		let mut spec = build_agent_instance_for_node_with_history(task, node, &memory_context);
		let capability_allowed = {
			let mut state = self.lock_state()?;
			let capability_tokens = issue_node_capability_tokens(
				&mut state.capability_auth,
				&spec.instance_id,
				node,
				self.runtime.resource_catalog(),
				!matches!(mode, RunMode::CapabilityDenied),
			);
			let is_allowed =
				verify_node_capabilities(&mut state.capability_auth, node, &capability_tokens);
			spec.capabilities = flatten_granted_capabilities(&capability_tokens);
			spec.capability_tokens = capability_tokens.clone();

			if !is_allowed {
				self.audit_sink
					.record(
						AuditRecord::new(
							spec.instance_id.clone(),
							"invoke",
							node_resource_label(node),
							"denied",
						)
						.with_correlation(AuditCorrelation {
							trace_id: format!("trace-{}", task.request_id.0),
							span_id: "capability-check".to_string(),
							task_id: Some(task.task_id.0.clone()),
							request_id: Some(task.request_id.0.clone()),
						})
						.with_attribute("node_id", node.node_id.0.clone()),
					)
					.map_err(|error| RuntimeError::new(error.to_string()))?;
			}

			is_allowed
		};

		if !capability_allowed {
			self.metrics.inc_failures();
			if matches!(mode, RunMode::RetryExhausted) {
				task.attempts = self.orchestrator.config.max_attempts.saturating_sub(1);
			}
			let terminal_state = self.fail_task(task, "capability denied", ErrorClass::Security)?;
			self.fail_experiment_run(task, "capability denied")?;
			self.save_task(task.clone())?;

			return Ok(Some(ResponseEnvelope {
				request_id: task.request_id.clone(),
				status: ResponseStatus::Failed,
				message: failure_message("capability denied", terminal_state),
				artifacts: Vec::new(),
			}));
		}

		if task.state != TaskState::Executing {
			self.record_transition(task, TaskState::Executing, "execute")?;
		}
		if matches!(mode, RunMode::TimeoutRecovery) {
			self.record_transition_with_error_class(
				task,
				TaskState::TimeoutRecovering,
				"execution timed out",
				Some(ErrorClass::Timeout),
			)?;
			self.save_task(task.clone())?;

			return Ok(Some(ResponseEnvelope {
				request_id: task.request_id.clone(),
				status: ResponseStatus::Failed,
				message: "task timed out and entered recovery flow".to_string(),
				artifacts: Vec::new(),
			}));
		}
		let mut result = self.runtime.execute(&spec, node);
		if let Some(limit_ms) = node_time_budget_limit_ms(&spec, node)
			&& let Some(elapsed_ms) = result_elapsed_ms(&result)
			&& elapsed_ms > limit_ms
		{
			result = node_budget_timeout_result(&spec, node, elapsed_ms, limit_ms);
		}
		let artifact = self.persist_result_artifact(&result)?;
		if matches!(mode, RunMode::MissingEvidence | RunMode::RetryExhausted) {
			result.evidence.clear();
		} else {
			result.evidence.push(EvidenceItem {
				kind: "artifact_ref".to_string(),
				value: artifact.uri.clone(),
			});
		}

		self.save_result(result.clone())?;
		self.attach_artifact_to_experiment(&task.task_id, artifact.artifact_id.clone())?;
		self.metrics.inc_artifacts();

		if matches!(result.status, ResultStatus::Error) {
			if let Some(pending_execution_approval) = pending_execution_approval_fact(&result) {
				return Ok(Some(self.freeze_pending_execution_approval(
					task,
					node,
					pending_execution_approval,
					&result.payload,
					&result.schema_version,
				)?));
			}

			self.metrics.inc_failures();
			if matches!(mode, RunMode::RetryExhausted) {
				task.attempts = self.orchestrator.config.max_attempts.saturating_sub(1);
			}
			let reason = result_message(&result);
			let error_class = classify_result_error(&result);
			if matches!(error_class, ErrorClass::Timeout) && node.retry_policy.retry_on_timeout {
				self.record_transition_with_error_class(
					task,
					TaskState::TimeoutRecovering,
					&reason,
					Some(ErrorClass::Timeout),
				)?;
				self.save_task(task.clone())?;

				return Ok(Some(ResponseEnvelope {
					request_id: task.request_id.clone(),
					status: ResponseStatus::Failed,
					message: "task timed out and entered recovery flow".to_string(),
					artifacts: vec![artifact.uri],
				}));
			}
			let terminal_state = self.fail_task(task, &reason, error_class)?;
			self.fail_experiment_run(task, &reason)?;
			self.save_task(task.clone())?;

			return Ok(Some(ResponseEnvelope {
				request_id: task.request_id.clone(),
				status: ResponseStatus::Failed,
				message: failure_message(&reason, terminal_state),
				artifacts: vec![artifact.uri],
			}));
		}

		task.last_result = Some(result);
		self.mark_node_completed(task, node);
		self.append_node_event(
			task,
			node,
			TaskEventKind::NodeCompleted,
			"execution completed",
		)?;
		Ok(None)
	}

	pub(super) fn process_validation_node(
		&self,
		task: &mut Task,
		node: &TaskNode,
		mode: RunMode,
	) -> Result<Option<ResponseEnvelope>, RuntimeError> {
		let evidence_sets = self.collect_validation_evidence(task, node)?;
		if evidence_sets.is_empty() {
			return Err(RuntimeError::new(
				"validation node reached before upstream execution results",
			));
		}
		self.record_transition(task, TaskState::Validating, "validate")?;
		let mut failures = Vec::new();
		for evidence_set in &evidence_sets {
			let report = self.validator.validate_evidence_set(evidence_set);
			if !report.accepted {
				failures.extend(report.failures);
			}
		}
		if !failures.is_empty() {
			self.metrics.inc_failures();
			self.metrics.inc_validation_failures();
			if matches!(mode, RunMode::RetryExhausted) {
				task.attempts = self.orchestrator.config.max_attempts.saturating_sub(1);
			}
			let terminal_state =
				self.fail_task(task, "validation failed", ErrorClass::Validation)?;
			self.fail_experiment_run(task, &failures.join(", "))?;
			self.save_task(task.clone())?;

			return Ok(Some(ResponseEnvelope {
				request_id: task.request_id.clone(),
				status: ResponseStatus::Failed,
				message: failure_message(&failures.join(", "), terminal_state),
				artifacts: Vec::new(),
			}));
		}

		for evidence_set in &evidence_sets {
			self.audit_sink
				.record(
					AuditRecord::new(
						evidence_set.result.producer.clone(),
						"validate",
						evidence_set.result.schema_version.clone(),
						"accepted",
					)
					.with_correlation(AuditCorrelation {
						trace_id: format!("trace-{}", task.request_id.0),
						span_id: "validation".to_string(),
						task_id: Some(task.task_id.0.clone()),
						request_id: Some(task.request_id.0.clone()),
					})
					.with_attribute("node_id", evidence_set.result.node_id.0.clone()),
				)
				.map_err(|error| RuntimeError::new(error.to_string()))?;
		}
		let validated_node_ids = evidence_sets
			.iter()
			.map(|evidence_set| evidence_set.result.node_id.0.clone())
			.collect::<Vec<_>>();
		let validated_messages = evidence_sets
			.iter()
			.map(|evidence_set| result_message(&evidence_set.result))
			.collect::<Vec<_>>();
		self.save_result(ResultEnvelope {
			task_id: task.task_id.clone(),
			node_id: node.node_id.clone(),
			producer: format!("validation:{}", node.node_id.0),
			schema_version: "result.v1".to_string(),
			status: ResultStatus::Ok,
			payload: serde_json::json!({
				"message": format!("validated {} result(s)", validated_node_ids.len()),
				"node_id": node.node_id.0,
				"validated_node_ids": validated_node_ids,
				"validated_messages": validated_messages,
			})
			.to_string(),
			evidence: vec![EvidenceItem {
				kind: "validation".to_string(),
				value: format!("accepted={}", evidence_sets.len()),
			}],
			confidence: 1.0,
		})?;
		self.mark_node_completed(task, node);
		self.append_node_event(
			task,
			node,
			TaskEventKind::NodeCompleted,
			"validation completed",
		)?;

		Ok(None)
	}

	pub(super) fn process_aggregation_node(
		&self,
		task: &mut Task,
		node: &TaskNode,
	) -> Result<(), RuntimeError> {
		let result_set = self.collect_node_result_set(task, node)?;
		if result_set.results.is_empty() {
			return Err(RuntimeError::new(format!(
				"aggregation node {} has no upstream results",
				node.node_id.0
			)));
		}
		if task.state != TaskState::Aggregating {
			self.record_transition(task, TaskState::Aggregating, "aggregate")?;
		}
		let representative_result =
			result_set.results.first().cloned().ok_or_else(|| {
				RuntimeError::new("aggregation node has no representative result")
			})?;
		let selected_result_node_ids = result_set
			.results
			.iter()
			.map(|result| result.node_id.0.clone())
			.collect::<Vec<_>>();
		let aggregated_confidence = result_set
			.results
			.iter()
			.map(|result| result.confidence)
			.fold(0.0_f32, f32::max);
		let mut aggregation_result = ResultEnvelope {
			task_id: task.task_id.clone(),
			node_id: node.node_id.clone(),
			producer: format!("aggregation:{}", node.node_id.0),
			schema_version: "result.v1".to_string(),
			status: ResultStatus::Ok,
			payload: serde_json::json!({
				"message": result_message(&representative_result),
				"node_id": node.node_id.0,
				"source_node_ids": result_set.source_node_ids.iter().map(|node_id| node_id.0.clone()).collect::<Vec<_>>(),
				"selected_result_node_ids": selected_result_node_ids,
				"aggregation_mode": format!("{:?}", result_set.aggregation_mode),
				"result_count": result_set.results.len(),
			})
			.to_string(),
			evidence: vec![EvidenceItem {
				kind: "aggregation".to_string(),
				value: format!("sources={}", result_set.source_node_ids.len()),
			}],
			confidence: aggregated_confidence,
		};
		let artifact = self.persist_result_artifact(&aggregation_result)?;
		aggregation_result.evidence.push(EvidenceItem {
			kind: "artifact_ref".to_string(),
			value: artifact.uri.clone(),
		});
		self.save_result(aggregation_result.clone())?;
		self.attach_artifact_to_experiment(&task.task_id, artifact.artifact_id.clone())?;
		self.metrics.inc_artifacts();
		task.last_result = Some(aggregation_result);
		self.mark_node_completed(task, node);
		self.append_node_event(
			task,
			node,
			TaskEventKind::NodeCompleted,
			"aggregation completed",
		)?;
		Ok(())
	}

	fn finalize_task(
		&self,
		task: &mut Task,
		request_id: roku_common_types::RequestId,
	) -> Result<ResponseEnvelope, RuntimeError> {
		let results = self.list_results(&task.task_id)?;
		let completion = assess_graph_completion(task, &results)?;
		if !completion.completed {
			return Err(RuntimeError::new(completion.reason));
		}
		if let Some(final_node_id) = &completion.final_node_id {
			task.last_result = results
				.iter()
				.find(|result| result.node_id == *final_node_id)
				.cloned();
		}
		if task.state != TaskState::Aggregating {
			self.record_transition(task, TaskState::Aggregating, "aggregate")?;
		}
		self.record_transition(task, TaskState::Succeeded, "done")?;
		self.complete_experiment_run(task, results.len())?;
		let artifacts = self
			.list_artifacts(&task.task_id)?
			.into_iter()
			.map(|artifact| artifact.uri)
			.collect();
		self.save_task(task.clone())?;
		self.clear_memory_context(&task.task_id);

		Ok(ResponseEnvelope {
			request_id,
			status: ResponseStatus::Succeeded,
			message: completion
				.final_message
				.unwrap_or_else(|| "task succeeded".to_string()),
			artifacts,
		})
	}

	fn normalize_task_for_resume(
		&self,
		task: &mut Task,
		target_state: TaskState,
	) -> Result<(), RuntimeError> {
		for next_state in resume_transition_path(task.state, target_state)? {
			self.record_transition(task, next_state, "resume")?;
		}
		Ok(())
	}

	fn plan_compensation_records(&self, task: &Task) -> Vec<CompensationRecord> {
		let node_by_id = task
			.graph
			.as_ref()
			.map(|graph| {
				graph
					.nodes
					.iter()
					.map(|node| (node.node_id.clone(), node.kind))
					.collect::<HashMap<_, _>>()
			})
			.unwrap_or_default();

		task.completed_nodes
			.iter()
			.map(|node_id| {
				let action = match node_by_id.get(node_id) {
					Some(TaskNodeKind::Execution) => CompensationAction::AuditOnly,
					_ => CompensationAction::Noop,
				};
				CompensationRecord {
					node_id: node_id.clone(),
					action,
					status: CompensationStatus::Pending,
					note: "cancellation compensation recorded".to_string(),
				}
			})
			.collect()
	}

	fn complete_compensation_records(&self, task: &mut Task, actor: &str) {
		for record in &mut task.compensation_records {
			record.status = CompensationStatus::Completed;
			record.note = format!("{} by {actor}", compensation_note(record.action));
		}
	}

	fn cancel_pending_approval_ticket(
		&self,
		approval_id: &roku_common_types::ApprovalId,
		actor: &str,
	) -> Result<(), RuntimeError> {
		let mut ticket = self
			.get_approval(approval_id)?
			.ok_or_else(|| RuntimeError::new("approval ticket not found for cancellation"))?;
		if ticket.status == ApprovalStatus::Pending {
			ticket.status = ApprovalStatus::Cancelled;
			ticket.decided_by = Some(actor.to_string());
			ticket.comment = Some("task cancelled before approval resolved".to_string());
			self.save_approval_ticket(ticket)?;
		}
		Ok(())
	}

	fn freeze_pending_execution_approval(
		&self,
		task: &mut Task,
		node: &TaskNode,
		fact: PendingExecutionApprovalFact,
		frozen_payload: &str,
		frozen_payload_schema_version: &str,
	) -> Result<ResponseEnvelope, RuntimeError> {
		let approval_id = ApprovalId(compact_approval_id(&task.task_id.0, &node.node_id.0));
		let digest = fact.canonical_execution.digest.clone();
		let frozen_payload_ref = self.persist_frozen_execution_snapshot_artifact(
			task,
			node,
			&digest,
			frozen_payload_schema_version,
			frozen_payload,
		)?;
		let ticket_summary = execution_ticket_summary(&fact.canonical_execution, &node.description);
		let ticket = ApprovalTicket {
			approval_id: approval_id.clone(),
			task_id: task.task_id.clone(),
			request_id: task.request_id.clone(),
			node_id: node.node_id.clone(),
			summary: ticket_summary.clone(),
			status: ApprovalStatus::Pending,
			decided_by: None,
			comment: None,
			pending_execution: Some(PendingExecutionApproval {
				approval_id: approval_id.clone(),
				digest: digest.clone(),
				canonical_execution: fact.canonical_execution,
				policy_decision: fact.policy_decision,
				execution_ref: Some(ApprovedExecutionRef {
					digest,
					frozen_payload_ref: Some(frozen_payload_ref),
				}),
			}),
		};

		self.record_transition(task, TaskState::WaitingApproval, "approval required")?;
		task.pending_approval_id = Some(approval_id.clone());
		self.append_node_event(
			task,
			node,
			TaskEventKind::ApprovalPending,
			"approval required",
		)?;
		self.metrics.inc_approvals_created();
		self.save_approval_ticket(ticket)?;
		self.save_task(task.clone())?;

		Ok(ResponseEnvelope {
			request_id: task.request_id.clone(),
			status: ResponseStatus::PendingApproval,
			message: pending_approval_message_from_summary(&ticket_summary),
			artifacts: vec![approval_artifact(&approval_id)],
		})
	}

	pub(super) fn validated_execution_resume(
		&self,
		task: &Task,
		ticket: &ApprovalTicket,
	) -> Result<Option<ValidatedExecutionResume>, RuntimeError> {
		let Some(pending_execution) = ticket.pending_execution.clone() else {
			return Ok(None);
		};

		let graph = task
			.graph
			.as_ref()
			.ok_or_else(|| RuntimeError::new("execution approval ticket requires a task graph"))?;
		let node = graph
			.nodes
			.iter()
			.find(|node| node.node_id == ticket.node_id)
			.cloned()
			.ok_or_else(|| {
				RuntimeError::new("execution approval ticket points to a missing graph node")
			})?;
		if node.kind != TaskNodeKind::Execution {
			return Err(RuntimeError::new(
				"execution approval ticket must target an execution node",
			));
		}
		if task
			.completed_nodes
			.iter()
			.any(|completed| completed == &node.node_id)
		{
			return Err(RuntimeError::new(
				"execution approval ticket cannot resume an already completed node",
			));
		}
		if pending_execution.policy_decision.outcome != PolicyOutcome::RequireApproval {
			return Err(RuntimeError::new(
				"execution approval ticket must carry a require_approval policy decision",
			));
		}
		if pending_execution.digest != pending_execution.canonical_execution.digest {
			return Err(RuntimeError::new(
				"execution approval ticket digest does not match the frozen canonical execution",
			));
		}
		if pending_execution.canonical_execution.tool_name != "command.run" {
			return Err(RuntimeError::new(
				"execution approval resume currently supports only command.run",
			));
		}
		if pending_execution.canonical_execution.invocation_mode != InvocationMode::DirectExec
			|| pending_execution
				.canonical_execution
				.shell_context
				.is_some()
		{
			return Err(RuntimeError::new(
				"execution approval resume requires a direct-exec frozen command payload",
			));
		}
		if pending_execution.canonical_execution.env_policy.mode
			!= ExecutionEnvPolicyMode::InheritSelected
		{
			return Err(RuntimeError::new(
				"execution approval resume requires an inherit_selected env policy",
			));
		}

		let execution_ref = pending_execution.execution_ref.as_ref().ok_or_else(|| {
			RuntimeError::new("execution approval ticket is missing its frozen execution reference")
		})?;
		if execution_ref.digest != pending_execution.digest {
			return Err(RuntimeError::new(
				"execution approval ticket reference digest does not match the frozen canonical execution",
			));
		}
		let frozen_payload_ref = execution_ref.frozen_payload_ref.clone().ok_or_else(|| {
			RuntimeError::new("execution approval ticket is missing its frozen payload reference")
		})?;
		let frozen_payload = self.load_frozen_execution_payload(&frozen_payload_ref)?;
		validate_frozen_execution_payload(&frozen_payload, &pending_execution)?;

		Ok(Some(ValidatedExecutionResume {
			node,
			pending_execution,
			frozen_payload_ref,
		}))
	}

	fn persist_frozen_execution_snapshot_artifact(
		&self,
		task: &Task,
		node: &TaskNode,
		digest: &roku_common_types::CanonicalDigest,
		schema_version: &str,
		frozen_payload: &str,
	) -> Result<String, RuntimeError> {
		let artifact = {
			let mut state = self.lock_state()?;
			state
				.artifact_store
				.persist_frozen_execution_snapshot_artifact(
					&task.task_id,
					&node.node_id,
					digest,
					schema_version,
					frozen_payload,
				)
				.map_err(|error| RuntimeError::new(error.to_string()))?
		};
		if self.get_experiment_run(&task.task_id)?.is_some() {
			self.attach_artifact_to_experiment(&task.task_id, artifact.artifact_id.clone())?;
		}
		self.metrics.inc_artifacts();
		Ok(artifact.uri)
	}

	fn load_frozen_execution_payload(
		&self,
		frozen_payload_ref: &str,
	) -> Result<String, RuntimeError> {
		let state = self.lock_state()?;
		state
			.artifact_store
			.load_content_by_uri(frozen_payload_ref)
			.map_err(|error| RuntimeError::new(error.to_string()))?
			.ok_or_else(|| RuntimeError::new("frozen execution payload is missing"))
	}

	pub(super) fn resume_approved_execution_ticket(
		&self,
		task: &mut Task,
		ticket: &ApprovalTicket,
		resume: ValidatedExecutionResume,
	) -> Result<ResponseEnvelope, RuntimeError> {
		self.append_node_event(
			task,
			&resume.node,
			TaskEventKind::ApprovalApproved,
			"approval granted",
		)?;
		self.record_transition(task, TaskState::Executing, "approval granted")?;
		self.save_task(task.clone())?;

		let mut result =
			execute_frozen_command_result(task, &resume.node, &resume.pending_execution);
		let artifact = self.persist_result_artifact(&result)?;
		result.evidence.push(EvidenceItem {
			kind: "artifact_ref".to_string(),
			value: artifact.uri.clone(),
		});
		result.evidence.push(EvidenceItem {
			kind: "frozen_payload_ref".to_string(),
			value: resume.frozen_payload_ref,
		});
		self.save_result(result.clone())?;
		self.attach_artifact_to_experiment(&task.task_id, artifact.artifact_id.clone())?;
		self.metrics.inc_artifacts();

		if matches!(result.status, ResultStatus::Error) {
			self.metrics.inc_failures();
			let reason = result_message(&result);
			let terminal_state = self.fail_task(task, &reason, ErrorClass::Dependency)?;
			self.fail_experiment_run(task, &reason)?;
			self.save_task(task.clone())?;

			return Ok(ResponseEnvelope {
				request_id: ticket.request_id.clone(),
				status: ResponseStatus::Failed,
				message: failure_message(&reason, terminal_state),
				artifacts: vec![artifact.uri],
			});
		}

		task.last_result = Some(result);
		self.mark_node_completed(task, &resume.node);
		self.append_node_event(
			task,
			&resume.node,
			TaskEventKind::NodeCompleted,
			"execution resumed from approved frozen payload",
		)?;
		self.process_task(task, RunMode::Normal)
	}
}

fn classify_resume_state(ready_nodes: &[TaskNode]) -> TaskState {
	if ready_nodes
		.iter()
		.any(|node| matches!(node.kind, TaskNodeKind::Aggregation))
	{
		TaskState::Validating
	} else {
		TaskState::Executing
	}
}

fn issue_node_capability_tokens(
	capability_auth: &mut roku_capability_auth::CapabilityAuthority,
	subject: &str,
	node: &TaskNode,
	catalog: &roku_plugin_catalog::ResourceCatalog,
	allow_invoke: bool,
) -> Vec<CapabilityToken> {
	let mut tokens = Vec::new();
	if node.resources.is_empty() && node.capabilities.is_empty() {
		return tokens;
	}

	let actions = if allow_invoke {
		vec!["invoke".to_string()]
	} else {
		vec!["read".to_string()]
	};

	if node.resources.is_empty() {
		tokens.push(
			capability_auth.issue(roku_capability_auth::CapabilityRequest {
				subject: subject.to_string(),
				resource: roku_common_types::ResourceSelector::tool("internal.execution"),
				actions,
				granted_capabilities: if allow_invoke {
					node.capabilities.clone()
				} else {
					Vec::new()
				},
				expires_at_unix: 999_999,
			}),
		);
		return tokens;
	}

	for resource in &node.resources {
		let granted_capabilities = catalog
			.descriptor(resource)
			.map(|descriptor| descriptor.required_capabilities.clone())
			.unwrap_or_default();
		tokens.push(
			capability_auth.issue(roku_capability_auth::CapabilityRequest {
				subject: subject.to_string(),
				resource: resource.clone(),
				actions: actions.clone(),
				granted_capabilities: if allow_invoke {
					granted_capabilities
				} else {
					Vec::new()
				},
				expires_at_unix: 999_999,
			}),
		);
	}

	tokens
}

fn verify_node_capabilities(
	capability_auth: &mut roku_capability_auth::CapabilityAuthority,
	node: &TaskNode,
	tokens: &[CapabilityToken],
) -> bool {
	node.capabilities.iter().all(|capability| {
		tokens
			.iter()
			.any(|token| capability_auth.verify(token, "invoke", Some(capability), 100))
	})
}

fn flatten_granted_capabilities(tokens: &[CapabilityToken]) -> Vec<String> {
	let mut capabilities = Vec::new();
	for token in tokens {
		for capability in &token.granted_capabilities {
			if !capabilities.contains(capability) {
				capabilities.push(capability.clone());
			}
		}
	}
	capabilities
}

fn node_resource_label(node: &TaskNode) -> String {
	if node.resources.is_empty() {
		"internal.execution".to_string()
	} else {
		node.resources
			.iter()
			.map(|resource| resource.display_key())
			.collect::<Vec<_>>()
			.join(",")
	}
}

fn resume_transition_path(
	current_state: TaskState,
	target_state: TaskState,
) -> Result<Vec<TaskState>, RuntimeError> {
	use TaskState::{
		Aggregating, Delegating, Executing, Failed, GraphBuilding, Planning, TimeoutRecovering,
		Validating,
	};

	let path = match (current_state, target_state) {
		(state, target) if state == target => Vec::new(),
		(Failed, Executing) => vec![Planning, GraphBuilding, Delegating, Executing],
		(Failed, Validating) => vec![Planning, GraphBuilding, Delegating, Executing, Validating],
		(Planning, Executing) => vec![GraphBuilding, Delegating, Executing],
		(Planning, Validating) => vec![GraphBuilding, Delegating, Executing, Validating],
		(GraphBuilding, Executing) => vec![Delegating, Executing],
		(GraphBuilding, Validating) => vec![Delegating, Executing, Validating],
		(Delegating, Executing) => vec![Executing],
		(Delegating, Validating) => vec![Executing, Validating],
		(Executing, Validating) => vec![Validating],
		(Validating, Executing) => vec![Executing],
		(TimeoutRecovering, Executing) => vec![Planning, GraphBuilding, Delegating, Executing],
		(TimeoutRecovering, Validating) => {
			vec![Planning, GraphBuilding, Delegating, Executing, Validating]
		}
		(Aggregating, Aggregating) => Vec::new(),
		_ => {
			return Err(RuntimeError::new(format!(
				"task cannot be resumed from state {:?} toward {:?}",
				current_state, target_state
			)));
		}
	};

	Ok(path)
}

fn compensation_note(action: CompensationAction) -> &'static str {
	match action {
		CompensationAction::Noop => "noop compensation recorded",
		CompensationAction::AuditOnly => "audit-only compensation recorded",
	}
}

fn node_time_budget_limit_ms(
	spec: &roku_common_types::AgentInstanceSpec,
	node: &TaskNode,
) -> Option<u64> {
	let mut limit_ms = spec.policy_bindings.time_budget_ms;
	if node.budget_snapshot.time_budget_ms > 0 {
		limit_ms = limit_ms.min(node.budget_snapshot.time_budget_ms);
	}
	if node.deadline_ms > 0 {
		limit_ms = limit_ms.min(node.deadline_ms);
	}
	(limit_ms > 0).then_some(limit_ms)
}

fn result_elapsed_ms(result: &ResultEnvelope) -> Option<u64> {
	let payload = serde_json::from_str::<serde_json::Value>(&result.payload).ok()?;
	payload.get("elapsed_ms")?.as_u64()
}

fn node_budget_timeout_result(
	spec: &roku_common_types::AgentInstanceSpec,
	node: &TaskNode,
	elapsed_ms: u64,
	limit_ms: u64,
) -> ResultEnvelope {
	ResultEnvelope {
		task_id: spec.context.task_id.clone(),
		node_id: node.node_id.clone(),
		producer: spec.instance_id.clone(),
		schema_version: "result.v1".to_string(),
		status: ResultStatus::Error,
		payload: serde_json::json!({
			"error_code": "node_deadline_exceeded",
			"message": format!(
				"node exceeded time budget: elapsed {elapsed_ms}ms > limit {limit_ms}ms"
			),
			"node_id": node.node_id.0,
			"elapsed_ms": elapsed_ms,
			"time_budget_ms": limit_ms,
		})
		.to_string(),
		evidence: vec![
			EvidenceItem {
				kind: "policy".to_string(),
				value: "deadline-exceeded".to_string(),
			},
			EvidenceItem {
				kind: "elapsed_ms".to_string(),
				value: elapsed_ms.to_string(),
			},
		],
		confidence: 0.0,
	}
}

fn pending_execution_approval_fact(
	result: &ResultEnvelope,
) -> Option<PendingExecutionApprovalFact> {
	let payload = serde_json::from_str::<serde_json::Value>(&result.payload).ok()?;
	let policy_decision: PolicyDecision =
		serde_json::from_value(payload.get("policy_decision")?.clone()).ok()?;
	if policy_decision.outcome != PolicyOutcome::RequireApproval {
		return None;
	}
	let canonical_execution: CanonicalExecution =
		serde_json::from_value(payload.get("canonical_execution")?.clone()).ok()?;
	if payload
		.get("digest")
		.and_then(serde_json::Value::as_str)
		.is_some_and(|digest| digest != canonical_execution.digest.0)
	{
		return None;
	}
	Some(PendingExecutionApprovalFact {
		policy_decision,
		canonical_execution,
	})
}

fn validate_frozen_execution_payload(
	frozen_payload: &str,
	pending_execution: &PendingExecutionApproval,
) -> Result<(), RuntimeError> {
	let payload = serde_json::from_str::<Value>(frozen_payload).map_err(|error| {
		RuntimeError::new(format!(
			"failed to decode frozen execution payload: {error}"
		))
	})?;
	let payload_digest = payload
		.get("digest")
		.and_then(Value::as_str)
		.ok_or_else(|| RuntimeError::new("frozen execution payload is missing its digest"))?;
	if payload_digest != pending_execution.digest.0 {
		return Err(RuntimeError::new(
			"frozen execution payload digest does not match the approved ticket",
		));
	}
	let payload_policy_decision: PolicyDecision =
		serde_json::from_value(payload.get("policy_decision").cloned().ok_or_else(|| {
			RuntimeError::new("frozen execution payload is missing its policy_decision")
		})?)
		.map_err(|error| {
			RuntimeError::new(format!(
				"failed to decode frozen execution payload policy_decision: {error}"
			))
		})?;
	if payload_policy_decision != pending_execution.policy_decision {
		return Err(RuntimeError::new(
			"frozen execution payload policy decision does not match the approved ticket",
		));
	}
	let payload_execution: CanonicalExecution =
		serde_json::from_value(payload.get("canonical_execution").cloned().ok_or_else(|| {
			RuntimeError::new("frozen execution payload is missing its canonical_execution")
		})?)
		.map_err(|error| {
			RuntimeError::new(format!(
				"failed to decode frozen execution payload canonical_execution: {error}"
			))
		})?;
	if payload_execution != pending_execution.canonical_execution {
		return Err(RuntimeError::new(
			"frozen execution payload canonical execution does not match the approved ticket",
		));
	}
	Ok(())
}

fn execute_frozen_command_result(
	task: &Task,
	node: &TaskNode,
	pending_execution: &PendingExecutionApproval,
) -> ResultEnvelope {
	let execution = &pending_execution.canonical_execution;
	let started_at = Instant::now();
	let mut command = Command::new(&execution.program);
	command
		.args(execution.argv.iter().skip(1))
		.current_dir(&execution.cwd)
		.stdin(Stdio::null())
		.stdout(Stdio::piped())
		.stderr(Stdio::piped());

	if execution
		.env_policy
		.allowed_keys
		.iter()
		.any(|key| key == "ROKU_COMMAND_SCOPE_ROOT")
	{
		let scope_root = execution
			.resource_scope
			.effective_read_roots
			.first()
			.cloned()
			.unwrap_or_else(|| execution.cwd.clone());
		command.env("ROKU_COMMAND_SCOPE_ROOT", scope_root);
	}
	if execution
		.env_policy
		.allowed_keys
		.iter()
		.any(|key| key == "ROKU_ALLOWED_READ_ROOTS")
	{
		command.env(
			"ROKU_ALLOWED_READ_ROOTS",
			serde_json::to_string(&execution.resource_scope.effective_read_roots)
				.unwrap_or_else(|_| "[]".to_string()),
		);
	}

	let output = match command.output() {
		Ok(output) => output,
		Err(error) => {
			return resumed_execution_failure_result(
				task,
				node,
				execution,
				format!(
					"failed to spawn approved frozen command `{}`: {error}",
					execution.program
				),
			);
		}
	};
	let elapsed_ms = started_at.elapsed().as_millis();
	let stdout = String::from_utf8_lossy(&output.stdout).to_string();
	let stderr = String::from_utf8_lossy(&output.stderr).to_string();
	let scope_root = execution
		.resource_scope
		.effective_read_roots
		.first()
		.cloned()
		.unwrap_or_else(|| execution.cwd.clone());
	let observed_output = observed_frozen_command_output(
		execution,
		&scope_root,
		output.status.code(),
		stdout,
		stderr,
		false,
	);

	ResultEnvelope {
		task_id: task.task_id.clone(),
		node_id: node.node_id.clone(),
		producer: format!("approval-resume:{}", node.node_id.0),
		schema_version: "result.v1".to_string(),
		status: ResultStatus::Ok,
		payload: json!({
			"worker_id": "approval-resume",
			"tool_name": execution.tool_name,
			"message": observed_output
				.get("message")
				.and_then(Value::as_str)
				.unwrap_or(&node.description),
			"node_id": node.node_id.0,
			"summary": node.description,
			"attempts": 1,
			"elapsed_ms": elapsed_ms,
			"output": observed_output,
		})
		.to_string(),
		evidence: vec![
			EvidenceItem {
				kind: "runtime".to_string(),
				value: "approval-resume".to_string(),
			},
			EvidenceItem {
				kind: "tool".to_string(),
				value: execution.tool_name.clone(),
			},
			EvidenceItem {
				kind: "execution_digest".to_string(),
				value: execution.digest.0.clone(),
			},
		],
		confidence: 0.9,
	}
}

fn resumed_execution_failure_result(
	task: &Task,
	node: &TaskNode,
	execution: &CanonicalExecution,
	message: String,
) -> ResultEnvelope {
	let command_preview = projected_execution_preview(execution)
		.map(|preview| preview.command_text)
		.unwrap_or_else(|| execution.program.clone());
	ResultEnvelope {
		task_id: task.task_id.clone(),
		node_id: node.node_id.clone(),
		producer: format!("approval-resume:{}", node.node_id.0),
		schema_version: "result.v1".to_string(),
		status: ResultStatus::Error,
		payload: json!({
			"error_code": "approved_execution_resume_failed",
			"message": format!("{message} [command={command_preview}]"),
			"tool_name": execution.tool_name,
			"node_id": node.node_id.0,
			"digest": execution.digest.0,
			"canonical_execution": execution,
		})
		.to_string(),
		evidence: vec![
			EvidenceItem {
				kind: "runtime".to_string(),
				value: "approval-resume".to_string(),
			},
			EvidenceItem {
				kind: "tool".to_string(),
				value: execution.tool_name.clone(),
			},
			EvidenceItem {
				kind: "execution_digest".to_string(),
				value: execution.digest.0.clone(),
			},
		],
		confidence: 0.0,
	}
}

fn observed_frozen_command_output(
	execution: &CanonicalExecution,
	scope_root: &str,
	exit_code: Option<i32>,
	stdout: String,
	stderr: String,
	truncated: bool,
) -> Value {
	let command_text = projected_execution_preview(execution)
		.map(|preview| preview.command_text)
		.unwrap_or_else(|| execution.program.clone());
	let ok = exit_code == Some(0);
	let message = if ok {
		let visible = stdout.trim();
		if visible.is_empty() {
			format!("`{command_text}` finished successfully with no stdout.")
		} else {
			visible.to_string()
		}
	} else if !stderr.trim().is_empty() {
		stderr.trim().to_string()
	} else {
		format!(
			"`{command_text}` exited with status {}.",
			exit_code.unwrap_or(-1)
		)
	};
	ToolOutputEnvelope::new(
		ok,
		(!ok).then_some("non_zero_exit"),
		false,
		message,
		json!({
			"command": command_text,
			"argv": execution.argv,
			"program": execution.program,
			"cwd": execution.cwd,
			"scope_root": scope_root,
			"exit_code": exit_code,
			"stdout": stdout,
			"stderr": stderr,
			"truncated": truncated,
			"digest": execution.digest.0,
		}),
	)
	.into_value()
}

pub(crate) fn pending_approval_message(ticket: &ApprovalTicket) -> String {
	pending_approval_message_from_summary(&approval_ticket_summary(ticket))
}

fn pending_approval_message_from_summary(summary: &str) -> String {
	format!("approval required: {summary}")
}

fn approval_ticket_summary(ticket: &ApprovalTicket) -> String {
	ticket
		.pending_execution
		.as_ref()
		.and_then(|pending_execution| {
			projected_execution_preview(&pending_execution.canonical_execution)
		})
		.map(|preview| preview.summary)
		.unwrap_or_else(|| ticket.summary.clone())
}

fn execution_ticket_summary(execution: &CanonicalExecution, fallback: &str) -> String {
	projected_execution_preview(execution)
		.map(|preview| preview.summary)
		.unwrap_or_else(|| fallback.to_string())
}

fn projected_execution_preview(execution: &CanonicalExecution) -> Option<ExecutionPreview> {
	project_execution_preview(execution)
}

fn classify_result_error(result: &ResultEnvelope) -> ErrorClass {
	let payload = serde_json::from_str::<serde_json::Value>(&result.payload).ok();
	let error_code = payload
		.as_ref()
		.and_then(|value| value.get("error_code"))
		.and_then(serde_json::Value::as_str);

	if result
		.evidence
		.iter()
		.any(|item| item.kind == "policy" && item.value == "budget-exhausted")
		|| matches!(error_code, Some("policy_bindings_rejected"))
	{
		ErrorClass::BudgetExhausted
	} else if result
		.evidence
		.iter()
		.any(|item| item.kind == "tool_error" && item.value == "timeout")
		|| matches!(error_code, Some("timeout" | "node_deadline_exceeded"))
	{
		ErrorClass::Timeout
	} else {
		ErrorClass::Dependency
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use roku_common_types::{
		ApprovalRequirement, ApprovalRequirementScope, CanonicalDigest, ExecutionActionClass,
		ExecutionEnvPolicy, ExecutionEnvPolicyMode, ExecutionResourceScope, InvocationMode, NodeId,
		PolicyReasonCode, ResponseStatus, TaskId, TaskNodeKind, TaskState,
	};

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

	fn sample_canonical_execution() -> CanonicalExecution {
		CanonicalExecution {
			tool_name: "command.run".to_string(),
			program: "rm".to_string(),
			argv: vec!["rm".to_string(), "-rf".to_string(), "tmp".to_string()],
			invocation_mode: InvocationMode::DirectExec,
			shell_context: None,
			cwd: "/workspace".to_string(),
			env_policy: ExecutionEnvPolicy {
				mode: ExecutionEnvPolicyMode::InheritSelected,
				allowed_keys: vec!["ROKU_COMMAND_SCOPE_ROOT".to_string()],
			},
			resource_scope: ExecutionResourceScope {
				working_directory: "/workspace".to_string(),
				resolved_targets: vec!["/workspace/tmp".to_string()],
				effective_read_roots: vec!["/workspace".to_string()],
				effective_write_roots: Vec::new(),
			},
			action_class: ExecutionActionClass::Exec,
			digest: CanonicalDigest("digest-123".to_string()),
		}
	}

	#[test]
	fn pending_execution_approval_fact_reads_structured_upstream_payload() {
		let result = ResultEnvelope {
			task_id: TaskId("task-1".to_string()),
			node_id: NodeId("node-1".to_string()),
			producer: "worker-1".to_string(),
			schema_version: "result.v1".to_string(),
			status: ResultStatus::Error,
			payload: serde_json::json!({
				"error_code": "approval_required",
				"message": "approval required",
				"policy_decision": {
					"outcome": "require_approval",
					"reason_code": "approval_required_by_untrusted_program",
					"approval_requirement": {
						"scope": "invocation",
						"reason_code": "approval_required_by_untrusted_program"
					}
				},
				"canonical_execution": {
					"tool_name": "command.run",
					"program": "rm",
					"argv": ["rm", "-rf", "tmp"],
					"invocation_mode": "direct_exec",
					"shell_context": null,
					"cwd": "/workspace",
					"env_policy": {
						"mode": "inherit_selected",
						"allowed_keys": ["ROKU_COMMAND_SCOPE_ROOT"]
					},
					"resource_scope": {
						"working_directory": "/workspace",
						"resolved_targets": ["/workspace/tmp"],
						"effective_read_roots": ["/workspace"],
						"effective_write_roots": []
					},
					"action_class": "exec",
					"digest": "digest-123"
				},
				"digest": "digest-123"
			})
			.to_string(),
			evidence: Vec::new(),
			confidence: 0.0,
		};

		let fact = pending_execution_approval_fact(&result)
			.expect("approval fact should round-trip from result payload");

		assert_eq!(fact.policy_decision, sample_policy_decision());
		assert_eq!(fact.canonical_execution, sample_canonical_execution());
	}

	#[test]
	fn pending_execution_approval_fact_ignores_non_require_approval_outcomes() {
		let result = ResultEnvelope {
			task_id: TaskId("task-1".to_string()),
			node_id: NodeId("node-1".to_string()),
			producer: "worker-1".to_string(),
			schema_version: "result.v1".to_string(),
			status: ResultStatus::Error,
			payload: serde_json::json!({
				"error_code": "policy_denied",
				"message": "policy denied",
				"policy_decision": {
					"outcome": "deny",
					"reason_code": "denied_by_shell_syntax",
					"approval_requirement": null
				},
				"canonical_execution": sample_canonical_execution(),
				"digest": "digest-123"
			})
			.to_string(),
			evidence: Vec::new(),
			confidence: 0.0,
		};

		assert!(pending_execution_approval_fact(&result).is_none());
	}

	#[test]
	fn freeze_pending_execution_approval_persists_ticket_and_returns_pending_response() {
		let service = RuntimeService::default();
		let mut task = Task {
			task_id: TaskId("task-1".to_string()),
			request_id: roku_common_types::RequestId("req-1".to_string()),
			session_id: "session-1".to_string(),
			goal: "remove tmp".to_string(),
			state: TaskState::Executing,
			attempts: 0,
			planning_mode_hint: None,
			conversation_history: Vec::new(),
			completed_nodes: Vec::new(),
			next_node_index: 0,
			pending_approval_id: None,
			last_result: None,
			compensation_records: Vec::new(),
			graph: None,
		};
		let node = TaskNode {
			node_id: NodeId("node-1".to_string()),
			kind: TaskNodeKind::Execution,
			description: "review risky command".to_string(),
			..TaskNode::default()
		};
		let expected_digest = CanonicalDigest("digest-123".to_string());
		let frozen_payload = serde_json::json!({
			"error_code": "approval_required",
			"message": "approval required",
			"policy_decision": sample_policy_decision(),
			"canonical_execution": sample_canonical_execution(),
			"digest": expected_digest.0.clone(),
		})
		.to_string();

		let response = service
			.freeze_pending_execution_approval(
				&mut task,
				&node,
				PendingExecutionApprovalFact {
					policy_decision: sample_policy_decision(),
					canonical_execution: sample_canonical_execution(),
				},
				&frozen_payload,
				"result.v1",
			)
			.expect("approval freeze should succeed");

		assert_eq!(response.status, ResponseStatus::PendingApproval);
		assert_eq!(
			response.message,
			"approval required: Run command rm -rf tmp from /workspace"
		);
		assert_eq!(task.state, TaskState::WaitingApproval);
		let approval_id = task
			.pending_approval_id
			.clone()
			.expect("approval id should be persisted onto task");
		assert_eq!(response.artifacts, vec![approval_artifact(&approval_id)]);

		let persisted_task = service
			.get_task(&task.task_id)
			.expect("task lookup should succeed")
			.expect("task should persist");
		assert_eq!(persisted_task.state, TaskState::WaitingApproval);
		assert_eq!(
			persisted_task.pending_approval_id,
			Some(approval_id.clone())
		);

		let ticket = service
			.get_approval(&approval_id)
			.expect("ticket lookup should succeed")
			.expect("approval ticket should persist");
		assert_eq!(
			ticket.summary,
			roku_common_types::project_execution_preview(&sample_canonical_execution())
				.expect("preview should project")
				.summary
		);
		let pending = ticket
			.pending_execution
			.expect("pending execution payload should be frozen");
		assert_eq!(pending.approval_id, approval_id);
		assert_eq!(pending.digest, expected_digest);
		assert_eq!(pending.canonical_execution, sample_canonical_execution());
		assert_eq!(pending.policy_decision, sample_policy_decision());
		let frozen_payload_ref = pending
			.execution_ref
			.as_ref()
			.and_then(|execution_ref| execution_ref.frozen_payload_ref.clone())
			.expect("frozen execution snapshot ref should persist");
		assert_eq!(
			frozen_payload_ref,
			"artifact://snapshots/task-1/node-1/digest-123"
		);
		assert_eq!(
			pending.execution_ref,
			Some(ApprovedExecutionRef {
				digest: expected_digest,
				frozen_payload_ref: Some(frozen_payload_ref.clone()),
			})
		);
		let persisted_frozen_payload = service
			.lock_state()
			.expect("runtime state lock should succeed")
			.artifact_store
			.load_content_by_uri(&frozen_payload_ref)
			.expect("frozen execution snapshot should load")
			.expect("frozen execution snapshot content should exist");
		assert_eq!(persisted_frozen_payload, frozen_payload);
	}
}
