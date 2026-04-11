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

use std::process::{Command, Stdio};
use std::time::Instant;

use roku_common_types::{
	ApprovalId, ApprovalStatus, ApprovalTicket, ApprovedExecutionRef, CanonicalExecution,
	CompensationAction, CompensationRecord, CompensationStatus, ErrorClass, EvidenceItem,
	ExecutionEnvPolicyMode, ExecutionPreview, InvocationMode, NodeBudgetSnapshot,
	PendingExecutionApproval, PolicyDecision, PolicyOutcome, PolicyReasonCode, ResourceSelector,
	ResponseEnvelope, ResponseStatus, ResultEnvelope, ResultStatus, RuntimeError, Task,
	TaskEventKind, TaskId, TaskNode, TaskNodeKind, TaskState, ToolOutputEnvelope,
	project_execution_preview,
};
use serde_json::{Value, json};

use super::helpers::{approval_artifact, failure_message, result_message};
use super::{RuntimeService, compact_approval_id};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PendingExecutionApprovalFact {
	policy_decision: PolicyDecision,
	canonical_execution: CanonicalExecution,
}

#[derive(Debug, Clone)]
pub(super) struct ValidatedExecutionResume {
	node: TaskNode,
	pending_execution: PendingExecutionApproval,
	frozen_payload_ref: String,
	frozen_tool_input: Value,
}

impl RuntimeService {
	pub fn resume_task(&self, task_id: &TaskId) -> Result<ResponseEnvelope, RuntimeError> {
		let task = self
			.get_task(task_id)?
			.ok_or_else(|| RuntimeError::new(format!("task not found: {}", task_id.0)))?;
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
				return Err(RuntimeError::new(
					"graphless direct runtime tasks must finish through their original runtime loop, not task replay",
				));
			}
			_ => {}
		}

		Err(RuntimeError::new(
			"direct runtime tasks resume through session-owned pending-loop intake or approval decisions, not task replay",
		))
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
		let disposition = super::state_machine::Orchestrator::default().request_cancellation(
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
		let disposition = super::state_machine::Orchestrator::default().register_failure(
			task,
			reason,
			Some(error_class),
		)?;
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

	fn plan_compensation_records(&self, task: &Task) -> Vec<CompensationRecord> {
		task.completed_nodes
			.iter()
			.map(|node_id| CompensationRecord {
				node_id: node_id.clone(),
				action: CompensationAction::Noop,
				status: CompensationStatus::Pending,
				note: "cancellation compensation recorded".to_string(),
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

	pub(super) fn freeze_pending_execution_approval(
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
		self.save_approval_ticket(ticket.clone())?;
		self.save_task(task.clone())?;

		Ok(ResponseEnvelope {
			request_id: task.request_id.clone(),
			status: ResponseStatus::PendingApproval,
			message: pending_approval_message(&ticket),
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

		let node = validated_execution_resume_node(ticket, &pending_execution)?;
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
		let approved_tool_name = pending_execution.canonical_execution.tool_name.as_str();
		if approved_tool_name == "Bash" {
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
		} else if !matches!(
			approved_tool_name,
			// Current PascalCase names.
			"Read" | "Write" | "Edit" | "Grep" | "Glob" | "Find" | "Exists" | "Inspect" | "ListDir"
			// Legacy dotted names for in-flight approval tickets created before the rename.
			| "fs.read_text" | "fs.write" | "fs.edit" | "fs.grep" | "fs.glob" | "fs.find"
			| "fs.exists" | "fs.inspect" | "fs.list_dir"
		) {
			return Err(RuntimeError::new(format!(
				"execution approval resume does not support `{approved_tool_name}`",
			)));
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
		let frozen_tool_input =
			validate_frozen_execution_payload(&frozen_payload, &pending_execution)?;

		Ok(Some(ValidatedExecutionResume {
			node,
			pending_execution,
			frozen_payload_ref,
			frozen_tool_input,
		}))
	}

	fn persist_frozen_execution_snapshot_artifact(
		&self,
		task: &Task,
		node: &TaskNode,
		digest: &roku_common_types::CanonicalDigest,
		_schema_version: &str,
		frozen_payload: &str,
	) -> Result<String, RuntimeError> {
		let uri = format!(
			"artifact://snapshots/{}/{}/{}",
			task.task_id.0, node.node_id.0, digest.0
		);
		let mut state = self.lock_state()?;
		state
			.frozen_payloads
			.insert(uri.clone(), frozen_payload.to_string());
		self.metrics.inc_artifacts();
		Ok(uri)
	}

	fn load_frozen_execution_payload(
		&self,
		frozen_payload_ref: &str,
	) -> Result<String, RuntimeError> {
		let state = self.lock_state()?;
		state
			.frozen_payloads
			.get(frozen_payload_ref)
			.cloned()
			.ok_or_else(|| RuntimeError::new("frozen execution payload not found"))
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

		// Use the normalized name for dispatch so legacy dotted names resolve to the
		// registered PascalCase tools.
		let dispatch_tool_name = match resume
			.pending_execution
			.canonical_execution
			.tool_name
			.as_str()
		{
			"fs.read_text" => "Read",
			"fs.write" => "Write",
			"fs.edit" => "Edit",
			"fs.grep" => "Grep",
			"fs.glob" => "Glob",
			"fs.find" => "Find",
			"fs.exists" => "Exists",
			"fs.inspect" => "Inspect",
			"fs.list_dir" => "ListDir",
			"command.run" => "Bash",
			other => other,
		};
		// Sync canonical_execution.tool_name with the normalized dispatch name
		// so ToolRuntime::invoke doesn't reject the mismatch.
		let mut canonical = resume.pending_execution.canonical_execution.clone();
		if canonical.tool_name != dispatch_tool_name {
			canonical.tool_name = dispatch_tool_name.to_string();
		}
		let mut result = if dispatch_tool_name == "Bash" {
			execute_frozen_command_result(task, &resume.node, &resume.pending_execution)
		} else {
			self.runtime.execute_approved_tool_invocation(
				task,
				&resume.node,
				dispatch_tool_name,
				resume.frozen_tool_input.clone(),
				canonical,
			)
		};
		result.evidence.push(EvidenceItem {
			kind: "frozen_payload_ref".to_string(),
			value: resume.frozen_payload_ref,
		});
		self.save_result(result.clone())?;

		if matches!(result.status, ResultStatus::Error) {
			self.metrics.inc_failures();
			let reason = result_message(&result);
			let terminal_state = self.fail_task(task, &reason, ErrorClass::Dependency)?;
			self.save_task(task.clone())?;

			return Ok(ResponseEnvelope {
				request_id: ticket.request_id.clone(),
				status: ResponseStatus::Failed,
				message: failure_message(&reason, terminal_state),
				artifacts: Vec::new(),
			});
		}

		let dummy_artifact = roku_common_types::Artifact {
			artifact_id: roku_common_types::ArtifactId("none".to_string()),
			task_id: task.task_id.clone(),
			node_id: resume.node.node_id.clone(),
			uri: String::new(),
			kind: "result".to_string(),
			schema_version: "result.v1".to_string(),
			checksum: String::new(),
			metadata: Vec::new(),
		};
		let message = result_message(&result);
		let response = self.complete_direct_runtime_path(
			task,
			&resume.node,
			result,
			message,
			dummy_artifact,
			"execution resumed from approved frozen payload",
		)?;
		self.clear_runtime_memory_layers(&task.task_id);
		Ok(response)
	}
}

fn compensation_note(action: CompensationAction) -> &'static str {
	match action {
		CompensationAction::Noop => "noop compensation recorded",
		CompensationAction::AuditOnly => "audit-only compensation recorded",
	}
}

pub(super) fn pending_execution_approval_fact(
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

fn validated_execution_resume_node(
	ticket: &ApprovalTicket,
	pending_execution: &PendingExecutionApproval,
) -> Result<TaskNode, RuntimeError> {
	Ok(TaskNode {
		node_id: ticket.node_id.clone(),
		kind: TaskNodeKind::Execution,
		description: ticket.summary.clone(),
		resources: vec![ResourceSelector::tool(
			pending_execution.canonical_execution.tool_name.clone(),
		)],
		budget_snapshot: NodeBudgetSnapshot {
			token_budget: 8_000,
			time_budget_ms: 60_000,
		},
		..TaskNode::default()
	})
}

fn validate_frozen_execution_payload(
	frozen_payload: &str,
	pending_execution: &PendingExecutionApproval,
) -> Result<Value, RuntimeError> {
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
	let payload_tool_name = payload
		.get("tool_name")
		.and_then(Value::as_str)
		.ok_or_else(|| RuntimeError::new("frozen execution payload is missing its tool_name"))?;
	if payload_tool_name != pending_execution.canonical_execution.tool_name {
		return Err(RuntimeError::new(
			"frozen execution payload tool_name does not match the approved ticket",
		));
	}
	payload
		.get("tool_input")
		.cloned()
		.ok_or_else(|| RuntimeError::new("frozen execution payload is missing its tool_input"))
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
	if let Some(pending_execution) = ticket.pending_execution.as_ref() {
		return execution_approval_message(pending_execution);
	}
	pending_approval_message_from_summary(&approval_ticket_summary(ticket))
}

fn pending_approval_message_from_summary(summary: &str) -> String {
	format!("🛡️ Approval Request\n\nAction: {summary}")
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

fn execution_approval_message(pending_execution: &PendingExecutionApproval) -> String {
	let execution = &pending_execution.canonical_execution;
	let lines = [
		"🛡️ Approval Request".to_string(),
		String::new(),
		format!("Tool: {}", execution.tool_name),
		format!("Action: {}", approval_action_text(execution)),
		format!(
			"Risk: {}",
			approval_risk_label(&pending_execution.policy_decision)
		),
		format!(
			"Reason: {}",
			approval_reason_text(&pending_execution.policy_decision)
		),
	];
	lines.join("\n")
}

fn approval_action_text(execution: &CanonicalExecution) -> String {
	if execution.tool_name == "Bash" {
		if let Some(preview) = projected_execution_preview(execution) {
			return preview.summary;
		}
		return format!("Run command {} from {}", execution.program, execution.cwd);
	}

	let target = execution
		.resource_scope
		.resolved_targets
		.first()
		.cloned()
		.unwrap_or_else(|| execution.cwd.clone());
	match execution.tool_name.as_str() {
		"Inspect" => format!("Inspect path {target}"),
		"ListDir" => format!("List directory {target}"),
		"Read" => format!("Read text from {target}"),
		"Exists" => format!("Check whether {target} exists"),
		other => format!("Execute {other} against {target}"),
	}
}

fn approval_risk_label(decision: &PolicyDecision) -> &'static str {
	match decision.reason_code {
		PolicyReasonCode::ApprovalRequiredByOutOfScopePath
		| PolicyReasonCode::ApprovalRequiredByWriteScope
		| PolicyReasonCode::ApprovalRequiredByNetwork => "high",
		PolicyReasonCode::ApprovalRequiredByUntrustedProgram => "medium",
		_ => "medium",
	}
}

fn approval_reason_text(decision: &PolicyDecision) -> &'static str {
	match decision.reason_code {
		PolicyReasonCode::ApprovalRequiredByOutOfScopePath => {
			"the requested path is outside the current allowed workspace roots"
		}
		PolicyReasonCode::ApprovalRequiredByWriteScope => {
			"the action may modify the filesystem and needs explicit confirmation"
		}
		PolicyReasonCode::ApprovalRequiredByNetwork => {
			"the action may reach the network and needs explicit confirmation"
		}
		PolicyReasonCode::ApprovalRequiredByUntrustedProgram => {
			"the command is outside the constrained built-in allowlist"
		}
		PolicyReasonCode::AllowedByPolicy => "the current execution policy requires confirmation",
		PolicyReasonCode::DeniedByShellSyntax => {
			"shell-wrapped syntax is not eligible for approval"
		}
		PolicyReasonCode::DeniedByCommandPolicy => "the command policy rejected the request",
		PolicyReasonCode::DeniedByOutOfScopeCwd => {
			"the working directory is outside the allowed workspace roots"
		}
		PolicyReasonCode::DeniedByOutOfScopeTarget => {
			"the requested target is outside the allowed workspace roots"
		}
		PolicyReasonCode::DeniedByUncanonicalizableInput => {
			"the runtime could not freeze a canonical execution payload"
		}
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

	fn sample_canonical_execution() -> CanonicalExecution {
		CanonicalExecution {
			tool_name: "Bash".to_string(),
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

	fn sample_fs_execution() -> CanonicalExecution {
		CanonicalExecution {
			tool_name: "ListDir".to_string(),
			program: "ListDir".to_string(),
			argv: vec!["ListDir".to_string(), "/Users/jojo".to_string()],
			invocation_mode: InvocationMode::DirectExec,
			shell_context: None,
			cwd: "/workspace".to_string(),
			env_policy: ExecutionEnvPolicy {
				mode: ExecutionEnvPolicyMode::Clean,
				allowed_keys: Vec::new(),
			},
			resource_scope: ExecutionResourceScope {
				working_directory: "/workspace".to_string(),
				resolved_targets: vec!["/Users/jojo".to_string()],
				effective_read_roots: vec!["/workspace".to_string()],
				effective_write_roots: Vec::new(),
			},
			action_class: ExecutionActionClass::Read,
			digest: CanonicalDigest("digest-fs-123".to_string()),
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
					"tool_name": "Bash",
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
			conversation_history: Vec::new(),
			completed_nodes: Vec::new(),
			next_node_index: 0,
			pending_approval_id: None,
			last_result: None,
			compensation_records: Vec::new(),
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
			"tool_name": "Bash",
			"tool_input": {
				"command": "rm -rf tmp",
				"cwd": "/workspace"
			},
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
			"🛡️ Approval Request\n\nTool: Bash\nAction: Run command rm -rf tmp from /workspace\nRisk: medium\nReason: the command is outside the constrained built-in allowlist"
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
		assert!(
			frozen_payload_ref.starts_with("artifact://snapshots/"),
			"frozen payload ref should use artifact URI scheme: {frozen_payload_ref}"
		);
		assert_eq!(
			pending.execution_ref,
			Some(ApprovedExecutionRef {
				digest: expected_digest,
				frozen_payload_ref: Some(frozen_payload_ref.clone()),
			})
		);
	}

	#[test]
	fn pending_approval_message_describes_filesystem_path_approval() {
		let approval_id = roku_common_types::ApprovalId("approval-fs-1".to_string());
		let ticket = ApprovalTicket {
			approval_id: approval_id.clone(),
			task_id: TaskId("task-fs-1".to_string()),
			request_id: roku_common_types::RequestId("req-fs-1".to_string()),
			node_id: NodeId("node-fs-1".to_string()),
			summary: "list home directory".to_string(),
			status: roku_common_types::ApprovalStatus::Pending,
			decided_by: None,
			comment: None,
			pending_execution: Some(PendingExecutionApproval {
				approval_id,
				digest: CanonicalDigest("digest-fs-123".to_string()),
				canonical_execution: sample_fs_execution(),
				policy_decision: sample_fs_policy_decision(),
				execution_ref: None,
			}),
		};

		assert_eq!(
			pending_approval_message(&ticket),
			"🛡️ Approval Request\n\nTool: ListDir\nAction: List directory /Users/jojo\nRisk: high\nReason: the requested path is outside the current allowed workspace roots"
		);
	}
}
