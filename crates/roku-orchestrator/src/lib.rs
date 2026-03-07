//! Task state machine and orchestration primitives.

use roku_common_types::{
	ErrorClass, RequestEnvelope, RuntimeError, Task, TaskEvent, TaskId, TaskState,
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
			| (TaskState::Aggregating, TaskState::Succeeded)
			| (TaskState::Failed, TaskState::Planning)
			| (_, TaskState::Failed)
			| (TaskState::Failed, TaskState::DeadLetter)
			| (_, TaskState::Cancelled)
	)
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
}
