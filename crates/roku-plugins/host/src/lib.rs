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

//! Plugin host building blocks for Roku.

mod admission;
mod discovery;
mod registry_loader;
mod startup;
pub mod tool_runtime;

pub use startup::{
	BundledPluginDescriptor, PluginDiscoveryConfig, PluginHostError, PluginStartupConfig,
	build_plugin_registry_snapshot, default_bundled_plugin_descriptors,
};
pub use tool_runtime::{
	ExecutionEvent, ExecutionEventKind, ExecutionHook, RuntimeConstraints, SandboxProfile, Tool,
	ToolDescriptor, ToolExecutionResult, ToolFailure, ToolInvocation, ToolInvocationRequest,
	ToolRuntime, ToolRuntimeError, ToolSchema,
};
