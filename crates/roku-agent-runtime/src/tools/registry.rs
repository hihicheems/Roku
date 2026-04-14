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

//! Tool registry: stores tool entries and provides mode-aware queries.

use std::collections::HashMap;

use roku_plugin_llm::ToolDefinition;

use super::trait_def::{LoopMode, ToolEntry};

/// Registry of all tools available to the agent loop.
///
/// Tools are registered once at startup and queried per-turn for visibility
/// and definition building.
pub struct ToolRegistry {
	tools: Vec<Box<dyn ToolEntry>>,
	index: HashMap<String, usize>,
}

impl ToolRegistry {
	pub fn new() -> Self {
		Self {
			tools: Vec::new(),
			index: HashMap::new(),
		}
	}

	/// Register a tool entry. Duplicate names replace the previous entry.
	pub fn register(&mut self, tool: Box<dyn ToolEntry>) {
		let name = tool.name().to_string();
		if let Some(&existing_idx) = self.index.get(&name) {
			self.tools[existing_idx] = tool;
		} else {
			let idx = self.tools.len();
			self.index.insert(name, idx);
			self.tools.push(tool);
		}
	}

	/// Get a tool entry by name.
	pub fn get(&self, name: &str) -> Option<&dyn ToolEntry> {
		self.index.get(name).map(|&idx| self.tools[idx].as_ref())
	}

	/// Return all tools visible in the given mode.
	pub fn visible_tools(&self, mode: LoopMode) -> Vec<&dyn ToolEntry> {
		self.tools
			.iter()
			.filter(|t| t.visible_in_mode(mode))
			.map(|t| t.as_ref())
			.collect()
	}

	/// Build tool definitions for all tools visible in the given mode.
	pub fn tool_definitions(&self, mode: LoopMode) -> Vec<ToolDefinition> {
		self.visible_tools(mode)
			.into_iter()
			.map(|t| t.tool_definition())
			.collect()
	}

	/// Return the names of all registered tools.
	pub fn all_names(&self) -> Vec<&str> {
		self.tools.iter().map(|t| t.name()).collect()
	}

	/// Return the names of tools visible in the given mode.
	pub fn visible_names(&self, mode: LoopMode) -> Vec<String> {
		self.visible_tools(mode)
			.into_iter()
			.map(|t| t.name().to_string())
			.collect()
	}

	/// Number of registered tools.
	pub fn len(&self) -> usize {
		self.tools.len()
	}

	/// Whether the registry is empty.
	pub fn is_empty(&self) -> bool {
		self.tools.is_empty()
	}
}

impl Default for ToolRegistry {
	fn default() -> Self {
		Self::new()
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use serde_json::json;

	struct FakeReadTool;
	impl ToolEntry for FakeReadTool {
		fn name(&self) -> &str {
			"Read"
		}
		fn description(&self) -> &str {
			"Read a file"
		}
		fn is_read_only(&self) -> bool {
			true
		}
		fn tool_definition(&self) -> ToolDefinition {
			ToolDefinition {
				name: "Read".to_string(),
				description: "Read a file".to_string(),
				parameters: json!({"type": "object", "properties": {"path": {"type": "string"}}}),
			}
		}
	}

	struct FakeWriteTool;
	impl ToolEntry for FakeWriteTool {
		fn name(&self) -> &str {
			"Write"
		}
		fn description(&self) -> &str {
			"Write a file"
		}
		fn is_read_only(&self) -> bool {
			false
		}
		fn tool_definition(&self) -> ToolDefinition {
			ToolDefinition {
				name: "Write".to_string(),
				description: "Write a file".to_string(),
				parameters: json!({"type": "object", "properties": {"file_path": {"type": "string"}}}),
			}
		}
	}

	#[test]
	fn register_and_lookup() {
		let mut reg = ToolRegistry::new();
		reg.register(Box::new(FakeReadTool));
		reg.register(Box::new(FakeWriteTool));
		assert_eq!(reg.len(), 2);
		assert!(reg.get("Read").is_some());
		assert!(reg.get("Write").is_some());
		assert!(reg.get("Nonexistent").is_none());
	}

	#[test]
	fn visible_tools_normal_mode() {
		let mut reg = ToolRegistry::new();
		reg.register(Box::new(FakeReadTool));
		reg.register(Box::new(FakeWriteTool));
		let visible = reg.visible_names(LoopMode::Normal);
		assert_eq!(visible.len(), 2);
	}

	#[test]
	fn visible_tools_plan_mode_filters_write() {
		let mut reg = ToolRegistry::new();
		reg.register(Box::new(FakeReadTool));
		reg.register(Box::new(FakeWriteTool));
		let visible = reg.visible_names(LoopMode::Plan);
		assert_eq!(visible.len(), 1);
		assert_eq!(visible[0], "Read");
	}

	#[test]
	fn tool_definitions_respect_mode() {
		let mut reg = ToolRegistry::new();
		reg.register(Box::new(FakeReadTool));
		reg.register(Box::new(FakeWriteTool));
		let defs = reg.tool_definitions(LoopMode::Plan);
		assert_eq!(defs.len(), 1);
		assert_eq!(defs[0].name, "Read");
	}

	#[test]
	fn duplicate_register_replaces() {
		let mut reg = ToolRegistry::new();
		reg.register(Box::new(FakeReadTool));
		reg.register(Box::new(FakeReadTool));
		assert_eq!(reg.len(), 1);
	}
}
