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

use std::collections::HashMap;

use crate::types::McpToolDescriptor;

#[derive(Debug, Default)]
pub struct McpToolCatalog {
	tools: HashMap<String, McpToolDescriptor>,
}

impl McpToolCatalog {
	pub fn register_tool(&mut self, descriptor: McpToolDescriptor) {
		self.tools.insert(
			tool_key(&descriptor.server_id, &descriptor.tool_name),
			descriptor,
		);
	}

	pub fn tool(&self, server_id: &str, tool_name: &str) -> Option<&McpToolDescriptor> {
		self.tools.get(&tool_key(server_id, tool_name))
	}

	pub fn list_by_server(&self, server_id: &str) -> Vec<McpToolDescriptor> {
		self.tools
			.values()
			.filter(|descriptor| descriptor.server_id == server_id)
			.cloned()
			.collect()
	}
}

pub(crate) fn tool_key(server_id: &str, tool_name: &str) -> String {
	format!("{server_id}::{tool_name}")
}
