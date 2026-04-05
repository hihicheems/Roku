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

//! Provider-neutral control-plane persistence contracts.
//!
//! `roku-memory` is the long-term Roku-owned home for control-plane semantics.
//! Concrete providers such as SQLite stay in adapter crates; they implement the
//! contracts here, but they do not redefine them.

mod dispatch;

use std::collections::HashMap;

use roku_common_types::{
	ApprovalId, ApprovalTicket, NodeId, ResultEnvelope, Task, TaskEvent, TaskId, TaskReplaySnapshot,
};
use thiserror::Error;

pub use dispatch::{
	BackpressureSnapshot, DispatchClaim, DispatchEnvelope, DispatchLease, DispatchQueue,
	InMemoryDispatchQueue, RetryClaim,
};

#[derive(Debug, Error)]
pub enum ControlPlaneError {
	#[error("control-plane backend failed: {0}")]
	Backend(String),
}

pub trait TaskRepository {
	fn save_task(&mut self, task: Task) -> Result<(), ControlPlaneError>;
	fn load_task(&self, task_id: &TaskId) -> Result<Option<Task>, ControlPlaneError>;
}

pub trait EventRepository {
	fn append_event(&mut self, event: TaskEvent) -> Result<(), ControlPlaneError>;
	fn list_events(&self, task_id: &TaskId) -> Result<Vec<TaskEvent>, ControlPlaneError>;
	fn load_replay_snapshot(
		&self,
		_task_id: &TaskId,
	) -> Result<Option<TaskReplaySnapshot>, ControlPlaneError> {
		Ok(None)
	}
	fn compact_task_events(
		&mut self,
		_snapshot: TaskReplaySnapshot,
		_retain_events: usize,
	) -> Result<(), ControlPlaneError> {
		Ok(())
	}
}

pub trait ApprovalRepository {
	fn save_ticket(&mut self, ticket: ApprovalTicket) -> Result<(), ControlPlaneError>;
	fn load_ticket(
		&self,
		approval_id: &ApprovalId,
	) -> Result<Option<ApprovalTicket>, ControlPlaneError>;
	fn list_tickets_for_task(
		&self,
		task_id: &TaskId,
	) -> Result<Vec<ApprovalTicket>, ControlPlaneError>;
}

pub trait ResultRepository {
	fn save_result(&mut self, result: ResultEnvelope) -> Result<(), ControlPlaneError>;
	fn load_result(
		&self,
		task_id: &TaskId,
		node_id: &NodeId,
	) -> Result<Option<ResultEnvelope>, ControlPlaneError>;
	fn list_results(&self, task_id: &TaskId) -> Result<Vec<ResultEnvelope>, ControlPlaneError>;
}

/// Provider-neutral control-plane bundle consumed by runtime-service.
pub struct ControlPlaneDataPlane {
	pub task_repo: Box<dyn TaskRepository + Send>,
	pub event_repo: Box<dyn EventRepository + Send>,
	pub approval_repo: Box<dyn ApprovalRepository + Send>,
	pub result_repo: Box<dyn ResultRepository + Send>,
}

impl ControlPlaneDataPlane {
	pub fn in_memory() -> Self {
		Self {
			task_repo: Box::new(InMemoryTaskRepository::default()),
			event_repo: Box::new(InMemoryEventRepository::default()),
			approval_repo: Box::new(InMemoryApprovalRepository::default()),
			result_repo: Box::new(InMemoryResultRepository::default()),
		}
	}
}

#[derive(Debug, Default)]
pub struct InMemoryTaskRepository {
	tasks: HashMap<String, Task>,
}

impl TaskRepository for InMemoryTaskRepository {
	fn save_task(&mut self, task: Task) -> Result<(), ControlPlaneError> {
		self.tasks.insert(task.task_id.0.clone(), task);
		Ok(())
	}

	fn load_task(&self, task_id: &TaskId) -> Result<Option<Task>, ControlPlaneError> {
		Ok(self.tasks.get(&task_id.0).cloned())
	}
}

#[derive(Debug, Default)]
pub struct InMemoryEventRepository {
	events: Vec<TaskEvent>,
}

impl EventRepository for InMemoryEventRepository {
	fn append_event(&mut self, event: TaskEvent) -> Result<(), ControlPlaneError> {
		self.events.push(event);
		Ok(())
	}

	fn list_events(&self, task_id: &TaskId) -> Result<Vec<TaskEvent>, ControlPlaneError> {
		Ok(self
			.events
			.iter()
			.filter(|event| event.task_id.0 == task_id.0)
			.cloned()
			.collect())
	}
}

#[derive(Debug, Default)]
pub struct InMemoryApprovalRepository {
	tickets: HashMap<String, ApprovalTicket>,
}

impl ApprovalRepository for InMemoryApprovalRepository {
	fn save_ticket(&mut self, ticket: ApprovalTicket) -> Result<(), ControlPlaneError> {
		self.tickets.insert(ticket.approval_id.0.clone(), ticket);
		Ok(())
	}

	fn load_ticket(
		&self,
		approval_id: &ApprovalId,
	) -> Result<Option<ApprovalTicket>, ControlPlaneError> {
		Ok(self.tickets.get(&approval_id.0).cloned())
	}

	fn list_tickets_for_task(
		&self,
		task_id: &TaskId,
	) -> Result<Vec<ApprovalTicket>, ControlPlaneError> {
		Ok(self
			.tickets
			.values()
			.filter(|ticket| ticket.task_id == *task_id)
			.cloned()
			.collect())
	}
}

#[derive(Debug, Default)]
pub struct InMemoryResultRepository {
	results: HashMap<String, ResultEnvelope>,
}

impl ResultRepository for InMemoryResultRepository {
	fn save_result(&mut self, result: ResultEnvelope) -> Result<(), ControlPlaneError> {
		self.results
			.insert(result_key(&result.task_id, &result.node_id), result);
		Ok(())
	}

	fn load_result(
		&self,
		task_id: &TaskId,
		node_id: &NodeId,
	) -> Result<Option<ResultEnvelope>, ControlPlaneError> {
		Ok(self.results.get(&result_key(task_id, node_id)).cloned())
	}

	fn list_results(&self, task_id: &TaskId) -> Result<Vec<ResultEnvelope>, ControlPlaneError> {
		Ok(self
			.results
			.values()
			.filter(|result| result.task_id == *task_id)
			.cloned()
			.collect())
	}
}

fn result_key(task_id: &TaskId, node_id: &NodeId) -> String {
	format!("{}:{}", task_id.0, node_id.0)
}

#[cfg(test)]
mod tests {
	use roku_common_types::{
		ApprovalStatus, EvidenceItem, RequestId, ResultStatus, TaskReplaySnapshot, TaskState,
	};

	use super::*;

	fn sample_task() -> Task {
		Task {
			task_id: TaskId("task-1".to_string()),
			request_id: RequestId("req-1".to_string()),
			session_id: "session-1".to_string(),
			goal: "analyze market".to_string(),
			state: TaskState::Queued,
			attempts: 0,
			conversation_history: Vec::new(),
			completed_nodes: Vec::new(),
			next_node_index: 0,
			pending_approval_id: None,
			last_result: None,
			compensation_records: Vec::new(),
		}
	}

	fn sample_event() -> TaskEvent {
		TaskEvent {
			task_id: TaskId("task-1".to_string()),
			from: TaskState::Queued,
			to: TaskState::Planning,
			reason: "start".to_string(),
			error_class: None,
			kind: roku_common_types::TaskEventKind::StateTransition,
			node_id: None,
			node_kind: None,
			attempt: Some(0),
		}
	}

	fn sample_ticket() -> ApprovalTicket {
		ApprovalTicket {
			approval_id: ApprovalId("approval-1".to_string()),
			task_id: TaskId("task-1".to_string()),
			request_id: RequestId("req-1".to_string()),
			node_id: NodeId("node-1".to_string()),
			summary: "review risky action".to_string(),
			status: ApprovalStatus::Pending,
			decided_by: None,
			comment: None,
			pending_execution: None,
		}
	}

	fn sample_result() -> ResultEnvelope {
		ResultEnvelope {
			task_id: TaskId("task-1".to_string()),
			node_id: NodeId("node-1".to_string()),
			producer: "agent-1".to_string(),
			schema_version: "result.v1".to_string(),
			status: ResultStatus::Ok,
			payload: "payload".to_string(),
			evidence: vec![EvidenceItem {
				kind: "artifact_ref".to_string(),
				value: "artifact://1".to_string(),
			}],
			confidence: 0.9,
		}
	}

	#[test]
	fn in_memory_repositories_roundtrip() {
		let mut task_repo = InMemoryTaskRepository::default();
		let mut event_repo = InMemoryEventRepository::default();
		let mut approval_repo = InMemoryApprovalRepository::default();
		let mut result_repo = InMemoryResultRepository::default();

		task_repo.save_task(sample_task()).expect("save task");
		event_repo.append_event(sample_event()).expect("save event");
		approval_repo
			.save_ticket(sample_ticket())
			.expect("save approval ticket");
		result_repo
			.save_result(sample_result())
			.expect("save result");

		assert!(
			task_repo
				.load_task(&TaskId("task-1".to_string()))
				.expect("load task")
				.is_some()
		);
		assert_eq!(
			event_repo
				.list_events(&TaskId("task-1".to_string()))
				.expect("list events")
				.len(),
			1
		);
		assert!(
			approval_repo
				.load_ticket(&ApprovalId("approval-1".to_string()))
				.expect("load ticket")
				.is_some()
		);
		assert_eq!(
			approval_repo
				.list_tickets_for_task(&TaskId("task-1".to_string()))
				.expect("list tickets")
				.len(),
			1
		);
		assert!(
			result_repo
				.load_result(&TaskId("task-1".to_string()), &NodeId("node-1".to_string()))
				.expect("load result")
				.is_some()
		);
	}

	#[test]
	fn event_repository_keeps_default_replay_snapshot_hooks() {
		let mut repo = InMemoryEventRepository::default();
		repo.append_event(sample_event()).expect("event append");
		assert!(
			repo.load_replay_snapshot(&TaskId("task-1".to_string()))
				.expect("snapshot lookup")
				.is_none()
		);
		repo.compact_task_events(
			TaskReplaySnapshot {
				task_id: TaskId("task-1".to_string()),
				compacted_event_count: 0,
				replayed_state: TaskState::Planning,
				completed_nodes: Vec::new(),
				pending_approval_id: None,
				last_result: None,
			},
			1,
		)
		.expect("default compaction should no-op");
	}

	#[test]
	fn control_plane_bundle_defaults_to_in_memory() {
		let mut bundle = ControlPlaneDataPlane::in_memory();
		bundle
			.task_repo
			.save_task(sample_task())
			.expect("bundle task repo should save");
		assert!(
			bundle
				.task_repo
				.load_task(&TaskId("task-1".to_string()))
				.expect("bundle task repo should load")
				.is_some()
		);
	}
}
