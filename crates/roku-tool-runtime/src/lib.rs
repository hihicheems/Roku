//! Tool runtime and dispatch.

use std::collections::HashMap;

pub trait Tool {
	fn invoke(&self, input: &str) -> Result<String, String>;
}

#[derive(Debug, Default)]
pub struct EchoTool;

impl Tool for EchoTool {
	fn invoke(&self, input: &str) -> Result<String, String> {
		Ok(input.to_string())
	}
}

#[derive(Default)]
pub struct ToolRuntime {
	tools: HashMap<String, Box<dyn Tool + Send + Sync>>,
}

impl ToolRuntime {
	pub fn register_tool(&mut self, name: impl Into<String>, tool: Box<dyn Tool + Send + Sync>) {
		self.tools.insert(name.into(), tool);
	}

	pub fn invoke(&self, name: &str, input: &str) -> Result<String, String> {
		let tool = self
			.tools
			.get(name)
			.ok_or_else(|| format!("tool not found: {name}"))?;
		tool.invoke(input)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn invoke_registered_tool() {
		let mut runtime = ToolRuntime::default();
		runtime.register_tool("echo", Box::new(EchoTool));
		let output = runtime.invoke("echo", "hello");
		assert_eq!(output, Ok("hello".to_string()));
	}
}
