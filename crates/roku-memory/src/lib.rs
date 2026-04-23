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

//! Provider-neutral memory subsystem ownership for Roku.
//!
//! `roku-memory` is the Roku-owned home for memory, continuity, session, and
//! pending-loop semantics. Concrete providers such as OpenViking or SQLite are
//! adapter crates: they implement Roku contracts, but they do not redefine them.
//!
//! The provider-neutral subdomains exposed here are the stable home for
//! long-term memory, short-term continuity, session-state, pending-loop
//! persistence, bundle assembly, and registry resolution.
//!
//! `registry` is the umbrella term for subsystem resolution. `entry registry` is
//! the main entry. `backend registry` and `runtime bundle registry` are internal
//! responsibility splits within that same Roku-owned registry surface.

pub mod bundle;
pub mod config;
pub mod control_plane;
pub mod long_term;
pub mod pending_loop;
pub mod registry;
pub mod session;
pub mod short_term;

pub use bundle::ResolvedMemorySubsystem;
pub use config::{
	HARD_MAX_MEMORY_RECALL_TOP_K, HARD_MAX_MEMORY_WRITE_BATCH_SIZE, MemoryRecallConfig,
	MemoryRecallConfigPatch, MemoryRuntimeConfig, MemoryRuntimeConfigError,
	MemoryRuntimeConfigPatch, MemoryWriteConfig, MemoryWriteConfigPatch,
};
pub use control_plane::{
	ApprovalRepository, BackpressureSnapshot, ControlPlaneDataPlane, ControlPlaneError,
	DispatchClaim, DispatchEnvelope, DispatchLease, DispatchQueue, EventRepository,
	InMemoryApprovalRepository, InMemoryDispatchQueue, InMemoryEventRepository,
	InMemoryResultRepository, InMemoryTaskRepository, ResultRepository, RetryClaim, TaskRepository,
};
pub use long_term::{
	ConservativeMemoryLifecyclePolicy, InMemoryLongTermMemoryBackend, LongTermMemoryBackend,
	MemoryBackendHealth, MemoryBackendStatus, MemoryDeleteSelector, MemoryError, MemoryFilters,
	MemoryHit, MemoryKind, MemoryLifecyclePolicy, MemoryMetadata, MemoryProvenance, MemoryQuery,
	MemoryRecallInput, MemoryRecallReason, MemoryRecord, MemoryScope, MemorySourceRef,
	MemoryWriteAck, MemoryWritePolicyInput, MemoryWriteReason, MemoryWriteRequest,
	NoopLongTermMemoryBackend, latest_session_compact_summary, session_compact_summary_query,
};
pub use pending_loop::{
	NoopPendingLoopSnapshotBackend, PendingLoopSnapshot, PendingLoopSnapshotBackend,
	PendingLoopSnapshotError,
};
pub use registry::{
	DisabledMemoryLifecyclePolicy, LongTermBackendSelection, MemoryAdapterAvailability,
	MemoryBackendId, MemoryEntryRegistry, MemoryRegistryError, MemorySubsystemRegistration,
	resolve_long_term_backend_selection,
};
pub use session::{
	InMemorySessionManagementBackend, InMemorySessionStateBackend, NoopSessionManagementBackend,
	NoopSessionStateBackend, SESSION_NAME_MAX_CHARS, SESSION_NAME_MIN_CHARS, SessionCreateRequest,
	SessionDeleteMode, SessionDescriptor, SessionManagementBackend, SessionManagementError,
	SessionState, SessionStateBackend, SessionStateError, SessionSummary, normalize_session_name,
	resolve_session_name,
};
pub use short_term::{
	InMemoryShortTermContinuityBackend, NoopShortTermContinuityBackend, ShortTermContinuityBackend,
	ShortTermContinuityError,
};
