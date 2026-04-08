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

//! E2E integration tests for MCP client using `@modelcontextprotocol/server-filesystem`.
//!
//! All tests are `#[ignore]` because they require Node.js/npx on the host.
//! Run with: `cargo test -p roku-plugin-mcp -- --ignored`

use std::sync::Arc;

use roku_plugin_host::Tool;
use roku_plugin_mcp::{
	McpConnection, McpServerConfig, McpTool, mcp_tool_name, mcp_tools_to_catalog_descriptors,
};
use serde_json::json;
use tempfile::TempDir;

/// Build config for the filesystem MCP server.
/// Uses canonicalized path to avoid macOS `/var` → `/private/var` symlink mismatch.
fn filesystem_config(canonical_dir: &str) -> McpServerConfig {
	McpServerConfig {
		name: "filesystem".to_string(),
		command: "npx".to_string(),
		args: vec![
			"-y".to_string(),
			"@modelcontextprotocol/server-filesystem".to_string(),
			canonical_dir.to_string(),
		],
		env: Default::default(),
	}
}

/// Create a temp dir with a known file, return (TempDir, canonical_path).
fn setup_test_dir() -> (TempDir, String) {
	let dir = TempDir::new().expect("create temp dir");
	std::fs::write(dir.path().join("hello.txt"), "Hello from MCP E2E test!")
		.expect("write test file");
	let canonical = dir
		.path()
		.canonicalize()
		.expect("canonicalize")
		.to_str()
		.expect("utf8 path")
		.to_string();
	(dir, canonical)
}

/// Drop McpConnection outside async context to avoid runtime-drop-in-async panic.
async fn cleanup(conn: McpConnection) {
	tokio::task::spawn_blocking(move || drop(conn)).await.ok();
}

// All tests use multi_thread flavor because:
// - call_tool_blocking uses scoped-thread pattern that blocks the calling thread
// - The service's background tasks need a live worker thread to process RPC responses
// - current_thread would deadlock (only thread blocked → no one to process responses)

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn connect_and_list_tools() {
	let (_dir, canonical) = setup_test_dir();
	let config = filesystem_config(&canonical);

	let conn = McpConnection::connect(&config)
		.await
		.expect("connect to filesystem server");

	assert_eq!(conn.server_name(), "filesystem");

	let tools = conn.list_tools().await.expect("list tools");
	assert!(!tools.is_empty(), "filesystem server should expose tools");

	let tool_names: Vec<String> = tools.iter().map(|t| t.name.to_string()).collect();
	assert!(
		tool_names.contains(&"read_file".to_string()),
		"expected read_file tool, got: {:?}",
		tool_names
	);
	assert!(
		tool_names.contains(&"list_directory".to_string()),
		"expected list_directory tool, got: {:?}",
		tool_names
	);

	cleanup(conn).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn call_read_file() {
	let (_dir, canonical) = setup_test_dir();
	let file_path = format!("{}/hello.txt", canonical);
	let config = filesystem_config(&canonical);

	let conn = McpConnection::connect(&config)
		.await
		.expect("connect to filesystem server");

	let result = conn
		.call_tool("read_file", json!({ "path": file_path }))
		.await
		.expect("call read_file");

	assert_ne!(result.is_error, Some(true), "read_file should succeed");

	let text: String = result
		.content
		.iter()
		.filter_map(|c| c.as_text().map(|t| t.text.to_string()))
		.collect();
	assert!(
		text.contains("Hello from MCP E2E test!"),
		"expected test content, got: {}",
		text
	);

	cleanup(conn).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn call_list_directory() {
	let (_dir, canonical) = setup_test_dir();
	let config = filesystem_config(&canonical);

	let conn = McpConnection::connect(&config)
		.await
		.expect("connect to filesystem server");

	let result = conn
		.call_tool("list_directory", json!({ "path": &canonical }))
		.await
		.expect("call list_directory");

	assert_ne!(result.is_error, Some(true), "list_directory should succeed");

	let text: String = result
		.content
		.iter()
		.filter_map(|c| c.as_text().map(|t| t.text.to_string()))
		.collect();
	assert!(
		text.contains("hello.txt"),
		"expected hello.txt in listing, got: {}",
		text
	);

	cleanup(conn).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn call_tool_blocking_bridge() {
	let (_dir, canonical) = setup_test_dir();
	let file_path = format!("{}/hello.txt", canonical);
	let config = filesystem_config(&canonical);

	let conn = McpConnection::connect(&config)
		.await
		.expect("connect to filesystem server");

	let result = conn
		.call_tool_blocking("read_file", json!({ "path": file_path }))
		.expect("call_tool_blocking should work from within tokio");

	assert_ne!(result.is_error, Some(true));
	let text: String = result
		.content
		.iter()
		.filter_map(|c| c.as_text().map(|t| t.text.to_string()))
		.collect();
	assert!(text.contains("Hello from MCP E2E test!"));

	cleanup(conn).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn mcp_tool_wrapper_invoke() {
	let (_dir, canonical) = setup_test_dir();
	let file_path = format!("{}/hello.txt", canonical);
	let config = filesystem_config(&canonical);

	let conn = Arc::new(
		McpConnection::connect(&config)
			.await
			.expect("connect to filesystem server"),
	);

	let tools = conn.list_tools().await.expect("list tools");
	let read_file_meta = tools
		.iter()
		.find(|t| t.name.as_ref() == "read_file")
		.expect("read_file tool should exist")
		.clone();

	let mcp_tool = McpTool::new(conn.clone(), read_file_meta);

	// Verify descriptor
	let descriptor = mcp_tool.descriptor();
	assert_eq!(descriptor.name, mcp_tool_name("filesystem", "read_file"));
	assert!(!descriptor.input_schema.required_fields.is_empty());

	// Verify invoke (sync bridge via Tool trait)
	let request = roku_plugin_host::ToolInvocationRequest {
		invocation_key: "test".to_string(),
		attempt: 0,
		input: json!({ "path": file_path }),
		sandbox_profile: roku_plugin_host::SandboxProfile::NoIsolation,
		attachments: vec![],
		allowed_read_roots: vec![],
		allowed_write_roots: vec![],
	};
	let result = mcp_tool.invoke(request).expect("invoke should succeed");
	let text = match &result {
		serde_json::Value::String(s) => s.clone(),
		other => other.to_string(),
	};
	assert!(
		text.contains("Hello from MCP E2E test!"),
		"invoke result: {:?}",
		result
	);

	// Drop both mcp_tool (holds Arc<McpConnection>) and conn in blocking context
	// to avoid runtime-drop-in-async panic.
	tokio::task::spawn_blocking(move || {
		drop(mcp_tool);
		drop(conn);
	})
	.await
	.ok();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn catalog_descriptor_conversion() {
	let (_dir, canonical) = setup_test_dir();
	let config = filesystem_config(&canonical);

	let conn = McpConnection::connect(&config)
		.await
		.expect("connect to filesystem server");

	let tools = conn.list_tools().await.expect("list tools");
	let descriptors = mcp_tools_to_catalog_descriptors("filesystem", &tools);

	assert_eq!(descriptors.len(), tools.len());

	let read_file_desc = descriptors
		.iter()
		.find(|d| d.name == "mcp__filesystem__read_file")
		.expect("catalog descriptor for read_file");

	assert!(read_file_desc.discoverable);
	assert!(read_file_desc.tags.contains(&"mcp".to_string()));
	assert!(read_file_desc.tags.contains(&"filesystem".to_string()));
	assert!(!read_file_desc.description.is_empty());

	cleanup(conn).await;
}
