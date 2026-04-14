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

//! Tool trait definition for the registry system.
//!
//! Every tool visible to the agent loop implements this trait. The registry
//! queries these methods to build tool definitions, filter by mode, and
//! classify risk.

use roku_plugin_llm::ToolDefinition;

/// Operational mode for the agent loop, used to filter tool visibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LoopMode {
	/// Normal execution: all tools available.
	#[default]
	Normal,
	/// Plan mode: only read-only tools available.
	Plan,
}

/// Trait implemented by every tool in the registry.
///
/// This is a *metadata* trait — it does not handle execution (which is
/// delegated to `ToolRuntime` via the plugin host). It provides the
/// information needed for:
/// - Building tool definitions for the LLM
/// - Filtering tools by mode (normal vs plan)
/// - Classifying risk (safe vs write)
pub trait ToolEntry: Send + Sync {
	/// The canonical tool name (e.g. `"Bash"`, `"Read"`).
	fn name(&self) -> &str;

	/// A short description for the LLM's tool selection.
	fn description(&self) -> &str;

	/// Whether this tool only reads state and never mutates.
	fn is_read_only(&self) -> bool;

	/// Whether this tool is visible in the given mode.
	fn visible_in_mode(&self, mode: LoopMode) -> bool {
		match mode {
			LoopMode::Normal => true,
			LoopMode::Plan => self.is_read_only(),
		}
	}

	/// Build the LLM-facing tool definition (name, description, parameters schema).
	fn tool_definition(&self) -> ToolDefinition;
}
