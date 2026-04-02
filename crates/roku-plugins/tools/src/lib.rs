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

mod availability;
mod builders;
mod builtin;
mod config;
mod contract;
mod runtime_config;

use roku_common_types::CanonicalExecution;
use serde_json::Value;

pub use availability::{
	RuntimeVisibleToolAvailabilitySnapshot, build_runtime_visible_tool_availability_snapshot,
};
pub use builders::{
	build_builtin_tool_runtime, build_builtin_tool_runtime_with_plugin_snapshot,
	build_builtin_tool_runtime_with_plugin_snapshot_and_runtime_capabilities,
	build_builtin_tool_runtime_with_plugin_snapshot_and_runtime_capabilities_and_runtime_config,
	build_builtin_tool_runtime_with_plugin_snapshot_and_runtime_config, build_llm_tool_runtime,
	build_llm_tool_runtime_with_plugin_snapshot,
	build_llm_tool_runtime_with_plugin_snapshot_and_runtime_config, build_resource_catalog,
	build_resource_catalog_with_plugin_snapshot,
	build_resource_catalog_with_plugin_snapshot_and_runtime_capabilities,
	build_resource_catalog_with_plugin_snapshot_and_runtime_capabilities_and_runtime_config,
	build_resource_catalog_with_plugin_snapshot_and_runtime_config,
};
pub use config::{BuiltinToolRole, ConfiguredTool, ToolCatalogConfig, ToolCatalogConfigError};
pub use runtime_config::{
	CommandToolRuntimeConfig, CommandToolRuntimeConfigPatch, FsToolRuntimeConfig,
	FsToolRuntimeConfigPatch, PythonToolRuntimeConfig, PythonToolRuntimeConfigPatch,
	TableToolRuntimeConfig, TableToolRuntimeConfigPatch, ToolWorkerRuntimeConfig,
	ToolWorkerRuntimeConfigPatch, ToolsRuntimeConfig, ToolsRuntimeConfigError,
	ToolsRuntimeConfigPatch, WebToolRuntimeConfig, WebToolRuntimeConfigPatch,
};

pub fn canonical_execution_for_builtin_tool_input(
	tool_name: &str,
	input: &Value,
) -> Option<CanonicalExecution> {
	match tool_name {
		"command.run" => builtin::command::canonical_execution_from_runtime_input(input),
		"fs.exists" | "fs.inspect" | "fs.list_dir" | "fs.read_text" => {
			builtin::fs::canonical_execution_from_runtime_input(tool_name, input)
		}
		_ => None,
	}
}
