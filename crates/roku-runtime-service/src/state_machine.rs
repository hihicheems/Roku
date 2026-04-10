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

//! Task state machine and orchestration primitives.

use roku_common_types::{
	ErrorClass, RecoveryEligibility, ReplayConsistencyStatus, RequestEnvelope, RuntimeError, Task,
	TaskEvent, TaskEventKind, TaskId, TaskState,
};

#[derive(Debug, Clone)]
pub struct OrchestratorConfig {
	pub max_attempts: u32,
}

impl Default for OrchestratorConfig {
	fn default() -> Self {
		Self { max_attempts: 3 }
	}
}

#[derive(Debug, Default)]
pub struct Orchestrator {
	pub config: OrchestratorConfig,
}

#[derive(Debug, Clone)]
pub struct FailureDisposition {
	pub terminal_state: TaskState,
	pub events: Vec<TaskEvent>,
}

impl Orchestrator {
	pub fn with_config(config: OrchestratorConfig) -> Self {
		Self { config }
	}

	pub fn create_task(&self, request: &RequestEnvelope) -> Task {
		Task {
			task_id: TaskId(format!("task-{}", request.request_id.0)),
			request_id: request.request_id.clone(),
			session_id: request.session_id.clone(),
			goal: request.goal.clone(),
			state: TaskState::Queued,
			attempts: 0,
			conversation_history: request.conversation_history.clone(),
			completed_nodes: Vec::new(),
			next_node_index: 0,
			pending_approval_id: None,
			last_result: None,
			compensation_records: Vec::new(),
		}
	}

	pub fn transition(
		&self,
		task: &mut Task,
		next: TaskState,
		reason: impl Into<String>,
		error_class: Option<ErrorClass>,
	) -> Result<TaskEvent, RuntimeError> {
		if !is_valid_transition(task.state, next) {
			return Err(RuntimeError::new(format!(
				"invalid transition: {:?} -> {:?}",
				task.state, next
			)));
		}

		let event = TaskEvent {
			task_id: task.task_id.clone(),
			from: task.state,
			to: next,
			reason: reason.into(),
			error_class,
			kind: TaskEventKind::StateTransition,
			node_id: None,
			node_kind: None,
			attempt: Some(task.attempts),
		};
		task.state = next;
		Ok(event)
	}

	pub fn mark_failed_or_dead_letter(&self, task: &mut Task) -> TaskState {
		task.attempts = task.attempts.saturating_add(1);
		if task.attempts >= self.config.max_attempts {
			task.state = TaskState::DeadLetter;
		} else {
			task.state = TaskState::Failed;
		}
		task.state
	}

	pub fn register_failure(
		&self,
		task: &mut Task,
		reason: impl Into<String>,
		error_class: Option<ErrorClass>,
	) -> Result<FailureDisposition, RuntimeError> {
		let reason = reason.into();
		task.attempts = task.attempts.saturating_add(1);

		let mut events = vec![self.transition(task, TaskState::Failed, reason, error_class)?];
		if task.attempts >= self.config.max_attempts {
			events.push(self.transition(
				task,
				TaskState::DeadLetter,
				"retry budget exhausted",
				Some(ErrorClass::BudgetExhausted),
			)?);
		}

		Ok(FailureDisposition {
			terminal_state: task.state,
			events,
		})
	}

	pub fn request_cancellation(
		&self,
		task: &mut Task,
		reason: impl Into<String>,
		has_compensation_work: bool,
	) -> Result<Vec<TaskEvent>, RuntimeError> {
		let reason = reason.into();
		let mut events = vec![self.transition(task, TaskState::CancelRequested, reason, None)?];

		if has_compensation_work {
			events.push(self.transition(
				task,
				TaskState::Compensating,
				"record cancellation compensation",
				None,
			)?);
			events.push(self.transition(
				task,
				TaskState::Cancelled,
				"cancellation compensation recorded",
				None,
			)?);
		} else {
			events.push(self.transition(
				task,
				TaskState::Cancelled,
				"cancelled before additional work started",
				None,
			)?);
		}

		Ok(events)
	}
}

pub fn build_idempotency_key(task_id: &TaskId, node_id: &str, attempt: u32) -> String {
	format!("{}:{}:{}", task_id.0, node_id, attempt)
}

pub fn is_valid_transition(from: TaskState, to: TaskState) -> bool {
	matches!(
		(from, to),
		(TaskState::Queued, TaskState::Planning)
			| (TaskState::Planning, TaskState::GraphBuilding)
			| (TaskState::GraphBuilding, TaskState::Delegating)
			| (TaskState::Delegating, TaskState::Executing)
			| (TaskState::Executing, TaskState::WaitingApproval)
			| (TaskState::Executing, TaskState::Validating)
			| (TaskState::Validating, TaskState::Executing)
			| (TaskState::Validating, TaskState::Aggregating)
			| (TaskState::Validating, TaskState::WaitingApproval)
			| (TaskState::WaitingApproval, TaskState::Executing)
			| (TaskState::Executing, TaskState::Aggregating)
			| (TaskState::Planning, TaskState::CancelRequested)
			| (TaskState::GraphBuilding, TaskState::CancelRequested)
			| (TaskState::Delegating, TaskState::CancelRequested)
			| (TaskState::Executing, TaskState::CancelRequested)
			| (TaskState::Validating, TaskState::CancelRequested)
			| (TaskState::WaitingApproval, TaskState::CancelRequested)
			| (TaskState::Aggregating, TaskState::CancelRequested)
			| (TaskState::Failed, TaskState::CancelRequested)
			| (TaskState::TimeoutRecovering, TaskState::CancelRequested)
			| (TaskState::CancelRequested, TaskState::Compensating)
			| (TaskState::CancelRequested, TaskState::Cancelled)
			| (TaskState::Compensating, TaskState::Cancelled)
			| (TaskState::Delegating, TaskState::TimeoutRecovering)
			| (TaskState::Executing, TaskState::TimeoutRecovering)
			| (TaskState::Validating, TaskState::TimeoutRecovering)
			| (TaskState::TimeoutRecovering, TaskState::Planning)
			| (TaskState::TimeoutRecovering, TaskState::GraphBuilding)
			| (TaskState::TimeoutRecovering, TaskState::Delegating)
			| (TaskState::TimeoutRecovering, TaskState::Executing)
			| (TaskState::TimeoutRecovering, TaskState::Validating)
			| (TaskState::Aggregating, TaskState::Succeeded)
			| (TaskState::Failed, TaskState::Planning)
			| (_, TaskState::Failed)
			| (TaskState::Failed, TaskState::DeadLetter)
			| (_, TaskState::Cancelled)
	)
}

pub fn replayed_state(persisted_state: TaskState, events: &[TaskEvent]) -> TaskState {
	events
		.iter()
		.rev()
		.find(|event| event.kind == TaskEventKind::StateTransition)
		.map(|event| event.to)
		.unwrap_or(persisted_state)
}

pub fn replayed_state_from(base_state: TaskState, events: &[TaskEvent]) -> TaskState {
	events
		.iter()
		.rev()
		.find(|event| event.kind == TaskEventKind::StateTransition)
		.map(|event| event.to)
		.unwrap_or(base_state)
}

pub fn replay_consistency_status(
	persisted_state: TaskState,
	events: &[TaskEvent],
) -> ReplayConsistencyStatus {
	let transitions_valid = events
		.iter()
		.filter(|event| event.kind == TaskEventKind::StateTransition)
		.all(|event| is_valid_transition(event.from, event.to));
	if !transitions_valid {
		return ReplayConsistencyStatus::InvalidTransitions;
	}

	let chain_consistent = events
		.iter()
		.filter(|event| event.kind == TaskEventKind::StateTransition)
		.cloned()
		.collect::<Vec<_>>()
		.windows(2)
		.all(|window| window[0].to == window[1].from);
	if !chain_consistent {
		return ReplayConsistencyStatus::BrokenTransitionChain;
	}

	if replayed_state(persisted_state, events) != persisted_state {
		return ReplayConsistencyStatus::SnapshotMismatch;
	}

	ReplayConsistencyStatus::Consistent
}

pub fn replay_consistency_status_from(
	base_state: TaskState,
	expected_state: TaskState,
	events: &[TaskEvent],
) -> ReplayConsistencyStatus {
	let state_events = events
		.iter()
		.filter(|event| event.kind == TaskEventKind::StateTransition)
		.cloned()
		.collect::<Vec<_>>();
	let transitions_valid = state_events
		.iter()
		.all(|event| is_valid_transition(event.from, event.to));
	if !transitions_valid {
		return ReplayConsistencyStatus::InvalidTransitions;
	}

	if let Some(first) = state_events.first()
		&& first.from != base_state
	{
		return ReplayConsistencyStatus::BrokenTransitionChain;
	}

	let chain_consistent = state_events
		.windows(2)
		.all(|window| window[0].to == window[1].from);
	if !chain_consistent {
		return ReplayConsistencyStatus::BrokenTransitionChain;
	}

	if replayed_state_from(base_state, events) != expected_state {
		return ReplayConsistencyStatus::SnapshotMismatch;
	}

	ReplayConsistencyStatus::Consistent
}

pub fn recovery_eligibility_for_state(state: TaskState) -> RecoveryEligibility {
	match state {
		TaskState::Planning
		| TaskState::GraphBuilding
		| TaskState::Delegating
		| TaskState::Executing
		| TaskState::Validating
		| TaskState::Failed
		| TaskState::TimeoutRecovering => RecoveryEligibility::ResumeReady,
		TaskState::WaitingApproval => RecoveryEligibility::PendingApproval,
		TaskState::Aggregating => RecoveryEligibility::FinalizeReady,
		TaskState::Queued
		| TaskState::CancelRequested
		| TaskState::Compensating
		| TaskState::Succeeded
		| TaskState::DeadLetter
		| TaskState::Cancelled => RecoveryEligibility::NotRecoverable,
	}
}
