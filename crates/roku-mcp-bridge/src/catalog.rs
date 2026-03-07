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
