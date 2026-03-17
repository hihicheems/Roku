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

//! Roku-owned pending-loop snapshot namespace.
//!
//! Pending-loop persistence belongs to the memory subsystem only when it is part
//! of Roku's continuity/session contract. Phase 2 moved the provider-neutral
//! snapshot contract here; concrete persistence remains adapter work.

use roku_common_types::PendingLoopBinding;
use thiserror::Error;

/// Persisted pending-loop binding used to resume a runtime loop later.
pub type PendingLoopSnapshot = PendingLoopBinding;

/// Error returned by pending-loop snapshot backends.
#[derive(Debug, Error)]
pub enum PendingLoopSnapshotError {
	#[error("pending-loop snapshot backend failed: {0}")]
	Backend(String),
}

/// Provider-neutral pending-loop snapshot contract.
pub trait PendingLoopSnapshotBackend: Send + Sync {
	fn load_pending_loop_snapshot(
		&self,
		session_id: &str,
	) -> Result<Option<PendingLoopSnapshot>, PendingLoopSnapshotError>;

	fn save_pending_loop_snapshot(
		&self,
		session_id: &str,
		snapshot: Option<PendingLoopSnapshot>,
	) -> Result<(), PendingLoopSnapshotError>;

	fn clear_pending_loop_snapshot(
		&self,
		session_id: &str,
	) -> Result<(), PendingLoopSnapshotError> {
		self.save_pending_loop_snapshot(session_id, None)
	}
}

/// Disabled pending-loop snapshot backend used when resume bindings are unavailable.
#[derive(Debug, Default)]
pub struct NoopPendingLoopSnapshotBackend;

impl PendingLoopSnapshotBackend for NoopPendingLoopSnapshotBackend {
	fn load_pending_loop_snapshot(
		&self,
		_session_id: &str,
	) -> Result<Option<PendingLoopSnapshot>, PendingLoopSnapshotError> {
		Ok(None)
	}

	fn save_pending_loop_snapshot(
		&self,
		_session_id: &str,
		_snapshot: Option<PendingLoopSnapshot>,
	) -> Result<(), PendingLoopSnapshotError> {
		Ok(())
	}
}
