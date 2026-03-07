use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum McpError {
	#[error("mcp server not found: {0}")]
	ServerNotFound(String),
	#[error("mcp tool not found: {server_id}/{tool_name}")]
	ToolNotFound {
		server_id: String,
		tool_name: String,
	},
	#[error("invalid mcp request: {0}")]
	InvalidRequest(String),
	#[error("mcp transport error: {0}")]
	Transport(String),
}
