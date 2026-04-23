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

//! Roku-owned long-term memory namespace.
//!
//! This module is the stable home for recall and write-back semantics.
//! Long-term contracts, types, and lifecycle policy live directly under this
//! subdomain so the crate no longer depends on top-level long-term-only
//! compatibility files.

mod backend;
mod policy;
mod session_summary;
mod types;

pub use backend::{
	InMemoryLongTermMemoryBackend, LongTermMemoryBackend, MemoryBackendHealth, MemoryBackendStatus,
	MemoryDeleteSelector, MemoryError, MemoryWriteAck, NoopLongTermMemoryBackend,
};
pub use policy::{
	ConservativeMemoryLifecyclePolicy, MemoryLifecyclePolicy, MemoryRecallInput,
	MemoryWritePolicyInput,
};
pub use session_summary::{
	COMPACT_SUMMARY_SENTINEL, latest_session_compact_summary, session_compact_summary_query,
};
pub use types::{
	MemoryFilters, MemoryHit, MemoryKind, MemoryMetadata, MemoryProvenance, MemoryQuery,
	MemoryRecallReason, MemoryRecord, MemoryScope, MemorySourceRef, MemoryWriteReason,
	MemoryWriteRequest,
};
