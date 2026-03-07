//! Tool runtime with descriptor-based dispatch and execution policies.

mod descriptor;
mod error;
mod event;
mod runtime;
#[cfg(test)]
mod tests;

pub use descriptor::{RuntimeConstraints, SandboxProfile, ToolDescriptor, ToolSchema};
pub use error::{ToolFailure, ToolRuntimeError};
pub use event::{ExecutionEvent, ExecutionEventKind, ExecutionHook};
pub use runtime::{Tool, ToolExecutionResult, ToolInvocation, ToolInvocationRequest, ToolRuntime};
