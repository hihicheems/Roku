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

//! Session-scoped in-memory task store for structured progress tracking.
//!
//! Tasks are created and managed by the AI via pseudo-tools (task_create,
//! task_update, task_list, task_get). The store is session-scoped and
//! cleared when the session ends.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};

/// Status of a tracked task.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
	Pending,
	InProgress,
	Completed,
	Failed,
}

impl std::fmt::Display for TaskStatus {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Self::Pending => write!(f, "pending"),
			Self::InProgress => write!(f, "in_progress"),
			Self::Completed => write!(f, "completed"),
			Self::Failed => write!(f, "failed"),
		}
	}
}

impl TaskStatus {
	pub fn from_str_loose(s: &str) -> Self {
		match s.to_lowercase().as_str() {
			"in_progress" | "inprogress" | "running" => Self::InProgress,
			"completed" | "done" | "complete" => Self::Completed,
			"failed" | "error" => Self::Failed,
			_ => Self::Pending,
		}
	}
}

/// A tracked task entry.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TrackedTask {
	pub id: u32,
	pub description: String,
	pub status: TaskStatus,
	pub output: Option<String>,
}

/// Session-scoped in-memory task store.
pub(crate) struct TaskStore {
	tasks: HashMap<u32, TrackedTask>,
	next_id: AtomicU32,
}

impl TaskStore {
	pub fn new() -> Self {
		Self {
			tasks: HashMap::new(),
			next_id: AtomicU32::new(1),
		}
	}

	/// Create a new task and return its ID.
	pub fn create(&mut self, description: String, status: Option<TaskStatus>) -> u32 {
		let id = self.next_id.fetch_add(1, Ordering::Relaxed);
		let task = TrackedTask {
			id,
			description,
			status: status.unwrap_or(TaskStatus::Pending),
			output: None,
		};
		self.tasks.insert(id, task);
		id
	}

	/// Update a task's status and/or output. Returns true if the task exists.
	pub fn update(&mut self, id: u32, status: Option<TaskStatus>, output: Option<String>) -> bool {
		if let Some(task) = self.tasks.get_mut(&id) {
			if let Some(s) = status {
				task.status = s;
			}
			if let Some(o) = output {
				task.output = Some(o);
			}
			true
		} else {
			false
		}
	}

	/// Get a task by ID.
	pub fn get(&self, id: u32) -> Option<&TrackedTask> {
		self.tasks.get(&id)
	}

	/// List all tasks, sorted by ID.
	pub fn list(&self) -> Vec<&TrackedTask> {
		let mut tasks: Vec<_> = self.tasks.values().collect();
		tasks.sort_by_key(|t| t.id);
		tasks
	}

	/// Format task list for LLM consumption.
	pub fn format_list(&self) -> String {
		let tasks = self.list();
		if tasks.is_empty() {
			return "No tasks.".to_string();
		}
		tasks
			.iter()
			.map(|t| format!("#{} [{}] {}", t.id, t.status, t.description))
			.collect::<Vec<_>>()
			.join("\n")
	}

	/// Format a single task for LLM consumption.
	pub fn format_task(task: &TrackedTask) -> String {
		let mut s = format!(
			"Task #{}\nStatus: {}\nDescription: {}",
			task.id, task.status, task.description
		);
		if let Some(output) = &task.output {
			s.push_str(&format!("\nOutput: {output}"));
		}
		s
	}
}

impl Default for TaskStore {
	fn default() -> Self {
		Self::new()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn create_and_get() {
		let mut store = TaskStore::new();
		let id = store.create("Write tests".to_string(), None);
		let task = store.get(id).unwrap();
		assert_eq!(task.description, "Write tests");
		assert_eq!(task.status, TaskStatus::Pending);
	}

	#[test]
	fn update_status() {
		let mut store = TaskStore::new();
		let id = store.create("Build feature".to_string(), None);
		assert!(store.update(id, Some(TaskStatus::InProgress), None));
		assert_eq!(store.get(id).unwrap().status, TaskStatus::InProgress);
		assert!(store.update(id, Some(TaskStatus::Completed), Some("Done!".to_string())));
		let task = store.get(id).unwrap();
		assert_eq!(task.status, TaskStatus::Completed);
		assert_eq!(task.output.as_deref(), Some("Done!"));
	}

	#[test]
	fn update_nonexistent_returns_false() {
		let mut store = TaskStore::new();
		assert!(!store.update(999, Some(TaskStatus::Failed), None));
	}

	#[test]
	fn list_sorted_by_id() {
		let mut store = TaskStore::new();
		store.create("First".to_string(), None);
		store.create("Second".to_string(), None);
		store.create("Third".to_string(), None);
		let tasks = store.list();
		assert_eq!(tasks.len(), 3);
		assert!(tasks[0].id < tasks[1].id);
		assert!(tasks[1].id < tasks[2].id);
	}

	#[test]
	fn format_list_output() {
		let mut store = TaskStore::new();
		store.create("Task A".to_string(), Some(TaskStatus::InProgress));
		store.create("Task B".to_string(), Some(TaskStatus::Completed));
		let output = store.format_list();
		assert!(output.contains("[in_progress] Task A"));
		assert!(output.contains("[completed] Task B"));
	}

	#[test]
	fn status_from_str_loose() {
		assert_eq!(
			TaskStatus::from_str_loose("in_progress"),
			TaskStatus::InProgress
		);
		assert_eq!(
			TaskStatus::from_str_loose("running"),
			TaskStatus::InProgress
		);
		assert_eq!(TaskStatus::from_str_loose("done"), TaskStatus::Completed);
		assert_eq!(TaskStatus::from_str_loose("failed"), TaskStatus::Failed);
		assert_eq!(TaskStatus::from_str_loose("anything"), TaskStatus::Pending);
	}
}
