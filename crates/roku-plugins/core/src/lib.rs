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

mod error;
mod id;
mod kind;
mod manifest;
mod policy;
mod registry;
mod source;

pub use error::{PluginCoreError, PluginIdError};
pub use id::PluginId;
pub use kind::PluginKind;
pub use manifest::{PluginCapabilities, PluginManifest, PluginRequirements};
pub use policy::{PluginEntryPolicy, PluginPolicyConfig, PluginProfile};
pub use registry::{
	PluginDisableReason, PluginRegistryEntry, PluginRegistrySnapshot, PluginStatus,
};
pub use source::{PluginSource, PluginSourceKind};
