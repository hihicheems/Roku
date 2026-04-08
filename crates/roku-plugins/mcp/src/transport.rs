// Copyright 2025 itscheems
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::sync::Arc;

use rmcp::{
	RoleClient,
	model::{CallToolRequestParams, CallToolResult},
	serve_client,
	service::RunningService,
	transport::TokioChildProcess,
};
use serde_json::Value;
use tokio::process::Command;

use crate::config::McpServerConfig;
use crate::error::McpError;

/// A live connection to a single MCP server process via stdio.
pub struct McpConnection {
	server_name: String,
	service: RunningService<RoleClient, ()>,
	/// Single-threaded tokio runtime for sync bridging (`Tool::invoke`).
	/// Avoids the "cannot block from within a runtime" panic when
	/// `Handle::current().block_on()` is called inside `block_in_place()`.
	blocking_runtime: Arc<tokio::runtime::Runtime>,
}

impl McpConnection {
	/// Spawn an MCP server process and connect to it via stdio.
	pub async fn connect(config: &McpServerConfig) -> Result<Self, McpError> {
		let mut cmd = Command::new(&config.command);
		for arg in &config.args {
			cmd.arg(arg);
		}
		for (key, value) in &config.env {
			cmd.env(key, value);
		}

		let transport = TokioChildProcess::new(cmd).map_err(|e| {
			McpError::ConnectionFailed(format!("failed to spawn '{}': {}", config.command, e))
		})?;

		let service = serve_client((), transport).await.map_err(|e| {
			McpError::ConnectionFailed(format!(
				"MCP handshake failed for server '{}': {}",
				config.name, e
			))
		})?;

		let blocking_runtime = Arc::new(
			tokio::runtime::Builder::new_current_thread()
				.enable_all()
				.build()
				.map_err(|e| {
					McpError::ConnectionFailed(format!(
						"failed to create blocking runtime for '{}': {}",
						config.name, e
					))
				})?,
		);

		Ok(Self {
			server_name: config.name.clone(),
			service,
			blocking_runtime,
		})
	}

	/// The configured name of this MCP server.
	pub fn server_name(&self) -> &str {
		&self.server_name
	}

	/// List all tools advertised by this server.
	pub async fn list_tools(&self) -> Result<Vec<rmcp::model::Tool>, McpError> {
		self.service
			.peer()
			.list_all_tools()
			.await
			.map_err(|e| McpError::Transport(format!("list_tools failed: {}", e)))
	}

	/// Call a tool by name with the given JSON arguments.
	pub async fn call_tool(
		&self,
		name: &str,
		arguments: Value,
	) -> Result<CallToolResult, McpError> {
		let params = build_call_params(name, arguments)?;
		self.service
			.peer()
			.call_tool(params)
			.await
			.map_err(|e| McpError::Transport(format!("call_tool '{}' failed: {}", name, e)))
	}

	/// Synchronous bridge for `call_tool`, safe to call from inside a tokio
	/// runtime (e.g. within `block_in_place`).
	///
	/// Follows the same scoped-thread pattern as `LlmRouter::generate_blocking`.
	pub fn call_tool_blocking(
		&self,
		name: &str,
		arguments: Value,
	) -> Result<CallToolResult, McpError> {
		let params = build_call_params(name, arguments)?;
		let fut = self.service.peer().call_tool(params);
		let rt = self.blocking_runtime.clone();

		if tokio::runtime::Handle::try_current().is_ok() {
			std::thread::scope(|s| {
				s.spawn(|| {
					rt.block_on(fut).map_err(|e| {
						McpError::Transport(format!("call_tool '{}' failed: {}", name, e))
					})
				})
				.join()
				.unwrap()
			})
		} else {
			rt.block_on(fut)
				.map_err(|e| McpError::Transport(format!("call_tool '{}' failed: {}", name, e)))
		}
	}
}

fn build_call_params(name: &str, arguments: Value) -> Result<CallToolRequestParams, McpError> {
	let args_map = match arguments {
		Value::Object(map) => Some(map),
		Value::Null => None,
		other => {
			return Err(McpError::InvalidRequest(format!(
				"tool arguments must be a JSON object or null, got: {}",
				other
			)));
		}
	};

	let mut params = CallToolRequestParams::new(name.to_string());
	if let Some(map) = args_map {
		params = params.with_arguments(map);
	}
	Ok(params)
}
