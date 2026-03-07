//! External coding provider adapter.

mod contract;
mod error;
mod provider;

pub use contract::ProviderExecutionPayload;
pub use error::CodingProviderError;
pub use provider::{CodingProvider, McpCodingProvider};
