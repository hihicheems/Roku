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

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Configuration for a single MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
	/// Human-readable server name (used in tool name prefix).
	pub name: String,
	/// Command to spawn the MCP server process.
	pub command: String,
	/// Arguments to pass to the command.
	#[serde(default)]
	pub args: Vec<String>,
	/// Environment variables to set for the server process.
	#[serde(default)]
	pub env: HashMap<String, String>,
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn serde_round_trip_minimal() {
		let config = McpServerConfig {
			name: "my-server".to_string(),
			command: "npx".to_string(),
			args: vec![],
			env: HashMap::new(),
		};

		let json = serde_json::to_string(&config).expect("serialize");
		let restored: McpServerConfig = serde_json::from_str(&json).expect("deserialize");

		assert_eq!(restored.name, config.name);
		assert_eq!(restored.command, config.command);
		assert!(restored.args.is_empty());
		assert!(restored.env.is_empty());
	}

	#[test]
	fn serde_round_trip_full() {
		let mut env = HashMap::new();
		env.insert("API_KEY".to_string(), "secret".to_string());

		let config = McpServerConfig {
			name: "full-server".to_string(),
			command: "node".to_string(),
			args: vec![
				"server.js".to_string(),
				"--port".to_string(),
				"3000".to_string(),
			],
			env,
		};

		let json = serde_json::to_string(&config).expect("serialize");
		let restored: McpServerConfig = serde_json::from_str(&json).expect("deserialize");

		assert_eq!(restored.name, config.name);
		assert_eq!(restored.command, config.command);
		assert_eq!(restored.args, config.args);
		assert_eq!(
			restored.env.get("API_KEY").map(String::as_str),
			Some("secret")
		);
	}

	#[test]
	fn deserialize_defaults_for_optional_fields() {
		let json = r#"{"name": "simple", "command": "echo"}"#;
		let config: McpServerConfig = serde_json::from_str(json).expect("deserialize");

		assert_eq!(config.name, "simple");
		assert_eq!(config.command, "echo");
		assert!(config.args.is_empty());
		assert!(config.env.is_empty());
	}
}
