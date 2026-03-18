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

//! Roku-owned runtime bundle namespace.
//!
//! Provider-neutral bundle ownership lives in this subsystem. Registration and
//! resolution stay in `registry`, but the resolved bundle shape lives here so
//! entry/runtime consumers have a stable home that is separate from
//! registration mechanics.

use crate::long_term::{LongTermMemoryBackend, NoopLongTermMemoryBackend};
use crate::pending_loop::{NoopPendingLoopSnapshotBackend, PendingLoopSnapshotBackend};
use crate::session::{NoopSessionStateBackend, SessionStateBackend};
use crate::short_term::{NoopShortTermContinuityBackend, ShortTermContinuityBackend};

/// Provider-neutral memory subsystem bundle returned by registry resolution.
pub struct ResolvedMemorySubsystem {
	pub long_term: std::sync::Arc<dyn LongTermMemoryBackend>,
	pub short_term: Box<dyn ShortTermContinuityBackend>,
	pub session_state: Box<dyn SessionStateBackend>,
	pub pending_loop: Box<dyn PendingLoopSnapshotBackend>,
}

impl ResolvedMemorySubsystem {
	/// Returns a fully disabled provider-neutral bundle.
	pub fn disabled() -> Self {
		Self {
			long_term: std::sync::Arc::new(NoopLongTermMemoryBackend),
			short_term: Box::new(NoopShortTermContinuityBackend),
			session_state: Box::new(NoopSessionStateBackend),
			pending_loop: Box::new(NoopPendingLoopSnapshotBackend),
		}
	}

	/// Builds a provider-neutral bundle from concrete adapter-backed parts.
	pub fn with_parts(
		long_term: std::sync::Arc<dyn LongTermMemoryBackend>,
		short_term: Box<dyn ShortTermContinuityBackend>,
		session_state: Box<dyn SessionStateBackend>,
		pending_loop: Box<dyn PendingLoopSnapshotBackend>,
	) -> Self {
		Self {
			long_term,
			short_term,
			session_state,
			pending_loop,
		}
	}
}
