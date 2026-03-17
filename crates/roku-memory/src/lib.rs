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

//! Provider-neutral long-term memory domain types and backend contracts for Roku.
//!
//! This crate defines Roku's own memory vocabulary: what a recall query looks like,
//! what a persisted memory record contains, how runtime policy can ask for recall
//! or write-back, and what a backend is allowed to do in response.
//!
//! Backends such as OpenViking are intentionally kept out of this crate. They map
//! these contracts onto provider-specific APIs, but they do not redefine the
//! memory model itself.

mod backend;
mod policy;
mod types;

pub use backend::{
	InMemoryLongTermMemoryBackend, LongTermMemoryBackend, MemoryBackendHealth, MemoryBackendStatus,
	MemoryDeleteSelector, MemoryError, MemoryWriteAck, NoopLongTermMemoryBackend,
};
pub use policy::{
	ConservativeMemoryLifecyclePolicy, MemoryLifecyclePolicy, MemoryRecallInput,
	MemoryWritePolicyInput,
};
pub use types::{
	MemoryFilters, MemoryHit, MemoryKind, MemoryMetadata, MemoryProvenance, MemoryQuery,
	MemoryRecallReason, MemoryRecord, MemoryScope, MemorySourceRef, MemoryWriteReason,
	MemoryWriteRequest,
};
