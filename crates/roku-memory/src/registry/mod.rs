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
//! Phase 1 established namespace ownership. Phase 2 and Phase 3 then moved the
//! provider-neutral bundle shape, disabled fallbacks, backend ids, and adapter
//! availability helpers into this module so later entry unification can build
//! on Roku-owned registry semantics instead of `roku-cmd` bootstrap code.

use std::str::FromStr;
use std::sync::Arc;

use crate::pending_loop::{NoopPendingLoopSnapshotBackend, PendingLoopSnapshotBackend};
use crate::session::{NoopSessionStateBackend, SessionStateBackend};
use crate::short_term::{NoopShortTermContinuityBackend, ShortTermContinuityBackend};
use crate::{
	LongTermMemoryBackend, MemoryLifecyclePolicy, MemoryQuery, MemoryRecallInput,
	MemoryWriteRequest, NoopLongTermMemoryBackend,
};
use serde::{Deserialize, Serialize};

/// Provider-neutral identifier for the configured long-term memory backend.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryBackendId {
	#[default]
	OpenViking,
	Sqlite,
}

impl MemoryBackendId {
	/// Returns the stable config/diagnostic label for the backend.
	pub const fn as_str(self) -> &'static str {
		match self {
			Self::OpenViking => "openviking",
			Self::Sqlite => "sqlite",
		}
	}
}

impl FromStr for MemoryBackendId {
	type Err = &'static str;

	fn from_str(value: &str) -> Result<Self, Self::Err> {
		match value.trim().to_ascii_lowercase().as_str() {
			"openviking" => Ok(Self::OpenViking),
			"sqlite" => Ok(Self::Sqlite),
			_ => Err("expected one of: openviking, sqlite"),
		}
	}
}

/// Capability snapshot advertised by one adapter registration surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryAdapterAvailability {
	pub backend: MemoryBackendId,
	pub long_term: bool,
	pub short_term: bool,
	pub session_state: bool,
	pub pending_loop: bool,
}

impl MemoryAdapterAvailability {
	/// Returns an availability snapshot for an adapter with no active capabilities.
	pub const fn unavailable(backend: MemoryBackendId) -> Self {
		Self {
			backend,
			long_term: false,
			short_term: false,
			session_state: false,
			pending_loop: false,
		}
	}
}

/// Registry-side resolution result for the active long-term backend selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LongTermBackendSelection {
	Disabled,
	Backend(MemoryBackendId),
}

/// Resolves the effective long-term backend choice using registry-owned fallback rules.
pub fn resolve_long_term_backend_selection(
	enabled: bool,
	requested: MemoryBackendId,
	availability: MemoryAdapterAvailability,
) -> LongTermBackendSelection {
	if !enabled || availability.backend != requested || !availability.long_term {
		LongTermBackendSelection::Disabled
	} else {
		LongTermBackendSelection::Backend(requested)
	}
}

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

/// Provider-neutral bundle shape returned by memory registry resolution.
///
/// Phase 2 defined this shape so entry and runtime layers can converge on a
/// shared contract surface before full entry-registry wiring is moved out of
/// `roku-cmd`.
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

	/// Builds a provider-neutral bundle from concrete adapter-backed parts.
	pub fn with_parts(
		long_term: Arc<dyn LongTermMemoryBackend>,
		short_term: Box<dyn ShortTermContinuityBackend>,
		session_state: Box<dyn SessionStateBackend>,
		pending_loop: Box<dyn PendingLoopSnapshotBackend>,
		lifecycle_policy: Arc<dyn MemoryLifecyclePolicy>,
	) -> Self {
		Self {
			long_term,
			short_term,
			session_state,
			pending_loop,
			lifecycle_policy,
		}
	}
}

#[cfg(test)]
mod tests {
	use super::{
		LongTermBackendSelection, MemoryAdapterAvailability, MemoryBackendId,
		resolve_long_term_backend_selection,
	};

	#[test]
	fn disables_long_term_when_adapter_id_does_not_match_requested_backend() {
		let selection = resolve_long_term_backend_selection(
			true,
			MemoryBackendId::OpenViking,
			MemoryAdapterAvailability {
				backend: MemoryBackendId::Sqlite,
				long_term: true,
				short_term: true,
				session_state: true,
				pending_loop: true,
			},
		);

		assert_eq!(selection, LongTermBackendSelection::Disabled);
	}

	#[test]
	fn selects_backend_only_when_requested_adapter_advertises_long_term_capability() {
		let selection = resolve_long_term_backend_selection(
			true,
			MemoryBackendId::OpenViking,
			MemoryAdapterAvailability {
				backend: MemoryBackendId::OpenViking,
				long_term: true,
				short_term: false,
				session_state: false,
				pending_loop: false,
			},
		);

		assert_eq!(
			selection,
			LongTermBackendSelection::Backend(MemoryBackendId::OpenViking)
		);
	}
}
