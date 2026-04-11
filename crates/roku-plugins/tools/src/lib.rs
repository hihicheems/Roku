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

//! Builtin tool catalog and runtime builders for Roku plugins.

use std::collections::BTreeSet;
use std::sync::LazyLock;

// Standard tag constants for tool metadata.
pub const TAG_CATEGORY_FILESYSTEM: &str = "category:filesystem";
pub const TAG_CATEGORY_SHELL: &str = "category:shell";
pub const TAG_CATEGORY_WEB: &str = "category:web";
pub const TAG_CATEGORY_TABLE: &str = "category:table";
pub const TAG_CATEGORY_PYTHON: &str = "category:python";
pub const TAG_CATEGORY_SKILL: &str = "category:skill";
pub const TAG_CATEGORY_META: &str = "category:meta";

pub const TAG_RISK_SAFE: &str = "risk:safe";
pub const TAG_RISK_WRITE: &str = "risk:write";

// Tool name constants — import instead of using string literals.
pub const TOOL_BASH: &str = "Bash";
pub const TOOL_READ: &str = "Read";
pub const TOOL_WRITE: &str = "Write";
pub const TOOL_EDIT: &str = "Edit";
pub const TOOL_GLOB: &str = "Glob";
pub const TOOL_GREP: &str = "Grep";
pub const TOOL_FIND: &str = "Find";
pub const TOOL_EXISTS: &str = "Exists";
pub const TOOL_INSPECT: &str = "Inspect";
pub const TOOL_LISTDIR: &str = "ListDir";
pub const TOOL_PYTHON: &str = "Python";
pub const TOOL_WEB_SEARCH: &str = "WebSearch";
pub const TOOL_WEB_FETCH: &str = "WebFetch";
pub const TOOL_TABLE_INSPECT: &str = "TableInspect";
pub const TOOL_TABLE_SHEETS: &str = "TableSheets";
pub const TOOL_TABLE_PREVIEW: &str = "TablePreview";
pub const TOOL_TABLE_SCHEMA: &str = "TableSchema";
pub const TOOL_SKILL_INSTALL: &str = "SkillInstall";
pub const TOOL_SKILL_RUN: &str = "SkillRun";

// Pseudo-tool name constants.
pub const PSEUDO_FINAL_ANSWER: &str = "final_answer";
pub const PSEUDO_ASK_USER: &str = "ask_user";
pub const PSEUDO_FAIL: &str = "fail";
pub const PSEUDO_AGENT: &str = "Agent";

mod availability;
mod builders;
mod builtin;
mod config;
mod contract;
mod runtime_config;

use roku_common_types::CanonicalExecution;
use serde_json::Value;

pub use availability::{
	RuntimeVisibleToolAvailabilitySnapshot, build_runtime_visible_tool_availability_snapshot,
};
pub use builders::{
	build_builtin_tool_runtime, build_builtin_tool_runtime_with_plugin_snapshot,
	build_builtin_tool_runtime_with_plugin_snapshot_and_runtime_capabilities,
	build_builtin_tool_runtime_with_plugin_snapshot_and_runtime_capabilities_and_runtime_config,
	build_builtin_tool_runtime_with_plugin_snapshot_and_runtime_config, build_llm_tool_runtime,
	build_llm_tool_runtime_with_plugin_snapshot,
	build_llm_tool_runtime_with_plugin_snapshot_and_runtime_config, build_resource_catalog,
	build_resource_catalog_with_plugin_snapshot,
	build_resource_catalog_with_plugin_snapshot_and_runtime_capabilities,
	build_resource_catalog_with_plugin_snapshot_and_runtime_capabilities_and_runtime_config,
	build_resource_catalog_with_plugin_snapshot_and_runtime_config,
};
pub use config::{BuiltinToolRole, ConfiguredTool, ToolCatalogConfig, ToolCatalogConfigError};
pub use roku_common_types::{
	CatalogDescriptor, CatalogMatch, ResourceCatalog, ResourceCost, ResourceKind, ResourceRisk,
};
pub use runtime_config::{
	CommandToolRuntimeConfig, CommandToolRuntimeConfigPatch, FsToolRuntimeConfig,
	FsToolRuntimeConfigPatch, PythonToolRuntimeConfig, PythonToolRuntimeConfigPatch,
	TableToolRuntimeConfig, TableToolRuntimeConfigPatch, ToolWorkerRuntimeConfig,
	ToolWorkerRuntimeConfigPatch, ToolsRuntimeConfig, ToolsRuntimeConfigError,
	ToolsRuntimeConfigPatch, WebToolRuntimeConfig, WebToolRuntimeConfigPatch,
};

static BUILTIN_TOOL_NAMES: LazyLock<BTreeSet<String>> = LazyLock::new(|| {
	let mut names = ToolCatalogConfig::default()
		.tools
		.into_iter()
		.map(|tool| tool.name)
		.collect::<BTreeSet<_>>();
	names.extend(
		[
			TOOL_BASH,
			TOOL_EDIT,
			TOOL_EXISTS,
			TOOL_FIND,
			TOOL_GLOB,
			TOOL_INSPECT,
			TOOL_LISTDIR,
			TOOL_READ,
			TOOL_WRITE,
			TOOL_GREP,
			TOOL_PYTHON,
			TOOL_TABLE_INSPECT,
			TOOL_TABLE_SHEETS,
			TOOL_TABLE_PREVIEW,
			TOOL_TABLE_SCHEMA,
			TOOL_WEB_FETCH,
			TOOL_WEB_SEARCH,
		]
		.into_iter()
		.map(str::to_string),
	);
	names
});

pub fn builtin_tool_names() -> &'static BTreeSet<String> {
	&BUILTIN_TOOL_NAMES
}

pub fn is_builtin_tool_name(tool_name: &str) -> bool {
	builtin_tool_names().contains(tool_name)
}

pub fn canonical_execution_for_builtin_tool_input(
	tool_name: &str,
	input: &Value,
) -> Option<CanonicalExecution> {
	if tool_name == TOOL_BASH {
		builtin::command::canonical_execution_from_runtime_input(input)
	} else if tool_name == TOOL_EXISTS
		|| tool_name == TOOL_INSPECT
		|| tool_name == TOOL_LISTDIR
		|| tool_name == TOOL_READ
		|| tool_name == TOOL_EDIT
		|| tool_name == TOOL_WRITE
	{
		builtin::fs::canonical_execution_from_runtime_input(tool_name, input)
	} else {
		None
	}
}
