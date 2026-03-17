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

//! Roku-owned memory registry namespace.
//!
//! `registry` is the umbrella term for memory subsystem resolution. `entry
//! registry` is the main entry. `backend registry` and `runtime bundle registry`
//! are internal responsibility splits within the same registry surface.
//!
//! Phase 1 establishes namespace ownership only. Concrete bundle shapes,
//! registration APIs, and fallback implementations arrive in later phases.

use std::sync::Arc;

use crate::pending_loop::{NoopPendingLoopSnapshotBackend, PendingLoopSnapshotBackend};
use crate::session::{NoopSessionStateBackend, SessionStateBackend};
use crate::short_term::{NoopShortTermContinuityBackend, ShortTermContinuityBackend};
use crate::{
	LongTermMemoryBackend, MemoryLifecyclePolicy, MemoryQuery, MemoryRecallInput,
	MemoryWriteRequest, NoopLongTermMemoryBackend,
};

/// Disabled provider-neutral lifecycle policy.
///
/// This keeps runtime wiring intact while making recall and write-back explicit no-ops.
#[derive(Debug, Default)]
pub struct DisabledMemoryLifecyclePolicy;

impl MemoryLifecyclePolicy for DisabledMemoryLifecyclePolicy {
	fn build_recall_query(&self, _input: &MemoryRecallInput) -> Option<MemoryQuery> {
		None
	}

	fn build_write_request(
		&self,
		_input: &crate::MemoryWritePolicyInput,
	) -> Option<MemoryWriteRequest> {
		None
	}
}

/// Provider-neutral bundle shape returned by later memory registry resolution.
///
/// Phase 2 defines this shape so entry and runtime layers can converge on a
/// shared contract surface before adapter resolution is moved out of `roku-cmd`.
pub struct ResolvedMemorySubsystem {
	pub long_term: Arc<dyn LongTermMemoryBackend>,
	pub short_term: Box<dyn ShortTermContinuityBackend>,
	pub session_state: Box<dyn SessionStateBackend>,
	pub pending_loop: Box<dyn PendingLoopSnapshotBackend>,
	pub lifecycle_policy: Arc<dyn MemoryLifecyclePolicy>,
}

impl ResolvedMemorySubsystem {
	/// Returns a fully disabled provider-neutral bundle.
	pub fn disabled() -> Self {
		Self {
			long_term: Arc::new(NoopLongTermMemoryBackend),
			short_term: Box::new(NoopShortTermContinuityBackend),
			session_state: Box::new(NoopSessionStateBackend),
			pending_loop: Box::new(NoopPendingLoopSnapshotBackend),
			lifecycle_policy: Arc::new(DisabledMemoryLifecyclePolicy),
		}
	}
}
