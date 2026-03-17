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

use std::collections::HashMap;
use std::sync::MutexGuard;

use roku_agent_runtime::LoopState;
use roku_common_types::{RequestEnvelope, ResponseEnvelope, RuntimeError, Task};
use roku_observability::LogLevel;

use crate::{ContextBundle, RuntimeService};
use crate::{log_runtime, truncate_for_log};

impl RuntimeService {
	pub fn pending_loop(&self, session_id: &str) -> Result<Option<LoopState>, RuntimeError> {
		let pending = self.lock_pending_loops()?;
		Ok(pending.get(session_id).cloned())
	}

	pub fn restore_pending_loop(&self, loop_state: LoopState) -> Result<(), RuntimeError> {
		let mut pending = self.lock_pending_loops()?;
		pending.insert(loop_state.session_id.clone(), loop_state);
		Ok(())
	}

	pub fn clear_pending_loop(&self, session_id: &str) -> Result<(), RuntimeError> {
		let mut pending = self.lock_pending_loops()?;
		pending.remove(session_id);
		Ok(())
	}

	pub(super) fn has_pending_loop(&self, session_id: &str) -> Result<bool, RuntimeError> {
		let pending = self.lock_pending_loops()?;
		Ok(pending.contains_key(session_id))
	}

	pub(super) fn sync_pending_loop(&self, loop_state: &LoopState) -> Result<(), RuntimeError> {
		let mut pending = self.lock_pending_loops()?;
		match loop_state.status {
			roku_agent_runtime::LoopStatus::AwaitingUser => {
				pending.insert(loop_state.session_id.clone(), loop_state.clone());
			}
			_ => {
				pending.remove(&loop_state.session_id);
			}
		}
		Ok(())
	}

	pub(super) fn take_resumable_pending_loop(
		&self,
		request: &RequestEnvelope,
	) -> Result<Option<LoopState>, RuntimeError> {
		let mut pending = self.lock_pending_loops()?;
		let Some(existing) = pending.get(&request.session_id).cloned() else {
			return Ok(None);
		};
		if request.planning_mode_hint.is_some() {
			pending.remove(&request.session_id);
			return Ok(None);
		}
		let assessment = self
			.runtime
			.assess_awaiting_user_resume(&existing, &request.goal);
		if assessment.should_resume {
			pending.remove(&request.session_id);
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
		pending.remove(&request.session_id);
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
	) -> Result<ResponseEnvelope, RuntimeError> {
		let initial_history_len = loop_state.history.len();
		let execution =
			self.runtime
				.execute_tool_loop(&task.task_id, request, loop_state, Some(&request.goal));
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
		Ok(response)
	}

	fn lock_pending_loops(
		&self,
	) -> Result<MutexGuard<'_, HashMap<String, LoopState>>, RuntimeError> {
		self.pending_loops
			.lock()
			.map_err(|error| RuntimeError::new(format!("pending loop state poisoned: {error}")))
	}
}
