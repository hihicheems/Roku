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
use std::sync::Mutex;

use crate::LoopState;
use roku_common_types::RuntimeError;

/// Runtime-facing persistence boundary for generic paused-loop snapshots.
pub trait PendingLoopSnapshotStore: Send + Sync {
	fn load(&self, session_id: &str) -> Result<Option<LoopState>, RuntimeError>;
	fn store(&self, loop_state: &LoopState) -> Result<(), RuntimeError>;
	fn delete(&self, session_id: &str) -> Result<(), RuntimeError>;
}

#[derive(Default)]
pub struct InMemoryPendingLoopSnapshotStore {
	snapshots: Mutex<HashMap<String, LoopState>>,
}

impl PendingLoopSnapshotStore for InMemoryPendingLoopSnapshotStore {
	fn load(&self, session_id: &str) -> Result<Option<LoopState>, RuntimeError> {
		let snapshots = self.snapshots.lock().map_err(|error| {
			RuntimeError::new(format!("pending loop snapshot store poisoned: {error}"))
		})?;
		Ok(snapshots.get(session_id).cloned())
	}

	fn store(&self, loop_state: &LoopState) -> Result<(), RuntimeError> {
		let mut snapshots = self.snapshots.lock().map_err(|error| {
			RuntimeError::new(format!("pending loop snapshot store poisoned: {error}"))
		})?;
		snapshots.insert(loop_state.session_id.clone(), loop_state.clone());
		Ok(())
	}

	fn delete(&self, session_id: &str) -> Result<(), RuntimeError> {
		let mut snapshots = self.snapshots.lock().map_err(|error| {
			RuntimeError::new(format!("pending loop snapshot store poisoned: {error}"))
		})?;
		snapshots.remove(session_id);
		Ok(())
	}
}
