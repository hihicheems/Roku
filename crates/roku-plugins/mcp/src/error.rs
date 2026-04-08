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
	#[error("mcp connection failed: {0}")]
	ConnectionFailed(String),
}
