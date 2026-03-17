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
//! Phase 1 fixed the ownership boundary and namespace skeleton. Phase 2 then
//! pulled short-term continuity, session-state, pending-loop snapshot, and
//! registry bundle contracts back into this crate. The flat compatibility
//! modules remain as internal implementation units, while callers consume the
//! provider-neutral subdomains re-exported here.
//!
//! `registry` is the umbrella term for subsystem resolution. `entry registry` is
//! the main entry. `backend registry` and `runtime bundle registry` are internal
//! responsibility splits within that same Roku-owned registry surface.

mod backend;
mod policy;
mod types;

pub mod bundle;
pub mod config;
pub mod long_term;
pub mod pending_loop;
pub mod registry;
pub mod session;
pub mod short_term;

pub use backend::{
	InMemoryLongTermMemoryBackend, LongTermMemoryBackend, MemoryBackendHealth, MemoryBackendStatus,
	MemoryDeleteSelector, MemoryError, MemoryWriteAck, NoopLongTermMemoryBackend,
};
pub use pending_loop::{
	NoopPendingLoopSnapshotBackend, PendingLoopSnapshot, PendingLoopSnapshotBackend,
	PendingLoopSnapshotError,
};
pub use policy::{
	ConservativeMemoryLifecyclePolicy, MemoryLifecyclePolicy, MemoryRecallInput,
	MemoryWritePolicyInput,
};
pub use registry::{
	DisabledMemoryLifecyclePolicy, LongTermBackendSelection, MemoryAdapterAvailability,
	MemoryBackendId, MemoryEntryRegistry, MemoryRegistryError, MemorySubsystemRegistration,
	ResolvedMemorySubsystem, resolve_long_term_backend_selection,
};
pub use session::{NoopSessionStateBackend, SessionState, SessionStateBackend, SessionStateError};
pub use short_term::{
	NoopShortTermContinuityBackend, ShortTermContinuityBackend, ShortTermContinuityError,
};
pub use types::{
	MemoryFilters, MemoryHit, MemoryKind, MemoryMetadata, MemoryProvenance, MemoryQuery,
	MemoryRecallReason, MemoryRecord, MemoryScope, MemorySourceRef, MemoryWriteReason,
	MemoryWriteRequest,
};
