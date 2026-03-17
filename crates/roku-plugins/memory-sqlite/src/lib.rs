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

//! SQLite adapter for Roku continuity/session memory contracts.
//!
//! This crate keeps SQLite at the same adapter layer as OpenViking. It
//! implements Roku-owned short-term continuity, session-state, and pending-loop
//! snapshot contracts without turning SQLite into a privileged core concern.

mod backend;
mod config;

pub use backend::{
	SqliteMemoryAdapterError, SqliteMemoryAdapters, SqliteMemoryRegistration,
	SqlitePendingLoopSnapshotAdapter, SqliteSessionStateAdapter, SqliteShortTermContinuityAdapter,
};
pub use config::{SqliteMemoryConfig, SqliteMemoryConfigError, SqliteMemoryConfigPatch};
