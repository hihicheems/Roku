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
//! This module is the stable home for recall and write-back semantics. The
//! concrete items still live in the crate's flat compatibility modules, but
//! callers consume them through this subdomain so the memory subsystem keeps a
//! stable provider-neutral surface.

pub use crate::backend::{
	InMemoryLongTermMemoryBackend, LongTermMemoryBackend, MemoryBackendHealth, MemoryBackendStatus,
	MemoryDeleteSelector, MemoryError, MemoryWriteAck, NoopLongTermMemoryBackend,
};
pub use crate::policy::{
	ConservativeMemoryLifecyclePolicy, MemoryLifecyclePolicy, MemoryRecallInput,
	MemoryWritePolicyInput,
};
pub use crate::types::{
	MemoryFilters, MemoryHit, MemoryKind, MemoryMetadata, MemoryProvenance, MemoryQuery,
	MemoryRecallReason, MemoryRecord, MemoryScope, MemorySourceRef, MemoryWriteReason,
	MemoryWriteRequest,
};
