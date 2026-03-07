//! Trait-backed state repositories with in-memory and file adapters.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use roku_common_types::{ApprovalId, ApprovalTicket, Task, TaskEvent, TaskId};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
	#[error("io error: {0}")]
	Io(#[from] std::io::Error),
	#[error("serialization error: {0}")]
	Serde(#[from] serde_json::Error),
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

#[cfg(test)]
mod tests {
	use super::*;
	use roku_common_types::{RequestId, TaskState};
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
			state: TaskState::Queued,
			attempts: 0,
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

	#[test]
	fn in_memory_repositories_roundtrip() {
		let mut task_repo = InMemoryTaskRepository::default();
		let mut event_repo = InMemoryEventRepository::default();
		let mut approval_repo = InMemoryApprovalRepository::default();

		task_repo
			.save_task(sample_task())
			.expect("save task should succeed");
		event_repo
			.append_event(sample_event())
			.expect("append event should succeed");
		approval_repo
			.save_ticket(sample_ticket())
			.expect("save approval ticket should succeed");

		let loaded_task = task_repo
			.load_task(&TaskId("task-1".to_string()))
			.expect("load task should succeed");
		let loaded_events = event_repo
			.list_events(&TaskId("task-1".to_string()))
			.expect("list events should succeed");
		let loaded_ticket = approval_repo
			.load_ticket(&ApprovalId("approval-1".to_string()))
			.expect("load approval ticket should succeed");

		assert!(loaded_task.is_some());
		assert_eq!(loaded_events.len(), 1);
		assert!(loaded_ticket.is_some());
	}

	#[test]
	fn file_repositories_roundtrip() {
		let task_path = unique_path("task");
		let event_path = unique_path("event");
		let approval_path = unique_path("approval");

		let mut task_repo = FileTaskRepository::new(task_path.clone());
		let mut event_repo = FileEventRepository::new(event_path.clone());
		let mut approval_repo = FileApprovalRepository::new(approval_path.clone());

		task_repo
			.save_task(sample_task())
			.expect("save task should succeed");
		event_repo
			.append_event(sample_event())
			.expect("append event should succeed");
		approval_repo
			.save_ticket(sample_ticket())
			.expect("save approval ticket should succeed");

		let loaded_task = task_repo
			.load_task(&TaskId("task-1".to_string()))
			.expect("load task should succeed");
		let loaded_events = event_repo
			.list_events(&TaskId("task-1".to_string()))
			.expect("list events should succeed");
		let loaded_ticket = approval_repo
			.load_ticket(&ApprovalId("approval-1".to_string()))
			.expect("load approval ticket should succeed");

		assert!(loaded_task.is_some());
		assert_eq!(loaded_events.len(), 1);
		assert!(loaded_ticket.is_some());

		let _ = fs::remove_file(task_path);
		let _ = fs::remove_file(event_path);
		let _ = fs::remove_file(approval_path);
	}
}
