use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolFailure {
	pub message: String,
	pub retriable: bool,
}

impl ToolFailure {
	pub fn retriable(message: impl Into<String>) -> Self {
		Self {
			message: message.into(),
			retriable: true,
		}
	}

	pub fn terminal(message: impl Into<String>) -> Self {
		Self {
			message: message.into(),
			retriable: false,
		}
	}
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ToolRuntimeError {
	#[error("tool not found: {0}")]
	ToolNotFound(String),
	#[error("tool already registered: {0}")]
	ToolAlreadyRegistered(String),
	#[error("invalid descriptor: {0}")]
	InvalidDescriptor(String),
	#[error("input schema violation for {tool}: missing required fields {missing_fields:?}")]
	InputSchemaViolation {
		tool: String,
		missing_fields: Vec<String>,
	},
	#[error("capability denied for {tool}: missing {missing_capabilities:?}")]
	CapabilityDenied {
		tool: String,
		missing_capabilities: Vec<String>,
	},
	#[error("tool timed out: {tool} exceeded {timeout_ms}ms (elapsed {elapsed_ms}ms)")]
	Timeout {
		tool: String,
		timeout_ms: u64,
		elapsed_ms: u128,
	},
	#[error("tool execution failed: {tool} after {attempts} attempts: {message}")]
	ExecutionFailed {
		tool: String,
		attempts: u8,
		message: String,
		retriable: bool,
	},
}
