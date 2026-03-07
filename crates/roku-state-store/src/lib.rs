//! In-memory state store adapters.

use std::collections::HashMap;

use roku_common_types::{Task, TaskEvent, TaskId};

pub trait TaskStore {
	fn upsert_task(&mut self, task: Task);
	fn get_task(&self, task_id: &TaskId) -> Option<&Task>;
}

pub trait EventStore {
	fn append_event(&mut self, event: TaskEvent);
	fn list_events(&self, task_id: &TaskId) -> Vec<&TaskEvent>;
}

#[derive(Debug, Default)]
pub struct InMemoryTaskStore {
	tasks: HashMap<String, Task>,
}

impl TaskStore for InMemoryTaskStore {
	fn upsert_task(&mut self, task: Task) {
		self.tasks.insert(task.task_id.0.clone(), task);
	}

	fn get_task(&self, task_id: &TaskId) -> Option<&Task> {
		self.tasks.get(&task_id.0)
	}
}

#[derive(Debug, Default)]
pub struct InMemoryEventStore {
	events: Vec<TaskEvent>,
}

impl EventStore for InMemoryEventStore {
	fn append_event(&mut self, event: TaskEvent) {
		self.events.push(event);
	}

	fn list_events(&self, task_id: &TaskId) -> Vec<&TaskEvent> {
		self.events
			.iter()
			.filter(|event| event.task_id.0 == task_id.0)
			.collect()
	}
}
