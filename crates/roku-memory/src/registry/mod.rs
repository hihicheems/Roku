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
//! availability helpers into this module. Phase 4 now adds the entry-registry
//! resolution surface so composition roots can call one Roku-owned registry
//! instead of rebuilding provider selection in each entry module.

use std::fmt;
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
use thiserror::Error;

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

impl fmt::Display for MemoryBackendId {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		formatter.write_str(self.as_str())
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

/// Provider-neutral registration surface consumed by the entry registry.
///
/// Adapter crates implement this trait so the registry can resolve a complete
/// memory subsystem bundle without `roku-cmd` importing concrete provider
/// bootstrap logic into every entry surface.
pub trait MemorySubsystemRegistration {
	fn availability(&self) -> MemoryAdapterAvailability;

	fn resolve_subsystem(&self) -> Result<ResolvedMemorySubsystem, String>;
}

/// Errors produced while resolving the selected entry-layer memory subsystem.
#[derive(Debug, Error)]
pub enum MemoryRegistryError {
	#[error("memory adapter `{backend}` failed to resolve: {message}")]
	AdapterBootstrap {
		backend: MemoryBackendId,
		message: String,
	},
}

/// Roku-owned entry registry for memory subsystem resolution.
///
/// The registry owns provider-neutral selection and disabled fallback semantics.
/// Composition roots may register concrete adapter implementations, but they
/// no longer rebuild provider selection logic inside each entry surface.
#[derive(Default)]
pub struct MemoryEntryRegistry<'a> {
	registrations: Vec<&'a dyn MemorySubsystemRegistration>,
}

impl<'a> MemoryEntryRegistry<'a> {
	/// Returns an empty entry registry.
	pub fn new() -> Self {
		Self::default()
	}

	/// Registers one adapter-backed memory subsystem.
	pub fn register(&mut self, registration: &'a dyn MemorySubsystemRegistration) -> &mut Self {
		self.registrations.push(registration);
		self
	}

	/// Resolves the selected memory subsystem or returns a disabled bundle.
	pub fn resolve_subsystem(
		&self,
		enabled: bool,
		requested: MemoryBackendId,
	) -> Result<ResolvedMemorySubsystem, MemoryRegistryError> {
		if !enabled {
			return Ok(ResolvedMemorySubsystem::disabled());
		}

		let registration = self
			.registrations
			.iter()
			.find(|registration| registration.availability().backend == requested)
			.copied();

		let Some(registration) = registration else {
			return Ok(ResolvedMemorySubsystem::disabled());
		};

		registration
			.resolve_subsystem()
			.map_err(|message| MemoryRegistryError::AdapterBootstrap {
				backend: requested,
				message,
			})
	}
}

#[cfg(test)]
mod tests {
	use std::sync::Arc;

	use crate::pending_loop::NoopPendingLoopSnapshotBackend;
	use crate::session::NoopSessionStateBackend;
	use crate::short_term::NoopShortTermContinuityBackend;
	use crate::{DisabledMemoryLifecyclePolicy, NoopLongTermMemoryBackend};

	use super::{
		LongTermBackendSelection, MemoryAdapterAvailability, MemoryBackendId, MemoryEntryRegistry,
		MemorySubsystemRegistration, ResolvedMemorySubsystem, resolve_long_term_backend_selection,
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

	struct StubRegistration {
		availability: MemoryAdapterAvailability,
	}

	impl MemorySubsystemRegistration for StubRegistration {
		fn availability(&self) -> MemoryAdapterAvailability {
			self.availability
		}

		fn resolve_subsystem(&self) -> Result<ResolvedMemorySubsystem, String> {
			Ok(ResolvedMemorySubsystem::with_parts(
				Arc::new(NoopLongTermMemoryBackend),
				Box::new(NoopShortTermContinuityBackend),
				Box::new(NoopSessionStateBackend),
				Box::new(NoopPendingLoopSnapshotBackend),
				Arc::new(DisabledMemoryLifecyclePolicy),
			))
		}
	}

	#[test]
	fn entry_registry_returns_disabled_bundle_when_memory_is_disabled() {
		let registry = MemoryEntryRegistry::new();
		let resolved = registry
			.resolve_subsystem(false, MemoryBackendId::OpenViking)
			.expect("disabled memory should resolve");

		assert_eq!(
			resolved
				.lifecycle_policy
				.build_recall_query(&crate::MemoryRecallInput {
					goal: "noop".to_string(),
					session_id: "session-1".to_string(),
					planning_mode_hint_present: false,
					pending_loop_active: false,
					short_term_continuity: Vec::new(),
					user_id: None,
					project_id: None,
					workspace_id: None,
				}),
			None
		);
	}

	#[test]
	fn entry_registry_selects_the_requested_adapter_registration() {
		let sqlite = StubRegistration {
			availability: MemoryAdapterAvailability {
				backend: MemoryBackendId::Sqlite,
				long_term: false,
				short_term: true,
				session_state: true,
				pending_loop: true,
			},
		};
		let openviking = StubRegistration {
			availability: MemoryAdapterAvailability {
				backend: MemoryBackendId::OpenViking,
				long_term: true,
				short_term: true,
				session_state: true,
				pending_loop: true,
			},
		};
		let mut registry = MemoryEntryRegistry::new();
		registry.register(&sqlite).register(&openviking);

		registry
			.resolve_subsystem(true, MemoryBackendId::OpenViking)
			.expect("registered adapter should resolve");
	}

	#[test]
	fn entry_registry_returns_disabled_bundle_when_backend_is_not_registered() {
		let registry = MemoryEntryRegistry::new();

		let resolved = registry
			.resolve_subsystem(true, MemoryBackendId::OpenViking)
			.expect("missing adapter should fall back to disabled bundle");

		assert_eq!(
			resolved
				.lifecycle_policy
				.build_recall_query(&crate::MemoryRecallInput {
					goal: "noop".to_string(),
					session_id: "session-1".to_string(),
					planning_mode_hint_present: false,
					pending_loop_active: false,
					short_term_continuity: Vec::new(),
					user_id: None,
					project_id: None,
					workspace_id: None,
				}),
			None
		);
	}
}
