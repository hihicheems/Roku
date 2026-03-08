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

//! Trait-backed state repositories with in-memory and file adapters.

mod dispatch;
mod postgres;

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use roku_common_types::{
	ApprovalId, ApprovalTicket, ConversationTurn, NodeId, ResultEnvelope, SessionPreferences, Task,
	TaskEvent, TaskId,
};
use thiserror::Error;

pub use dispatch::{
	BackpressureSnapshot, DispatchClaim, DispatchEnvelope, DispatchLease, DispatchQueue,
	InMemoryDispatchQueue, RetryClaim,
};
pub use postgres::{
	PostgresApprovalRepository, PostgresConversationRepository, PostgresEventRepository,
	PostgresResultRepository, PostgresSessionPreferenceRepository, PostgresStoreConfig,
	PostgresTaskRepository,
};

#[derive(Debug, Error)]
pub enum StoreError {
	#[error("io error: {0}")]
	Io(#[from] std::io::Error),
	#[error("serialization error: {0}")]
	Serde(#[from] serde_json::Error),
	#[error("postgres error: {0}")]
	Postgres(String),
}

pub trait TaskRepository {
	fn save_task(&mut self, task: Task) -> Result<(), StoreError>;
	fn load_task(&self, task_id: &TaskId) -> Result<Option<Task>, StoreError>;
}

pub trait EventRepository {
	fn append_event(&mut self, event: TaskEvent) -> Result<(), StoreError>;
	fn list_events(&self, task_id: &TaskId) -> Result<Vec<TaskEvent>, StoreError>;
}

pub trait ApprovalRepository {
	fn save_ticket(&mut self, ticket: ApprovalTicket) -> Result<(), StoreError>;
	fn load_ticket(&self, approval_id: &ApprovalId) -> Result<Option<ApprovalTicket>, StoreError>;
}

pub trait ResultRepository {
	fn save_result(&mut self, result: ResultEnvelope) -> Result<(), StoreError>;
	fn load_result(
		&self,
		task_id: &TaskId,
		node_id: &NodeId,
	) -> Result<Option<ResultEnvelope>, StoreError>;
	fn list_results(&self, task_id: &TaskId) -> Result<Vec<ResultEnvelope>, StoreError>;
}

pub trait SessionPreferenceRepository {
	fn save_preferences(
		&mut self,
		session_id: &str,
		preferences: SessionPreferences,
	) -> Result<(), StoreError>;
	fn load_preferences(&self, session_id: &str) -> Result<Option<SessionPreferences>, StoreError>;
}

pub trait ConversationRepository {
	fn append_turn(&mut self, session_id: &str, turn: ConversationTurn) -> Result<(), StoreError>;
	fn load_recent_turns(
		&self,
		session_id: &str,
		limit: usize,
	) -> Result<Vec<ConversationTurn>, StoreError>;
}

#[derive(Debug, Default)]
pub struct InMemoryTaskRepository {
	tasks: HashMap<String, Task>,
}

impl TaskRepository for InMemoryTaskRepository {
	fn save_task(&mut self, task: Task) -> Result<(), StoreError> {
		self.tasks.insert(task.task_id.0.clone(), task);
		Ok(())
	}

	fn load_task(&self, task_id: &TaskId) -> Result<Option<Task>, StoreError> {
		Ok(self.tasks.get(&task_id.0).cloned())
	}
}

#[derive(Debug, Default)]
pub struct InMemoryEventRepository {
	events: Vec<TaskEvent>,
}

impl EventRepository for InMemoryEventRepository {
	fn append_event(&mut self, event: TaskEvent) -> Result<(), StoreError> {
		self.events.push(event);
		Ok(())
	}

	fn list_events(&self, task_id: &TaskId) -> Result<Vec<TaskEvent>, StoreError> {
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
	fn save_ticket(&mut self, ticket: ApprovalTicket) -> Result<(), StoreError> {
		self.tickets.insert(ticket.approval_id.0.clone(), ticket);
		Ok(())
	}

	fn load_ticket(&self, approval_id: &ApprovalId) -> Result<Option<ApprovalTicket>, StoreError> {
		Ok(self.tickets.get(&approval_id.0).cloned())
	}
}

#[derive(Debug, Default)]
pub struct InMemoryResultRepository {
	results: HashMap<String, ResultEnvelope>,
}

impl ResultRepository for InMemoryResultRepository {
	fn save_result(&mut self, result: ResultEnvelope) -> Result<(), StoreError> {
		self.results
			.insert(result_key(&result.task_id, &result.node_id), result);
		Ok(())
	}

	fn load_result(
		&self,
		task_id: &TaskId,
		node_id: &NodeId,
	) -> Result<Option<ResultEnvelope>, StoreError> {
		Ok(self.results.get(&result_key(task_id, node_id)).cloned())
	}

	fn list_results(&self, task_id: &TaskId) -> Result<Vec<ResultEnvelope>, StoreError> {
		Ok(self
			.results
			.values()
			.filter(|result| result.task_id == *task_id)
			.cloned()
			.collect())
	}
}

#[derive(Debug, Default)]
pub struct InMemorySessionPreferenceRepository {
	preferences: HashMap<String, SessionPreferences>,
}

impl SessionPreferenceRepository for InMemorySessionPreferenceRepository {
	fn save_preferences(
		&mut self,
		session_id: &str,
		preferences: SessionPreferences,
	) -> Result<(), StoreError> {
		self.preferences.insert(session_id.to_string(), preferences);
		Ok(())
	}

	fn load_preferences(&self, session_id: &str) -> Result<Option<SessionPreferences>, StoreError> {
		Ok(self.preferences.get(session_id).cloned())
	}
}

#[derive(Debug, Default)]
pub struct InMemoryConversationRepository {
	turns_by_session: HashMap<String, Vec<ConversationTurn>>,
}

impl ConversationRepository for InMemoryConversationRepository {
	fn append_turn(&mut self, session_id: &str, turn: ConversationTurn) -> Result<(), StoreError> {
		self.turns_by_session
			.entry(session_id.to_string())
			.or_default()
			.push(turn);
		Ok(())
	}

	fn load_recent_turns(
		&self,
		session_id: &str,
		limit: usize,
	) -> Result<Vec<ConversationTurn>, StoreError> {
		let Some(turns) = self.turns_by_session.get(session_id) else {
			return Ok(Vec::new());
		};
		let start = turns.len().saturating_sub(limit);
		Ok(turns[start..].to_vec())
	}
}

#[derive(Debug, Clone)]
pub struct FileTaskRepository {
	path: PathBuf,
}

impl FileTaskRepository {
	pub fn new(path: impl Into<PathBuf>) -> Self {
		Self { path: path.into() }
	}

	fn read_all(&self) -> Result<HashMap<String, Task>, StoreError> {
		if !self.path.exists() {
			return Ok(HashMap::new());
		}
		let data = fs::read_to_string(&self.path)?;
		if data.trim().is_empty() {
			return Ok(HashMap::new());
		}
		Ok(serde_json::from_str(&data)?)
	}

	fn write_all(&self, tasks: &HashMap<String, Task>) -> Result<(), StoreError> {
		ensure_parent_dir(&self.path)?;
		let encoded = serde_json::to_string_pretty(tasks)?;
		fs::write(&self.path, encoded)?;
		Ok(())
	}
}

impl TaskRepository for FileTaskRepository {
	fn save_task(&mut self, task: Task) -> Result<(), StoreError> {
		let mut tasks = self.read_all()?;
		tasks.insert(task.task_id.0.clone(), task);
		self.write_all(&tasks)
	}

	fn load_task(&self, task_id: &TaskId) -> Result<Option<Task>, StoreError> {
		let tasks = self.read_all()?;
		Ok(tasks.get(&task_id.0).cloned())
	}
}

#[derive(Debug, Clone)]
pub struct FileEventRepository {
	path: PathBuf,
}

impl FileEventRepository {
	pub fn new(path: impl Into<PathBuf>) -> Self {
		Self { path: path.into() }
	}

	fn read_all(&self) -> Result<Vec<TaskEvent>, StoreError> {
		if !self.path.exists() {
			return Ok(Vec::new());
		}
		let data = fs::read_to_string(&self.path)?;
		if data.trim().is_empty() {
			return Ok(Vec::new());
		}
		Ok(serde_json::from_str(&data)?)
	}

	fn write_all(&self, events: &[TaskEvent]) -> Result<(), StoreError> {
		ensure_parent_dir(&self.path)?;
		let encoded = serde_json::to_string_pretty(events)?;
		fs::write(&self.path, encoded)?;
		Ok(())
	}
}

#[derive(Debug, Clone)]
pub struct FileApprovalRepository {
	path: PathBuf,
}

impl FileApprovalRepository {
	pub fn new(path: impl Into<PathBuf>) -> Self {
		Self { path: path.into() }
	}

	fn read_all(&self) -> Result<HashMap<String, ApprovalTicket>, StoreError> {
		if !self.path.exists() {
			return Ok(HashMap::new());
		}
		let data = fs::read_to_string(&self.path)?;
		if data.trim().is_empty() {
			return Ok(HashMap::new());
		}
		Ok(serde_json::from_str(&data)?)
	}

	fn write_all(&self, tickets: &HashMap<String, ApprovalTicket>) -> Result<(), StoreError> {
		ensure_parent_dir(&self.path)?;
		let encoded = serde_json::to_string_pretty(tickets)?;
		fs::write(&self.path, encoded)?;
		Ok(())
	}
}

impl ApprovalRepository for FileApprovalRepository {
	fn save_ticket(&mut self, ticket: ApprovalTicket) -> Result<(), StoreError> {
		let mut tickets = self.read_all()?;
		tickets.insert(ticket.approval_id.0.clone(), ticket);
		self.write_all(&tickets)
	}

	fn load_ticket(&self, approval_id: &ApprovalId) -> Result<Option<ApprovalTicket>, StoreError> {
		let tickets = self.read_all()?;
		Ok(tickets.get(&approval_id.0).cloned())
	}
}

#[derive(Debug, Clone)]
pub struct FileResultRepository {
	path: PathBuf,
}

impl FileResultRepository {
	pub fn new(path: impl Into<PathBuf>) -> Self {
		Self { path: path.into() }
	}

	fn read_all(&self) -> Result<HashMap<String, ResultEnvelope>, StoreError> {
		if !self.path.exists() {
			return Ok(HashMap::new());
		}
		let data = fs::read_to_string(&self.path)?;
		if data.trim().is_empty() {
			return Ok(HashMap::new());
		}
		Ok(serde_json::from_str(&data)?)
	}

	fn write_all(&self, results: &HashMap<String, ResultEnvelope>) -> Result<(), StoreError> {
		ensure_parent_dir(&self.path)?;
		let encoded = serde_json::to_string_pretty(results)?;
		fs::write(&self.path, encoded)?;
		Ok(())
	}
}

#[derive(Debug, Clone)]
pub struct FileSessionPreferenceRepository {
	path: PathBuf,
}

impl FileSessionPreferenceRepository {
	pub fn new(path: impl Into<PathBuf>) -> Self {
		Self { path: path.into() }
	}

	fn read_all(&self) -> Result<HashMap<String, SessionPreferences>, StoreError> {
		if !self.path.exists() {
			return Ok(HashMap::new());
		}
		let data = fs::read_to_string(&self.path)?;
		if data.trim().is_empty() {
			return Ok(HashMap::new());
		}
		Ok(serde_json::from_str(&data)?)
	}

	fn write_all(
		&self,
		preferences: &HashMap<String, SessionPreferences>,
	) -> Result<(), StoreError> {
		ensure_parent_dir(&self.path)?;
		let encoded = serde_json::to_string_pretty(preferences)?;
		fs::write(&self.path, encoded)?;
		Ok(())
	}
}

impl SessionPreferenceRepository for FileSessionPreferenceRepository {
	fn save_preferences(
		&mut self,
		session_id: &str,
		preferences: SessionPreferences,
	) -> Result<(), StoreError> {
		let mut all_preferences = self.read_all()?;
		all_preferences.insert(session_id.to_string(), preferences);
		self.write_all(&all_preferences)
	}

	fn load_preferences(&self, session_id: &str) -> Result<Option<SessionPreferences>, StoreError> {
		let all_preferences = self.read_all()?;
		Ok(all_preferences.get(session_id).cloned())
	}
}

#[derive(Debug, Clone)]
pub struct FileConversationRepository {
	path: PathBuf,
}

impl FileConversationRepository {
	pub fn new(path: impl Into<PathBuf>) -> Self {
		Self { path: path.into() }
	}

	fn read_all(&self) -> Result<HashMap<String, Vec<ConversationTurn>>, StoreError> {
		if !self.path.exists() {
			return Ok(HashMap::new());
		}
		let data = fs::read_to_string(&self.path)?;
		if data.trim().is_empty() {
			return Ok(HashMap::new());
		}
		Ok(serde_json::from_str(&data)?)
	}

	fn write_all(
		&self,
		conversations: &HashMap<String, Vec<ConversationTurn>>,
	) -> Result<(), StoreError> {
		ensure_parent_dir(&self.path)?;
		let encoded = serde_json::to_string_pretty(conversations)?;
		fs::write(&self.path, encoded)?;
		Ok(())
	}
}

impl ConversationRepository for FileConversationRepository {
	fn append_turn(&mut self, session_id: &str, turn: ConversationTurn) -> Result<(), StoreError> {
		let mut conversations = self.read_all()?;
		conversations
			.entry(session_id.to_string())
			.or_default()
			.push(turn);
		self.write_all(&conversations)
	}

	fn load_recent_turns(
		&self,
		session_id: &str,
		limit: usize,
	) -> Result<Vec<ConversationTurn>, StoreError> {
		let conversations = self.read_all()?;
		let Some(turns) = conversations.get(session_id) else {
			return Ok(Vec::new());
		};
		let start = turns.len().saturating_sub(limit);
		Ok(turns[start..].to_vec())
	}
}

impl ResultRepository for FileResultRepository {
	fn save_result(&mut self, result: ResultEnvelope) -> Result<(), StoreError> {
		let mut results = self.read_all()?;
		results.insert(result_key(&result.task_id, &result.node_id), result);
		self.write_all(&results)
	}

	fn load_result(
		&self,
		task_id: &TaskId,
		node_id: &NodeId,
	) -> Result<Option<ResultEnvelope>, StoreError> {
		let results = self.read_all()?;
		Ok(results.get(&result_key(task_id, node_id)).cloned())
	}

	fn list_results(&self, task_id: &TaskId) -> Result<Vec<ResultEnvelope>, StoreError> {
		let results = self.read_all()?;
		Ok(results
			.into_values()
			.filter(|result| result.task_id == *task_id)
			.collect())
	}
}

impl EventRepository for FileEventRepository {
	fn append_event(&mut self, event: TaskEvent) -> Result<(), StoreError> {
		let mut events = self.read_all()?;
		events.push(event);
		self.write_all(&events)
	}

	fn list_events(&self, task_id: &TaskId) -> Result<Vec<TaskEvent>, StoreError> {
		let events = self.read_all()?;
		Ok(events
			.into_iter()
			.filter(|event| event.task_id.0 == task_id.0)
			.collect())
	}
}

fn ensure_parent_dir(path: &Path) -> Result<(), StoreError> {
	if let Some(parent) = path.parent() {
		fs::create_dir_all(parent)?;
	}
	Ok(())
}

fn result_key(task_id: &TaskId, node_id: &NodeId) -> String {
	format!("{}:{}", task_id.0, node_id.0)
}

#[cfg(test)]
mod tests {
	use super::*;
	use roku_common_types::{
		ConversationRole, ConversationTurn, EvidenceItem, PlanningModeHint, RequestId,
		ResultStatus, TaskState,
	};
	use std::time::{SystemTime, UNIX_EPOCH};

	fn unique_path(suffix: &str) -> PathBuf {
		let nanos = SystemTime::now()
			.duration_since(UNIX_EPOCH)
			.expect("clock should be after epoch")
			.as_nanos();
		std::env::temp_dir().join(format!("roku-store-{suffix}-{nanos}.json"))
	}

	fn sample_task() -> Task {
		Task {
			task_id: TaskId("task-1".to_string()),
			request_id: RequestId("req-1".to_string()),
			session_id: "session-1".to_string(),
			goal: "analyze market".to_string(),
			state: TaskState::Queued,
			attempts: 0,
			planning_mode_hint: None,
			conversation_history: Vec::new(),
			completed_nodes: Vec::new(),
			next_node_index: 0,
			pending_approval_id: None,
			last_result: None,
			graph: None,
		}
	}

	fn sample_event() -> TaskEvent {
		TaskEvent {
			task_id: TaskId("task-1".to_string()),
			from: TaskState::Queued,
			to: TaskState::Planning,
			reason: "start".to_string(),
			error_class: None,
		}
	}

	fn sample_ticket() -> ApprovalTicket {
		ApprovalTicket {
			approval_id: ApprovalId("approval-1".to_string()),
			task_id: TaskId("task-1".to_string()),
			request_id: RequestId("req-1".to_string()),
			node_id: roku_common_types::NodeId("node-1".to_string()),
			summary: "review risky action".to_string(),
			status: roku_common_types::ApprovalStatus::Pending,
			decided_by: None,
			comment: None,
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
		let mut session_repo = InMemorySessionPreferenceRepository::default();
		let mut conversation_repo = InMemoryConversationRepository::default();

		task_repo
			.save_task(sample_task())
			.expect("save task should succeed");
		event_repo
			.append_event(sample_event())
			.expect("append event should succeed");
		approval_repo
			.save_ticket(sample_ticket())
			.expect("save approval ticket should succeed");
		result_repo
			.save_result(sample_result())
			.expect("save result should succeed");
		session_repo
			.save_preferences(
				"session-1",
				SessionPreferences {
					planning_mode: Some(PlanningModeHint::TreeSearch),
				},
			)
			.expect("save session preferences should succeed");
		conversation_repo
			.append_turn(
				"session-1",
				ConversationTurn {
					role: ConversationRole::User,
					content: "hello".to_string(),
					created_at_unix_ms: 1,
				},
			)
			.expect("append conversation turn should succeed");

		let loaded_task = task_repo
			.load_task(&TaskId("task-1".to_string()))
			.expect("load task should succeed");
		let loaded_events = event_repo
			.list_events(&TaskId("task-1".to_string()))
			.expect("list events should succeed");
		let loaded_ticket = approval_repo
			.load_ticket(&ApprovalId("approval-1".to_string()))
			.expect("load approval ticket should succeed");
		let loaded_result = result_repo
			.load_result(&TaskId("task-1".to_string()), &NodeId("node-1".to_string()))
			.expect("load result should succeed");
		let loaded_preferences = session_repo
			.load_preferences("session-1")
			.expect("load session preferences should succeed");
		let loaded_turns = conversation_repo
			.load_recent_turns("session-1", 8)
			.expect("load conversation turns should succeed");

		assert!(loaded_task.is_some());
		assert_eq!(loaded_events.len(), 1);
		assert!(loaded_ticket.is_some());
		assert!(loaded_result.is_some());
		assert_eq!(
			loaded_preferences.and_then(|preferences| preferences.planning_mode),
			Some(PlanningModeHint::TreeSearch)
		);
		assert_eq!(loaded_turns.len(), 1);
	}

	#[test]
	fn file_repositories_roundtrip() {
		let task_path = unique_path("task");
		let event_path = unique_path("event");
		let approval_path = unique_path("approval");
		let result_path = unique_path("result");

		let mut task_repo = FileTaskRepository::new(task_path.clone());
		let mut event_repo = FileEventRepository::new(event_path.clone());
		let mut approval_repo = FileApprovalRepository::new(approval_path.clone());
		let mut result_repo = FileResultRepository::new(result_path.clone());
		let session_path = unique_path("session");
		let conversation_path = unique_path("conversation");
		let mut session_repo = FileSessionPreferenceRepository::new(session_path.clone());
		let mut conversation_repo = FileConversationRepository::new(conversation_path.clone());

		task_repo
			.save_task(sample_task())
			.expect("save task should succeed");
		event_repo
			.append_event(sample_event())
			.expect("append event should succeed");
		approval_repo
			.save_ticket(sample_ticket())
			.expect("save approval ticket should succeed");
		result_repo
			.save_result(sample_result())
			.expect("save result should succeed");
		session_repo
			.save_preferences(
				"session-1",
				SessionPreferences {
					planning_mode: Some(PlanningModeHint::ReAct),
				},
			)
			.expect("save session preferences should succeed");
		conversation_repo
			.append_turn(
				"session-1",
				ConversationTurn {
					role: ConversationRole::Assistant,
					content: "hi".to_string(),
					created_at_unix_ms: 2,
				},
			)
			.expect("append conversation turn should succeed");

		let loaded_task = task_repo
			.load_task(&TaskId("task-1".to_string()))
			.expect("load task should succeed");
		let loaded_events = event_repo
			.list_events(&TaskId("task-1".to_string()))
			.expect("list events should succeed");
		let loaded_ticket = approval_repo
			.load_ticket(&ApprovalId("approval-1".to_string()))
			.expect("load approval ticket should succeed");
		let loaded_result = result_repo
			.load_result(&TaskId("task-1".to_string()), &NodeId("node-1".to_string()))
			.expect("load result should succeed");
		let loaded_preferences = session_repo
			.load_preferences("session-1")
			.expect("load session preferences should succeed");
		let loaded_turns = conversation_repo
			.load_recent_turns("session-1", 8)
			.expect("load conversation turns should succeed");

		assert!(loaded_task.is_some());
		assert_eq!(loaded_events.len(), 1);
		assert!(loaded_ticket.is_some());
		assert!(loaded_result.is_some());
		assert_eq!(
			loaded_preferences.and_then(|preferences| preferences.planning_mode),
			Some(PlanningModeHint::ReAct)
		);
		assert_eq!(loaded_turns.len(), 1);

		let _ = fs::remove_file(task_path);
		let _ = fs::remove_file(event_path);
		let _ = fs::remove_file(approval_path);
		let _ = fs::remove_file(result_path);
		let _ = fs::remove_file(session_path);
		let _ = fs::remove_file(conversation_path);
	}
}
