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

use crate::state_machine::{
	replay_consistency_status, replay_consistency_status_from, replayed_state, replayed_state_from,
};
use roku_common_types::{
	ApprovalTicket, ErrorClass, NodeId, RecoveryEligibility, ReplayConsistencyStatus,
	ResultEnvelope, ResumeCandidate, RuntimeError, Task, TaskEvent, TaskEventKind, TaskId,
	TaskNode, TaskReplayCursor, TaskReplayReport, TaskReplaySnapshot, TaskState,
};

use crate::RuntimeService;

impl RuntimeService {
	pub fn get_task(&self, task_id: &TaskId) -> Result<Option<Task>, RuntimeError> {
		let state = self.lock_state()?;
		state
			.task_repo
			.load_task(task_id)
			.map_err(|error| RuntimeError::new(error.to_string()))
	}

	pub fn list_task_events(&self, task_id: &TaskId) -> Result<Vec<TaskEvent>, RuntimeError> {
		let state = self.lock_state()?;
		state
			.event_repo
			.list_events(task_id)
			.map_err(|error| RuntimeError::new(error.to_string()))
	}

	pub fn compact_task_replay(
		&self,
		task_id: &TaskId,
		retain_events: usize,
	) -> Result<(), RuntimeError> {
		let task = self
			.get_task(task_id)?
			.ok_or_else(|| RuntimeError::new(format!("task not found: {}", task_id.0)))?;
		let analysis = self.analyze_task_recovery(&task)?;
		let snapshot = TaskReplaySnapshot {
			task_id: task_id.clone(),
			compacted_event_count: 0,
			replayed_state: analysis.replayed_state,
			completed_nodes: analysis.reconstructed_task.completed_nodes.clone(),
			pending_approval_id: analysis.reconstructed_task.pending_approval_id.clone(),
			last_result: analysis.reconstructed_task.last_result.clone(),
		};
		let mut state = self.lock_state()?;
		state
			.event_repo
			.compact_task_events(snapshot, retain_events)
			.map_err(|error| RuntimeError::new(error.to_string()))
	}

	pub fn get_task_replay_report(
		&self,
		task_id: &TaskId,
	) -> Result<Option<TaskReplayReport>, RuntimeError> {
		let Some(task) = self.get_task(task_id)? else {
			return Ok(None);
		};
		let analysis = self.analyze_task_recovery(&task)?;
		Ok(Some(build_replay_report(task, analysis)))
	}

	#[allow(dead_code)]
	pub(super) fn list_approval_tickets_for_task(
		&self,
		task_id: &TaskId,
	) -> Result<Vec<ApprovalTicket>, RuntimeError> {
		let state = self.lock_state()?;
		state
			.approval_repo
			.list_tickets_for_task(task_id)
			.map_err(|error| RuntimeError::new(error.to_string()))
	}

	pub(super) fn record_transition(
		&self,
		task: &mut Task,
		next: TaskState,
		reason: &str,
	) -> Result<(), RuntimeError> {
		self.record_transition_with_error_class(task, next, reason, None)
	}

	pub(super) fn record_transition_with_error_class(
		&self,
		task: &mut Task,
		next: TaskState,
		reason: &str,
		error_class: Option<ErrorClass>,
	) -> Result<(), RuntimeError> {
		let event = self
			.orchestrator
			.transition(task, next, reason, error_class)?;
		let mut state = self.lock_state()?;
		state
			.event_repo
			.append_event(event)
			.map_err(|error| RuntimeError::new(error.to_string()))
	}

	pub(super) fn save_task(&self, task: Task) -> Result<(), RuntimeError> {
		let mut state = self.lock_state()?;
		state
			.task_repo
			.save_task(task)
			.map_err(|error| RuntimeError::new(error.to_string()))
	}

	pub(super) fn save_approval_ticket(&self, ticket: ApprovalTicket) -> Result<(), RuntimeError> {
		let mut state = self.lock_state()?;
		state
			.approval_repo
			.save_ticket(ticket)
			.map_err(|error| RuntimeError::new(error.to_string()))
	}

	pub(super) fn save_result(&self, result: ResultEnvelope) -> Result<(), RuntimeError> {
		let mut state = self.lock_state()?;
		state
			.result_repo
			.save_result(result)
			.map_err(|error| RuntimeError::new(error.to_string()))
	}

	pub(super) fn append_node_event(
		&self,
		task: &Task,
		node: &TaskNode,
		kind: TaskEventKind,
		reason: impl Into<String>,
	) -> Result<(), RuntimeError> {
		let mut state = self.lock_state()?;
		state
			.event_repo
			.append_event(TaskEvent {
				task_id: task.task_id.clone(),
				from: task.state,
				to: task.state,
				reason: reason.into(),
				error_class: None,
				kind,
				node_id: Some(node.node_id.clone()),
				node_kind: Some(node.kind),
				attempt: Some(task.attempts),
			})
			.map_err(|error| RuntimeError::new(error.to_string()))
	}

	#[allow(dead_code)]
	pub(super) fn list_results(
		&self,
		task_id: &TaskId,
	) -> Result<Vec<ResultEnvelope>, RuntimeError> {
		let state = self.lock_state()?;
		state
			.result_repo
			.list_results(task_id)
			.map_err(|error| RuntimeError::new(error.to_string()))
	}

	pub(super) fn mark_node_completed(&self, task: &mut Task, node: &TaskNode) {
		self.mark_node_completed_by_id(task, &node.node_id);
	}

	pub(super) fn mark_node_completed_by_id(&self, task: &mut Task, node_id: &NodeId) {
		if task
			.completed_nodes
			.iter()
			.all(|completed| completed != node_id)
		{
			task.completed_nodes.push(node_id.clone());
		}
		task.next_node_index = task.completed_nodes.len();
	}

	pub(super) fn lock_state(
		&self,
	) -> Result<std::sync::MutexGuard<'_, crate::RuntimeState>, RuntimeError> {
		self.state
			.lock()
			.map_err(|_| RuntimeError::new("runtime state lock poisoned"))
	}

	pub(super) fn analyze_task_recovery(
		&self,
		task: &Task,
	) -> Result<TaskRecoveryAnalysis, RuntimeError> {
		let replay_snapshot = self.load_replay_snapshot(&task.task_id)?;
		let events = self.list_task_events(&task.task_id)?;
		let total_event_count = replay_snapshot.as_ref().map_or(events.len(), |snapshot| {
			snapshot.compacted_event_count.saturating_add(events.len())
		});
		let (replayed_state, consistency_status) = if let Some(snapshot) = &replay_snapshot {
			(
				replayed_state_from(snapshot.replayed_state, &events),
				replay_consistency_status_from(snapshot.replayed_state, task.state, &events),
			)
		} else {
			(
				replayed_state(task.state, &events),
				replay_consistency_status(task.state, &events),
			)
		};
		let transitions_valid = !matches!(
			consistency_status,
			ReplayConsistencyStatus::InvalidTransitions
		);
		let chain_consistent = !matches!(
			consistency_status,
			ReplayConsistencyStatus::BrokenTransitionChain
		);
		let snapshot_matches_replay = !matches!(
			consistency_status,
			ReplayConsistencyStatus::SnapshotMismatch
		);

		let resume_candidates: Vec<ResumeCandidate> = Vec::new();
		let recovery_eligibility = if task.state == TaskState::WaitingApproval {
			RecoveryEligibility::PendingApproval
		} else {
			RecoveryEligibility::NotRecoverable
		};

		Ok(TaskRecoveryAnalysis {
			reconstructed_task: task.clone(),
			events,
			total_event_count,
			replayed_state,
			transitions_valid,
			chain_consistent,
			snapshot_matches_replay,
			consistency_status,
			recovery_eligibility,
			resume_candidates,
		})
	}

	fn load_replay_snapshot(
		&self,
		task_id: &TaskId,
	) -> Result<Option<TaskReplaySnapshot>, RuntimeError> {
		let state = self.lock_state()?;
		state
			.event_repo
			.load_replay_snapshot(task_id)
			.map_err(|error| RuntimeError::new(error.to_string()))
	}
}

fn build_replay_report(task: Task, analysis: TaskRecoveryAnalysis) -> TaskReplayReport {
	TaskReplayReport {
		task_id: task.task_id,
		persisted_state: task.state,
		replayed_state: analysis.replayed_state,
		event_count: analysis.total_event_count,
		transitions_valid: analysis.transitions_valid,
		chain_consistent: analysis.chain_consistent,
		snapshot_matches_replay: analysis.snapshot_matches_replay,
		recoverable: !matches!(
			analysis.recovery_eligibility,
			RecoveryEligibility::Blocked | RecoveryEligibility::NotRecoverable
		),
		replay_cursor: TaskReplayCursor {
			replayed_state: analysis.replayed_state,
			event_count: analysis.total_event_count,
		},
		consistency_status: analysis.consistency_status,
		recovery_eligibility: analysis.recovery_eligibility,
		resume_candidates: analysis.resume_candidates,
		events: analysis.events,
	}
}

pub(super) struct TaskRecoveryAnalysis {
	pub reconstructed_task: Task,
	pub events: Vec<TaskEvent>,
	pub total_event_count: usize,
	pub replayed_state: TaskState,
	pub transitions_valid: bool,
	pub chain_consistent: bool,
	pub snapshot_matches_replay: bool,
	pub consistency_status: ReplayConsistencyStatus,
	pub recovery_eligibility: RecoveryEligibility,
	pub resume_candidates: Vec<ResumeCandidate>,
}
