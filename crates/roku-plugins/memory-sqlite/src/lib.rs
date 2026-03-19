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

//! SQLite adapter for Roku memory and control-plane contracts.
//!
//! This crate keeps SQLite at the same adapter layer as OpenViking. It
//! implements Roku-owned short-term continuity, session-state, pending-loop
//! snapshot, and control-plane persistence contracts without turning SQLite
//! into a privileged core concern.

mod backend;
mod config;
pub mod control_plane;
mod registration;
mod store;

pub use backend::{
	SqliteMemoryAdapterError, SqliteMemoryAdapters, SqlitePendingLoopSnapshotAdapter,
	SqliteSessionManagementAdapter, SqliteSessionStateAdapter, SqliteShortTermContinuityAdapter,
};
pub use config::{SqliteMemoryConfig, SqliteMemoryConfigError, SqliteMemoryConfigPatch};
pub use control_plane::{
	SqliteApprovalRepository, SqliteControlPlaneConfig, SqliteControlPlaneDataPlane,
	SqliteControlPlaneError, SqliteDispatchQueue, SqliteEventRepository, SqliteResultRepository,
	SqliteTaskRepository,
};
pub use registration::{SqliteMemoryRegistration, SqliteMemorySubsystemRegistration};
