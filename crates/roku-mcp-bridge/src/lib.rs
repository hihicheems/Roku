//! MCP protocol bridge boundary.

mod bridge;
mod catalog;
mod error;
mod types;

pub use bridge::{InMemoryMcpBridge, McpClient};
pub use catalog::McpToolCatalog;
pub use error::McpError;
pub use types::{McpRequest, McpResponse, McpToolDescriptor};
