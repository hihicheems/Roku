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
	TaskEvent, TaskId, TaskState,
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
			planning_mode_hint: request.planning_mode_hint,
			conversation_history: request.conversation_history.clone(),
			completed_nodes: Vec::new(),
			next_node_index: 0,
			pending_approval_id: None,
			last_result: None,
			compensation_records: Vec::new(),
			graph: None,
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
		.last()
		.map(|event| event.to)
		.unwrap_or(persisted_state)
}

pub fn replay_consistency_status(
	persisted_state: TaskState,
	events: &[TaskEvent],
) -> ReplayConsistencyStatus {
	let transitions_valid = events
		.iter()
		.all(|event| is_valid_transition(event.from, event.to));
	if !transitions_valid {
		return ReplayConsistencyStatus::InvalidTransitions;
	}

	let chain_consistent = events
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

#[cfg(test)]
mod tests {
	use super::*;
	use roku_common_types::{RequestEnvelope, RequestId};

	#[test]
	fn transition_rules_are_enforced() {
		let orchestrator = Orchestrator::default();
		let request = RequestEnvelope {
			request_id: RequestId("req-1".to_string()),
			session_id: "s1".to_string(),
			goal: "g".to_string(),
			planning_mode_hint: None,
			conversation_history: Vec::new(),
		};
		let mut task = orchestrator.create_task(&request);

		let ok = orchestrator.transition(&mut task, TaskState::Planning, "plan", None);
		assert!(ok.is_ok());

		let bad = orchestrator.transition(&mut task, TaskState::Succeeded, "skip", None);
		assert!(bad.is_err());
	}

	#[test]
	fn mark_dead_letter_after_max_attempts() {
		let orchestrator = Orchestrator::with_config(OrchestratorConfig { max_attempts: 2 });
		let request = RequestEnvelope {
			request_id: RequestId("req-1".to_string()),
			session_id: "s1".to_string(),
			goal: "g".to_string(),
			planning_mode_hint: None,
			conversation_history: Vec::new(),
		};
		let mut task = orchestrator.create_task(&request);

		let first = orchestrator.mark_failed_or_dead_letter(&mut task);
		assert_eq!(first, TaskState::Failed);

		let second = orchestrator.mark_failed_or_dead_letter(&mut task);
		assert_eq!(second, TaskState::DeadLetter);
	}

	#[test]
	fn register_failure_transitions_to_dead_letter_after_budget_exhaustion() {
		let orchestrator = Orchestrator::with_config(OrchestratorConfig { max_attempts: 2 });
		let request = RequestEnvelope {
			request_id: RequestId("req-1".to_string()),
			session_id: "s1".to_string(),
			goal: "g".to_string(),
			planning_mode_hint: None,
			conversation_history: Vec::new(),
		};
		let mut task = orchestrator.create_task(&request);
		orchestrator
			.transition(&mut task, TaskState::Planning, "plan", None)
			.expect("planning transition should succeed");

		let first = orchestrator
			.register_failure(&mut task, "failed once", Some(ErrorClass::Validation))
			.expect("failure registration should succeed");
		assert_eq!(first.terminal_state, TaskState::Failed);
		assert_eq!(first.events.len(), 1);

		orchestrator
			.transition(&mut task, TaskState::Planning, "retry", None)
			.expect("retry transition should succeed");
		let second = orchestrator
			.register_failure(&mut task, "failed twice", Some(ErrorClass::Validation))
			.expect("dead-letter registration should succeed");
		assert_eq!(second.terminal_state, TaskState::DeadLetter);
		assert_eq!(second.events.len(), 2);
	}

	#[test]
	fn replay_consistency_detects_snapshot_mismatch() {
		let events = vec![TaskEvent {
			task_id: TaskId("task-1".to_string()),
			from: TaskState::Queued,
			to: TaskState::Planning,
			reason: "start".to_string(),
			error_class: None,
		}];

		assert_eq!(
			replay_consistency_status(TaskState::Queued, &events),
			ReplayConsistencyStatus::SnapshotMismatch
		);
	}

	#[test]
	fn recovery_eligibility_maps_waiting_approval() {
		assert_eq!(
			recovery_eligibility_for_state(TaskState::WaitingApproval),
			RecoveryEligibility::PendingApproval
		);
	}

	#[test]
	fn request_cancellation_transitions_through_compensation_when_work_exists() {
		let orchestrator = Orchestrator::default();
		let request = RequestEnvelope {
			request_id: RequestId("req-1".to_string()),
			session_id: "s1".to_string(),
			goal: "g".to_string(),
			planning_mode_hint: None,
			conversation_history: Vec::new(),
		};
		let mut task = orchestrator.create_task(&request);
		orchestrator
			.transition(&mut task, TaskState::Planning, "plan", None)
			.expect("planning transition should succeed");

		let events = orchestrator
			.request_cancellation(&mut task, "operator cancelled", true)
			.expect("cancellation should succeed");

		assert_eq!(events.len(), 3);
		assert_eq!(events[0].to, TaskState::CancelRequested);
		assert_eq!(events[1].to, TaskState::Compensating);
		assert_eq!(events[2].to, TaskState::Cancelled);
		assert_eq!(task.state, TaskState::Cancelled);
	}

	#[test]
	fn timeout_recovery_state_is_resumable() {
		assert_eq!(
			recovery_eligibility_for_state(TaskState::TimeoutRecovering),
			RecoveryEligibility::ResumeReady
		);
	}
}
