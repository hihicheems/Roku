use thiserror::Error;

use roku_mcp_bridge::McpError;

#[derive(Debug, Error)]
pub enum CodingProviderError {
	#[error("invalid coding work contract: {0}")]
	InvalidContract(String),
	#[error("mcp bridge error: {0}")]
	Bridge(#[from] McpError),
	#[error("coding provider rejected work contract: {0}")]
	ProviderRejected(String),
	#[error("invalid coding provider response: {0}")]
	InvalidResponse(String),
}
