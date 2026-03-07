//! MCP protocol bridge boundary.

#[derive(Debug, Clone)]
pub struct McpRequest {
	pub method: String,
	pub payload: String,
}

#[derive(Debug, Clone)]
pub struct McpResponse {
	pub success: bool,
	pub payload: String,
}

pub trait McpClient {
	fn call(&self, request: &McpRequest) -> McpResponse;
}

#[derive(Debug, Default)]
pub struct NoopMcpBridge;

impl McpClient for NoopMcpBridge {
	fn call(&self, request: &McpRequest) -> McpResponse {
		McpResponse {
			success: true,
			payload: format!("mcp:{}", request.method),
		}
	}
}
