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

use roku_agent_runtime::LoopState;
use roku_common_types::{
	RequestEnvelope, ResponseEnvelope, RuntimeError, RuntimeMemorySections, Task, TaskId,
};
use roku_observability::LogLevel;

use crate::{ContextBundle, RuntimeMemoryLayers, RuntimeService};
use crate::{log_runtime, truncate_for_log};

impl RuntimeService {
	pub fn pending_loop(&self, session_id: &str) -> Result<Option<LoopState>, RuntimeError> {
		self.pending_loop_snapshot_store.load(session_id)
	}

	pub fn restore_pending_loop(&self, loop_state: LoopState) -> Result<(), RuntimeError> {
		self.pending_loop_snapshot_store.store(&loop_state)
	}

	pub fn clear_pending_loop(&self, session_id: &str) -> Result<(), RuntimeError> {
		self.pending_loop_snapshot_store.delete(session_id)
	}

	pub(super) fn sync_pending_loop(&self, loop_state: &LoopState) -> Result<(), RuntimeError> {
		match loop_state.status {
			roku_agent_runtime::LoopStatus::AwaitingUser => {
				self.pending_loop_snapshot_store.store(loop_state)?;
			}
			_ => {
				self.pending_loop_snapshot_store
					.delete(&loop_state.session_id)?;
			}
		}
		Ok(())
	}

	pub(super) fn take_resumable_pending_loop(
		&self,
		request: &RequestEnvelope,
	) -> Result<Option<LoopState>, RuntimeError> {
		let Some(existing) = self.pending_loop_snapshot_store.load(&request.session_id)? else {
			return Ok(None);
		};
		if request.planning_mode_hint.is_some() {
			self.pending_loop_snapshot_store
				.delete(&request.session_id)?;
			return Ok(None);
		}
		let assessment = self
			.runtime
			.assess_awaiting_user_resume(&existing, &request.goal);
		if assessment.should_resume {
			self.pending_loop_snapshot_store
				.delete(&request.session_id)?;
			log_runtime(
				LogLevel::Info,
				"resuming awaiting runtime loop",
				[
					("request_id", request.request_id.0.clone()),
					("session_id", request.session_id.clone()),
					("run_id", existing.run_id.clone()),
					("reason", assessment.reason.clone()),
				],
			);
			return Ok(Some(existing));
		}
		let mut existing = existing;
		let stop_message = format!(
			"Discarded stale pending loop before new intake: {}",
			assessment.reason
		);
		self.record_runtime_loop_stop_step(&mut existing, &stop_message);
		self.pending_loop_snapshot_store.store(&existing)?;
		self.pending_loop_snapshot_store
			.delete(&request.session_id)?;
		log_runtime(
			LogLevel::Info,
			"discarded stale awaiting runtime loop before new intake",
			[
				("request_id", request.request_id.0.clone()),
				("session_id", request.session_id.clone()),
				("run_id", existing.run_id.clone()),
				("goal", truncate_for_log(&request.goal, 120)),
				("reason", assessment.reason),
			],
		);
		Ok(None)
	}

	pub(super) fn resume_pending_loop(
		&self,
		task: &mut Task,
		request: &RequestEnvelope,
		loop_state: &mut LoopState,
		context_bundle: &ContextBundle,
		runtime_memory_sections: &RuntimeMemorySections,
	) -> Result<ResponseEnvelope, RuntimeError> {
		let initial_history_len = loop_state.history.len();
		let execution = self.runtime.execute_tool_loop(
			&task.task_id,
			request,
			loop_state,
			runtime_memory_sections,
			Some(&request.goal),
		);
		self.record_runtime_loop_history(loop_state, initial_history_len);
		let response =
			self.finalize_direct_path(task, execution.node, execution.result, execution.message)?;
		if let Some(action) = execution.terminal_step_action {
			self.record_runtime_loop_terminal_step_with_action(
				loop_state,
				action,
				response.status,
				&response.message,
			);
		} else {
			self.record_runtime_loop_terminal_step(loop_state, response.status, &response.message);
		}
		self.sync_pending_loop(loop_state)?;
		self.apply_memory_write_back(request, &response, context_bundle);
		self.clear_runtime_memory_layers(&task.task_id);
		Ok(response)
	}

	pub(crate) fn cache_runtime_memory_layers(
		&self,
		task_id: &TaskId,
		layers: &RuntimeMemoryLayers,
	) {
		if let Ok(mut map) = self.runtime_memory_layers.lock() {
			map.insert(task_id.0.clone(), layers.clone());
		}
	}

	pub(crate) fn task_runtime_memory_layers(&self, task_id: &TaskId) -> RuntimeMemoryLayers {
		if let Ok(map) = self.runtime_memory_layers.lock() {
			return map.get(&task_id.0).cloned().unwrap_or_default();
		}
		RuntimeMemoryLayers::default()
	}

	pub(crate) fn clear_runtime_memory_layers(&self, task_id: &TaskId) {
		if let Ok(mut map) = self.runtime_memory_layers.lock() {
			map.remove(&task_id.0);
		}
	}
}
