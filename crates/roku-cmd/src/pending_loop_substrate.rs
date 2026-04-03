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

//! Command-side bridge from the provider-neutral memory backend to the runtime-owned
//! pending-loop substrate.
//!
//! `roku-cmd` resolves concrete memory adapters as part of process bootstrap, while
//! `roku-runtime-service` owns the generic paused-loop contract. This module keeps that
//! composition-root glue in one place so CLI flows can use the shared runtime substrate instead
//! of keeping a separate pending/resume shape.

use roku_agent_runtime::LoopState;
use roku_common_types::RuntimeError;
use roku_memory::{PendingLoopSnapshot, PendingLoopSnapshotBackend, PendingLoopSnapshotError};
use roku_runtime_service::PendingLoopSnapshotStore;

pub(crate) struct MemoryPendingLoopSnapshotStore {
	backend: Box<dyn PendingLoopSnapshotBackend>,
}

impl MemoryPendingLoopSnapshotStore {
	pub(crate) fn new(backend: Box<dyn PendingLoopSnapshotBackend>) -> Self {
		Self { backend }
	}
}

impl PendingLoopSnapshotStore for MemoryPendingLoopSnapshotStore {
	fn load(&self, session_id: &str) -> Result<Option<LoopState>, RuntimeError> {
		let Some(snapshot) = self
			.backend
			.load_pending_loop_snapshot(session_id)
			.map_err(pending_loop_snapshot_error)?
		else {
			return Ok(None);
		};
		match serde_json::from_str::<LoopState>(&snapshot.loop_state_json) {
			Ok(loop_state) => Ok(Some(loop_state)),
			Err(_) => {
				self.backend
					.clear_pending_loop_snapshot(session_id)
					.map_err(pending_loop_snapshot_error)?;
				Ok(None)
			}
		}
	}

	fn store(&self, loop_state: &LoopState) -> Result<(), RuntimeError> {
		let snapshot = PendingLoopSnapshot {
			run_id: loop_state.run_id.clone(),
			loop_state_json: serde_json::to_string(loop_state).map_err(|error| {
				RuntimeError::new(format!("failed to encode pending runtime loop: {error}"))
			})?,
		};
		self.backend
			.save_pending_loop_snapshot(&loop_state.session_id, Some(snapshot))
			.map_err(pending_loop_snapshot_error)
	}

	fn delete(&self, session_id: &str) -> Result<(), RuntimeError> {
		self.backend
			.clear_pending_loop_snapshot(session_id)
			.map_err(pending_loop_snapshot_error)
	}
}

fn pending_loop_snapshot_error(error: PendingLoopSnapshotError) -> RuntimeError {
	RuntimeError::new(error.to_string())
}
