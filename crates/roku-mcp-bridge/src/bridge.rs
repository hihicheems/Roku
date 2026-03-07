use std::collections::HashMap;

use crate::catalog::McpToolCatalog;
use crate::{McpError, McpRequest, McpResponse, McpToolDescriptor};

pub trait McpClient {
	fn call(&self, request: &McpRequest) -> Result<McpResponse, McpError>;
	fn discover_tools(&self, server_id: &str) -> Result<Vec<McpToolDescriptor>, McpError>;
}

#[derive(Debug, Default)]
pub struct InMemoryMcpBridge {
	catalog: McpToolCatalog,
	responses: HashMap<String, McpResponse>,
}

impl InMemoryMcpBridge {
	pub fn register_tool(&mut self, descriptor: McpToolDescriptor) {
		self.catalog.register_tool(descriptor);
	}

	pub fn register_response(
		&mut self,
		server_id: impl Into<String>,
		method: impl Into<String>,
		response: McpResponse,
	) {
		let key = response_key(&server_id.into(), &method.into());
		self.responses.insert(key, response);
	}
}

impl McpClient for InMemoryMcpBridge {
	fn call(&self, request: &McpRequest) -> Result<McpResponse, McpError> {
		let tools = self.catalog.list_by_server(&request.server_id);
		if tools.is_empty() {
			return Err(McpError::ServerNotFound(request.server_id.clone()));
		}

		if let Some(tool_name) = parse_tool_call_method(&request.method)
			&& self.catalog.tool(&request.server_id, tool_name).is_none()
		{
			return Err(McpError::ToolNotFound {
				server_id: request.server_id.clone(),
				tool_name: tool_name.to_string(),
			});
		}

		if let Some(response) = self
			.responses
			.get(&response_key(&request.server_id, &request.method))
		{
			return Ok(response.clone());
		}

		Ok(McpResponse {
			success: true,
			payload: format!("mcp:{}:{}", request.server_id, request.method),
			audit_ref: Some(format!("audit:{}:{}", request.server_id, request.method)),
		})
	}

	fn discover_tools(&self, server_id: &str) -> Result<Vec<McpToolDescriptor>, McpError> {
		let tools = self.catalog.list_by_server(server_id);
		if tools.is_empty() {
			return Err(McpError::ServerNotFound(server_id.to_string()));
		}
		Ok(tools)
	}
}

fn response_key(server_id: &str, method: &str) -> String {
	format!("{server_id}::{method}")
}

fn parse_tool_call_method(method: &str) -> Option<&str> {
	method.strip_prefix("tools/call/")
}

#[cfg(test)]
mod tests {
	use super::*;

	fn coding_descriptor() -> McpToolDescriptor {
		McpToolDescriptor {
			server_id: "coding-provider".to_string(),
			tool_name: "coding.execute".to_string(),
			description: "Execute coding work contract".to_string(),
			required_capabilities: vec!["external:coding_provider".to_string()],
			input_schema: "coding_work_contract.v1".to_string(),
			output_schema: "code_change_report.v1".to_string(),
		}
	}

	#[test]
	fn discover_registered_tools() {
		let mut bridge = InMemoryMcpBridge::default();
		bridge.register_tool(coding_descriptor());

		let tools = bridge
			.discover_tools("coding-provider")
			.expect("tools should be discoverable");
		assert_eq!(tools.len(), 1);
		assert_eq!(tools[0].tool_name, "coding.execute");
	}

	#[test]
	fn reject_unknown_tool_calls() {
		let mut bridge = InMemoryMcpBridge::default();
		bridge.register_tool(coding_descriptor());

		let error = bridge
			.call(&McpRequest {
				server_id: "coding-provider".to_string(),
				method: "tools/call/missing".to_string(),
				payload: "{}".to_string(),
			})
			.expect_err("unknown tool should be rejected");
		assert!(matches!(
			error,
			McpError::ToolNotFound {
				server_id,
				tool_name
			} if server_id == "coding-provider" && tool_name == "missing"
		));
	}

	#[test]
	fn return_registered_response_for_tool_call() {
		let mut bridge = InMemoryMcpBridge::default();
		bridge.register_tool(coding_descriptor());
		bridge.register_response(
			"coding-provider",
			"tools/call/coding.execute",
			McpResponse {
				success: true,
				payload: "{\"status\":\"ok\"}".to_string(),
				audit_ref: Some("audit-1".to_string()),
			},
		);

		let response = bridge
			.call(&McpRequest {
				server_id: "coding-provider".to_string(),
				method: "tools/call/coding.execute".to_string(),
				payload: "{}".to_string(),
			})
			.expect("tool call should succeed");

		assert_eq!(response.payload, "{\"status\":\"ok\"}");
		assert_eq!(response.audit_ref.as_deref(), Some("audit-1"));
	}
}
