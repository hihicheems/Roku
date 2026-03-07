use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpRequest {
	pub server_id: String,
	pub method: String,
	pub payload: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpResponse {
	pub success: bool,
	pub payload: String,
	pub audit_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpToolDescriptor {
	pub server_id: String,
	pub tool_name: String,
	pub description: String,
	pub required_capabilities: Vec<String>,
	pub input_schema: String,
	pub output_schema: String,
}
