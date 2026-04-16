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

//! Tool subsystem: trait, registry, definitions, visibility, and dispatch.
//!
//! This module provides the `ToolEntry` trait and `ToolRegistry` for managing
//! tool metadata, visibility, and definition building. Tool *execution* is
//! still handled by `ToolRuntime` in the plugin host — this module manages
//! the metadata layer above it.

pub(crate) mod definitions;
pub(crate) mod dispatch;
pub(crate) mod registry;
pub(crate) mod trait_def;
pub(crate) mod visibility;

// Re-exports for runtime.rs and other consumers.
pub(crate) use definitions::register_catalog_tools;
pub(crate) use dispatch::{
	execution_elapsed_ms, observation_from_execution, raw_tool_output_from_result, tool_result_cap,
	tool_selector_by_name, truncate_raw_tool_output, truncate_tool_result_for_message,
};
pub(crate) use registry::ToolRegistry;
pub(crate) use trait_def::LoopMode;

// Preserve the original re-exports from the old tools.rs.
pub(crate) use roku_plugin_tools::{
	build_builtin_tool_runtime_with_plugin_snapshot_and_runtime_capabilities_and_runtime_config,
	build_llm_tool_runtime_with_plugin_snapshot_and_runtime_config,
	build_resource_catalog_with_plugin_snapshot_and_runtime_capabilities_and_runtime_config,
};
