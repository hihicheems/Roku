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

use roku_plugin_llm::ToolDefinition;
use roku_plugin_tools::{
	PSEUDO_AGENT, PSEUDO_ASK_USER, PSEUDO_FAIL, PSEUDO_FINAL_ANSWER, ResourceCatalog, TOOL_BASH,
	TOOL_EDIT, TOOL_EXISTS, TOOL_FIND, TOOL_GLOB, TOOL_GREP, TOOL_INSPECT, TOOL_LISTDIR,
	TOOL_PYTHON, TOOL_READ, TOOL_SKILL_INSTALL, TOOL_TABLE_INSPECT, TOOL_TABLE_PREVIEW,
	TOOL_TABLE_SCHEMA, TOOL_TABLE_SHEETS, TOOL_WEB_FETCH, TOOL_WEB_SEARCH, TOOL_WRITE,
};

/// Read-only path tools that accept a `path` parameter.
const PATH_READ_TOOLS: &[&str] = &[TOOL_EXISTS, TOOL_INSPECT, TOOL_LISTDIR, TOOL_READ];

/// Table tools that accept a `path` parameter.
const TABLE_TOOLS: &[&str] = &[
	TOOL_TABLE_INSPECT,
	TOOL_TABLE_SHEETS,
	TOOL_TABLE_PREVIEW,
	TOOL_TABLE_SCHEMA,
];
use serde_json::{Value, json};

use crate::runtime_loop::ToolObservation;
use crate::runtime_loop::grounding::{
	extract_concrete_path_candidates, extract_concrete_table_path,
	extract_explicit_path_candidates, extract_explicit_python_code, extract_explicit_shell_command,
	extract_fetch_url, extract_glob_pattern, extract_grep_pattern, extract_path_candidates,
	extract_row_limit, extract_sheet_name, extract_skill_source_url, extract_web_query,
};

/// Build native tool definitions from visible tools for the LLM provider.
///
/// Converts CatalogDescriptor data into ToolDefinition objects and appends
/// special pseudo-tools (final_answer, ask_user, fail) so the model can
/// express all NextStepAction variants through native tool_use.
pub(crate) fn build_tool_definitions(
	visible_tools: &[String],
	catalog: Option<&ResourceCatalog>,
) -> Vec<ToolDefinition> {
	let mut definitions = Vec::new();

	// Real tools from catalog.
	if let Some(catalog) = catalog {
		for tool_name in visible_tools {
			let selector = roku_common_types::ResourceSelector::tool(tool_name);
			if let Some(descriptor) = catalog.descriptor(&selector) {
				let parameters = if descriptor.input_schema.is_empty() {
					json!({"type": "object", "properties": {}})
				} else {
					let mut properties = serde_json::Map::new();
					for key in &descriptor.input_schema {
						properties.insert(key.clone(), json!({"type": "string"}));
					}
					json!({
						"type": "object",
						"properties": properties,
						"required": descriptor.input_schema.iter()
							.filter(|key| *key != "cwd" && *key != "timeout_ms")
							.collect::<Vec<_>>(),
					})
				};
				definitions.push(ToolDefinition {
					name: tool_name.clone(),
					description: descriptor.selection_hint.clone(),
					parameters,
				});
			}
		}
	}

	// Special pseudo-tools for non-tool actions.
	definitions.push(ToolDefinition {
		name: PSEUDO_FINAL_ANSWER.to_string(),
		description: "Provide the final answer to the user when the task is complete.".to_string(),
		parameters: json!({
			"type": "object",
			"properties": {
				"message": {"type": "string", "description": "The final response message to the user."}
			},
			"required": ["message"]
		}),
	});
	definitions.push(ToolDefinition {
		name: PSEUDO_ASK_USER.to_string(),
		description: "Ask the user for clarification or additional information.".to_string(),
		parameters: json!({
			"type": "object",
			"properties": {
				"question": {"type": "string", "description": "The question to ask the user."}
			},
			"required": ["question"]
		}),
	});
	definitions.push(ToolDefinition {
		name: PSEUDO_FAIL.to_string(),
		description: "Report that the task cannot be completed.".to_string(),
		parameters: json!({
			"type": "object",
			"properties": {
				"reason": {"type": "string", "description": "Why the task cannot be completed."}
			},
			"required": ["reason"]
		}),
	});
	definitions.push(ToolDefinition {
		name: PSEUDO_AGENT.to_string(),
		description: "Spawn a sub-agent to handle a complex sub-task independently. Use when the task can be decomposed into parallel or isolated work. The sub-agent has independent message history and returns a compacted result. Sub-agents cannot spawn further sub-agents.".to_string(),
		parameters: json!({
			"type": "object",
			"properties": {
				"task": {"type": "string", "description": "The prompt/goal for the sub-agent."},
				"tools": {"type": "string", "description": "Optional comma-separated tool names the sub-agent can use. Defaults to all available tools."}
			},
			"required": ["task"]
		}),
	});

	definitions
}

pub(crate) fn ground_tool_arguments(tool_name: &str, grounding_input: &str) -> Option<Value> {
	if PATH_READ_TOOLS.contains(&tool_name) {
		extract_concrete_path_candidates(grounding_input)
			.into_iter()
			.next()
			.map(|path| json!({ "path": path }))
	} else if tool_name == TOOL_FIND {
		extract_explicit_path_candidates(grounding_input)
			.into_iter()
			.next()
			.map(|name| json!({ "name": name, "kind": "any" }))
	} else if tool_name == TOOL_GLOB {
		extract_glob_pattern(grounding_input).map(|pattern| json!({ "pattern": pattern }))
	} else if tool_name == TOOL_GREP {
		extract_grep_pattern(grounding_input).map(|pattern| json!({ "pattern": pattern }))
	} else if tool_name == TOOL_EDIT || tool_name == TOOL_WRITE {
		extract_concrete_path_candidates(grounding_input)
			.into_iter()
			.next()
			.map(|path| json!({ "file_path": path }))
	} else if TABLE_TOOLS.contains(&tool_name) {
		let path = extract_concrete_table_path(grounding_input)?;
		let mut arguments = json!({ "path": path });
		if tool_name == TOOL_TABLE_PREVIEW {
			arguments["rows"] = Value::from(extract_row_limit(grounding_input).unwrap_or(5_u64));
		}
		if let Some(sheet) = extract_sheet_name(grounding_input) {
			arguments["sheet"] = Value::String(sheet);
		}
		Some(arguments)
	} else if tool_name == TOOL_WEB_SEARCH {
		extract_web_query(grounding_input).map(|query| json!({ "query": query, "top_k": 5_u64 }))
	} else if tool_name == TOOL_WEB_FETCH {
		extract_fetch_url(grounding_input).map(|url| json!({ "url": url }))
	} else if tool_name == TOOL_BASH {
		extract_explicit_shell_command(grounding_input).map(|command| json!({ "command": command }))
	} else if tool_name == TOOL_PYTHON {
		extract_explicit_python_code(grounding_input).map(|code| json!({ "code": code }))
	} else if tool_name == TOOL_SKILL_INSTALL {
		extract_skill_source_url(grounding_input)
			.map(|source_url| json!({ "source_url": source_url }))
	} else {
		None
	}
}

pub(crate) fn next_working_directory_from_observation(
	observation: &ToolObservation,
	current_working_directory: &str,
) -> Option<String> {
	if observation.ok && observation.tool_name == TOOL_INSPECT {
		let is_directory = observation
			.data
			.get("kind")
			.and_then(Value::as_str)
			.is_some_and(|kind| kind == "directory");
		if is_directory {
			return observation
				.data
				.get("path")
				.and_then(Value::as_str)
				.filter(|path| !path.is_empty() && *path != current_working_directory)
				.map(str::to_string);
		}
	}
	None
}

pub(crate) fn attachments_for_tool(
	tool_name: &str,
	grounding_input: &str,
) -> Vec<std::path::PathBuf> {
	if tool_name == TOOL_PYTHON {
		extract_path_candidates(grounding_input)
			.into_iter()
			.map(std::path::PathBuf::from)
			.collect()
	} else {
		Vec::new()
	}
}
