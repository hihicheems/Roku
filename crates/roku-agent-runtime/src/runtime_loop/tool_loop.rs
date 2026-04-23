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
	PSEUDO_AGENT, PSEUDO_ASK_USER, PSEUDO_FAIL, PSEUDO_FINAL_ANSWER, PSEUDO_TASK_CREATE,
	PSEUDO_TASK_GET, PSEUDO_TASK_LIST, PSEUDO_TASK_UPDATE, PSEUDO_TOOL_SEARCH, ResourceCatalog,
	TOOL_BASH, TOOL_EDIT, TOOL_EXISTS, TOOL_FIND, TOOL_GLOB, TOOL_GREP, TOOL_INSPECT, TOOL_LISTDIR,
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
	disallowed_tools: &[String],
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

	// Task management pseudo-tools.
	definitions.push(ToolDefinition {
		name: PSEUDO_TASK_CREATE.to_string(),
		description: "Create a new task for tracking progress. Returns the task ID.".to_string(),
		parameters: json!({
			"type": "object",
			"properties": {
				"description": {"type": "string", "description": "What needs to be done."},
				"status": {"type": "string", "description": "Initial status: pending, in_progress, completed, failed. Defaults to pending."}
			},
			"required": ["description"]
		}),
	});
	definitions.push(ToolDefinition {
		name: PSEUDO_TASK_UPDATE.to_string(),
		description: "Update an existing task's status and/or output.".to_string(),
		parameters: json!({
			"type": "object",
			"properties": {
				"task_id": {"type": "string", "description": "The task ID to update."},
				"status": {"type": "string", "description": "New status: pending, in_progress, completed, failed."},
				"output": {"type": "string", "description": "Optional output or result message."}
			},
			"required": ["task_id"]
		}),
	});
	definitions.push(ToolDefinition {
		name: PSEUDO_TASK_LIST.to_string(),
		description: "List all tracked tasks with their status.".to_string(),
		parameters: json!({
			"type": "object",
			"properties": {}
		}),
	});
	definitions.push(ToolDefinition {
		name: PSEUDO_TASK_GET.to_string(),
		description: "Get details of a specific task by ID.".to_string(),
		parameters: json!({
			"type": "object",
			"properties": {
				"task_id": {"type": "string", "description": "The task ID to retrieve."}
			},
			"required": ["task_id"]
		}),
	});

	// Remove any pseudo-tools that appear in the disallowed list.
	if !disallowed_tools.is_empty() {
		definitions.retain(|d| !disallowed_tools.iter().any(|blocked| blocked == &d.name));
	}

	definitions
}

/// Default fraction of the context window that tool schemas may occupy before
/// deferred mode activates.
pub(crate) const DEFAULT_DEFERRED_TOOL_SCHEMA_THRESHOLD: f64 = 0.10;

/// Tools that are never deferred — they must always have full schemas in every
/// LLM request regardless of schema size.
const CORE_TOOLS: &[&str] = &[
	TOOL_BASH,
	TOOL_READ,
	TOOL_WRITE,
	TOOL_EDIT,
	TOOL_GLOB,
	TOOL_GREP,
	PSEUDO_FINAL_ANSWER,
	PSEUDO_ASK_USER,
	PSEUDO_FAIL,
	PSEUDO_AGENT,
	PSEUDO_TASK_CREATE,
	PSEUDO_TASK_UPDATE,
	PSEUDO_TASK_LIST,
	PSEUDO_TASK_GET,
];

/// Check whether deferred mode should activate and apply it.
///
/// Returns the (potentially filtered) tool definitions and updates the
/// deferred state on `loop_state`. When the total estimated schema tokens
/// exceed `context_window_tokens * DEFAULT_DEFERRED_TOOL_SCHEMA_THRESHOLD`,
/// non-core tools are removed and a `tool_search` pseudo-tool is appended
/// so the model can request individual schemas on demand.
///
/// Once a tool has been loaded via `tool_search`, it remains loaded for the
/// rest of the session (monotonic — no unloading).
pub(crate) fn apply_deferred_mode(
	definitions: Vec<ToolDefinition>,
	loop_state: &mut super::loop_state::LoopState,
	context_window_tokens: u64,
) -> Vec<ToolDefinition> {
	let threshold = (context_window_tokens as f64 * DEFAULT_DEFERRED_TOOL_SCHEMA_THRESHOLD) as u64;

	// Estimate total schema tokens from all definitions.
	let total_schema_tokens: u64 = definitions
		.iter()
		.map(|d| {
			let name_bytes = d.name.len() as u64;
			let desc_bytes = d.description.len() as u64;
			let params_bytes = d.parameters.to_string().len() as u64;
			// name/desc: bytes→chars (÷4 for token estimate); params JSON: ÷2 (structured)
			name_bytes.div_ceil(4) + desc_bytes.div_ceil(4) + params_bytes.div_ceil(2)
		})
		.sum();

	if total_schema_tokens <= threshold {
		// Below threshold — deferred mode not needed.
		// Preserve loaded_names so already-loaded tools stay visible if the
		// schema grows above threshold again later in the session. Clear
		// deferred_names since nothing is currently deferred.
		if let Some(ref mut state) = loop_state.deferred_tools {
			state.deferred_names.clear();
		}
		return definitions;
	}

	// Above threshold: enter or continue deferred mode.
	let deferred = loop_state
		.deferred_tools
		.get_or_insert_with(super::loop_state::DeferredToolState::default);

	let mut result = Vec::new();
	let mut new_deferred_names = Vec::new();

	for def in &definitions {
		let is_core = CORE_TOOLS.contains(&def.name.as_str());
		let is_loaded = deferred.loaded_names.contains(&def.name);

		if is_core || is_loaded {
			result.push(def.clone());
		} else {
			new_deferred_names.push(def.name.clone());
		}
	}

	deferred.deferred_names = new_deferred_names;

	// Append the ToolSearch pseudo-tool when there are deferred schemas.
	if !deferred.deferred_names.is_empty() {
		let deferred_list = deferred.deferred_names.join(", ");
		result.push(ToolDefinition {
			name: PSEUDO_TOOL_SEARCH.to_string(),
			description: format!(
				"Search for and load a deferred tool's schema. \
				 Available deferred tools: {deferred_list}. \
				 Call this with the tool name to load its full schema for the next turn.",
			),
			parameters: serde_json::json!({
				"type": "object",
				"properties": {
					"tool_name": {
						"type": "string",
						"description": "The name of the deferred tool to load."
					}
				},
				"required": ["tool_name"]
			}),
		});
	}

	result
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

#[cfg(test)]
mod tests {
	use super::{apply_deferred_mode, build_tool_definitions};
	use crate::runtime_loop::loop_state::DeferredToolState;
	use roku_plugin_llm::ToolDefinition;

	fn test_loop_state() -> crate::runtime_loop::LoopState {
		use crate::router::{IntentFamily, RouteDecision, RouteRisk};
		use crate::runtime_loop::LoopContext;
		let ctx = LoopContext {
			request_id: "req-test".to_string(),
			session_id: "session-test".to_string(),
			goal: "test".to_string(),
			workspace_root: "/workspace".to_string(),
			working_directory: "/workspace".to_string(),
			visible_tools: Vec::new(),
			bound_resources: Vec::new(),
			route_decision: RouteDecision::new(
				IntentFamily::Chat,
				0.9,
				false,
				RouteRisk::Low,
				Vec::new(),
				Vec::new(),
				Vec::new(),
				"test",
			),
			last_observation: None,
		};
		crate::runtime_loop::LoopState::new("test", &ctx)
	}

	fn fat_tool(name: &str) -> ToolDefinition {
		ToolDefinition {
			name: name.to_string(),
			description: "A".repeat(500),
			parameters: serde_json::json!({
				"type": "object",
				"properties": {
					"arg1": {"type": "string", "description": "B".repeat(500)}
				}
			}),
		}
	}

	/// Byte-stability guard: repeated calls with identical inputs must emit
	/// the same serialized bytes. This is the foundation that 09 / 10 rely
	/// on — a cached tool block is only usable if its bytes do not drift
	/// across turns.
	#[test]
	fn build_tool_definitions_is_serialization_deterministic() {
		let visible: Vec<String> = Vec::new();
		let disallowed: Vec<String> = Vec::new();

		let first = build_tool_definitions(&visible, None, &disallowed);
		let second = build_tool_definitions(&visible, None, &disallowed);

		let first_bytes = serde_json::to_vec(&first).expect("serialize tool defs");
		let second_bytes = serde_json::to_vec(&second).expect("serialize tool defs");

		assert_eq!(
			first_bytes, second_bytes,
			"pseudo-tool schema must be byte-stable across calls"
		);
		assert!(!first.is_empty(), "pseudo-tools should be present");
	}

	#[test]
	fn build_tool_definitions_disallowed_removes_tool_deterministically() {
		let visible: Vec<String> = Vec::new();
		let disallowed: Vec<String> = vec!["ask_user".to_string()];

		let first = build_tool_definitions(&visible, None, &disallowed);
		let second = build_tool_definitions(&visible, None, &disallowed);

		let first_bytes = serde_json::to_vec(&first).expect("serialize tool defs");
		let second_bytes = serde_json::to_vec(&second).expect("serialize tool defs");

		assert_eq!(first_bytes, second_bytes);
		assert!(
			!first.iter().any(|d| d.name == "ask_user"),
			"disallowed entry should not appear in the output"
		);
	}

	#[test]
	fn deferred_mode_activates_above_threshold() {
		// Build 50 fat non-core tools that will push schema tokens past 10% of
		// 200_000 (threshold = 20_000 tokens).
		let mut definitions: Vec<ToolDefinition> = (0..50)
			.map(|i| fat_tool(&format!("CustomTool{i}")))
			.collect();
		definitions.push(ToolDefinition {
			name: "Bash".to_string(),
			description: "Run bash".to_string(),
			parameters: serde_json::json!({"type": "object", "properties": {}}),
		});

		let mut loop_state = test_loop_state();
		let result = apply_deferred_mode(definitions, &mut loop_state, 200_000);

		// Core tool must be present.
		assert!(
			result.iter().any(|d| d.name == "Bash"),
			"Bash must be present"
		);
		// ToolSearch pseudo-tool must have been injected.
		assert!(
			result.iter().any(|d| d.name == "tool_search"),
			"tool_search must be present when deferred mode activates"
		);
		// Non-core custom tools must be absent (deferred).
		assert!(
			!result.iter().any(|d| d.name == "CustomTool0"),
			"CustomTool0 must be deferred (not in result)"
		);
		// Deferred state must be set.
		assert!(
			loop_state.deferred_tools.is_some(),
			"deferred_tools must be populated"
		);
		let deferred = loop_state.deferred_tools.as_ref().unwrap();
		assert!(
			deferred.deferred_names.contains(&"CustomTool0".to_string()),
			"CustomTool0 must appear in deferred_names"
		);
	}

	#[test]
	fn deferred_mode_does_not_activate_below_threshold() {
		// A single small tool — well below 10% of 200_000 token threshold.
		let definitions = vec![ToolDefinition {
			name: "Bash".to_string(),
			description: "Run bash".to_string(),
			parameters: serde_json::json!({"type": "object", "properties": {}}),
		}];

		let mut loop_state = test_loop_state();
		let result = apply_deferred_mode(definitions.clone(), &mut loop_state, 200_000);

		assert_eq!(
			result.len(),
			1,
			"no deferred mode — result must equal input"
		);
		assert!(
			loop_state.deferred_tools.is_none(),
			"deferred_tools must be None below threshold"
		);
	}

	#[test]
	fn apply_deferred_mode_is_idempotent_across_repeated_calls() {
		// `maybe_compact`'s end-of-turn byte rebuild calls `apply_deferred_mode`
		// on the frozen (pre-deferred) snapshot after the pre-flight already
		// applied it once. This invariant requires idempotency: a second
		// pass on the same definitions with the same LoopState must produce
		// the same output as the first pass. Otherwise the post-tool byte
		// estimate would diverge from what the next outbound call actually
		// sends, over- or under-estimating prompt pressure.
		let mut definitions: Vec<ToolDefinition> = (0..50)
			.map(|i| fat_tool(&format!("CustomTool{i}")))
			.collect();
		definitions.push(ToolDefinition {
			name: "Bash".to_string(),
			description: "Run bash".to_string(),
			parameters: serde_json::json!({"type": "object", "properties": {}}),
		});

		let mut loop_state = test_loop_state();
		let first = apply_deferred_mode(definitions.clone(), &mut loop_state, 200_000);
		let second = apply_deferred_mode(definitions.clone(), &mut loop_state, 200_000);

		let first_bytes = serde_json::to_vec(&first).expect("serialize first");
		let second_bytes = serde_json::to_vec(&second).expect("serialize second");
		assert_eq!(
			first_bytes, second_bytes,
			"apply_deferred_mode output must be byte-identical across repeated calls \
			 with the same inputs — otherwise the post-tool rebuild in maybe_compact \
			 would disagree with the pre-flight snapshot"
		);

		// Sanity: deferred state after the second call is structurally unchanged.
		let deferred = loop_state
			.deferred_tools
			.as_ref()
			.expect("deferred state populated");
		assert!(
			!deferred.deferred_names.is_empty(),
			"non-core tools should be in deferred_names"
		);
		assert!(
			deferred.loaded_names.is_empty(),
			"loaded_names is not mutated by apply_deferred_mode — only by tool_search"
		);
	}

	#[test]
	fn apply_deferred_mode_on_frozen_snapshot_matches_fresh_build() {
		// `maybe_compact`'s non-dirty branch pulls the frozen (pre-deferred)
		// snapshot out of `loop_state.frozen_tool_schema` and re-runs
		// `apply_deferred_mode` on it. The result must match what the dirty
		// branch produces from a fresh `build_tool_definitions` + full
		// `apply_deferred_mode` pipeline. This test locks that symmetry so
		// the non-dirty path does not under-count by skipping the deferred
		// filter (the regression `8a21e8d` introduced before this fix).

		// "Frozen snapshot" surrogate: a pre-deferred definition set.
		let frozen_snapshot: Vec<ToolDefinition> = {
			let mut defs: Vec<ToolDefinition> = (0..50)
				.map(|i| fat_tool(&format!("CustomTool{i}")))
				.collect();
			defs.push(ToolDefinition {
				name: "Bash".to_string(),
				description: "Run bash".to_string(),
				parameters: serde_json::json!({"type": "object", "properties": {}}),
			});
			defs
		};

		// "Fresh build" surrogate: the same definitions (same inputs would
		// yield the same output from `build_tool_definitions` in production).
		let fresh_build = frozen_snapshot.clone();

		let mut state_non_dirty = test_loop_state();
		let non_dirty_result = apply_deferred_mode(frozen_snapshot, &mut state_non_dirty, 200_000);

		let mut state_dirty = test_loop_state();
		let dirty_result = apply_deferred_mode(fresh_build, &mut state_dirty, 200_000);

		let non_dirty_bytes = serde_json::to_vec(&non_dirty_result).expect("serialize non-dirty");
		let dirty_bytes = serde_json::to_vec(&dirty_result).expect("serialize dirty");
		assert_eq!(
			non_dirty_bytes, dirty_bytes,
			"both branches must produce the same effective-schema bytes when given \
			 the same pre-deferred input"
		);
		// Both must carry the tool_search stub — proving neither branch
		// accidentally returns the raw pre-deferred set.
		assert!(
			non_dirty_result.iter().any(|d| d.name == "tool_search"),
			"tool_search must appear in the non-dirty branch output"
		);
		assert!(
			dirty_result.iter().any(|d| d.name == "tool_search"),
			"tool_search must appear in the dirty branch output"
		);
	}

	#[test]
	fn loaded_tools_stay_loaded() {
		// Pre-seed deferred state with MyTool already loaded.
		let mut loop_state = test_loop_state();
		loop_state.deferred_tools = Some(DeferredToolState {
			deferred_names: vec!["MyTool".to_string()],
			loaded_names: vec!["MyTool".to_string()],
		});

		// Build definitions with 50 fat custom tools + MyTool + Bash.
		let mut definitions: Vec<ToolDefinition> = (0..50)
			.map(|i| fat_tool(&format!("CustomTool{i}")))
			.collect();
		definitions.push(ToolDefinition {
			name: "MyTool".to_string(),
			description: "Already loaded".to_string(),
			parameters: serde_json::json!({"type": "object", "properties": {}}),
		});
		definitions.push(ToolDefinition {
			name: "Bash".to_string(),
			description: "Run bash".to_string(),
			parameters: serde_json::json!({"type": "object", "properties": {}}),
		});

		let result = apply_deferred_mode(definitions, &mut loop_state, 200_000);

		// MyTool must still be present because it was previously loaded.
		assert!(
			result.iter().any(|d| d.name == "MyTool"),
			"MyTool must stay loaded (monotonic)"
		);
		// Core tool must be present.
		assert!(
			result.iter().any(|d| d.name == "Bash"),
			"Bash must be present"
		);
		// Non-loaded custom tools must still be deferred.
		assert!(
			!result.iter().any(|d| d.name == "CustomTool0"),
			"CustomTool0 must remain deferred"
		);
	}
}
