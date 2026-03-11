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

//! Builtin tool catalog and runtime builders for Roku plugins.

mod builders;
mod builtin;
mod config;

pub use builders::{
	build_builtin_tool_runtime, build_builtin_tool_runtime_with_plugin_snapshot,
	build_builtin_tool_runtime_with_plugin_snapshot_and_runtime_capabilities,
	build_llm_tool_runtime, build_llm_tool_runtime_with_plugin_snapshot, build_resource_catalog,
	build_resource_catalog_with_plugin_snapshot,
	build_resource_catalog_with_plugin_snapshot_and_runtime_capabilities,
};
pub use config::{BuiltinToolRole, ConfiguredTool, ToolCatalogConfig, ToolCatalogConfigError};
