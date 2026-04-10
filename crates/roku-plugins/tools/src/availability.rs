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

use std::collections::BTreeSet;

use roku_common_types::{ResourceCatalog, ResourceKind};
use serde::{Deserialize, Serialize};

/// Shared runtime-visible availability truth for route seeding and loop initialization.
///
/// The route classifier only needs to know which tool names are currently enabled before it
/// shortlists them. The runtime loop additionally needs a filtered safe-baseline seed for
/// `visible_tools`. This snapshot keeps both answers aligned so later consumers can stop
/// rebuilding separate availability views from the catalog.
///
/// Invocation-time deny / approval policy stays outside this snapshot. High-risk tools such as
/// `command.run` remain runtime-visible here whenever they are enabled; the policy bridge is
/// responsible for surfacing `policy_denied` or `approval_required` after invocation is attempted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeVisibleToolAvailabilitySnapshot {
	pub enabled_tools: BTreeSet<String>,
	pub baseline_visible_tools: Vec<String>,
}

impl RuntimeVisibleToolAvailabilitySnapshot {
	pub fn from_resource_catalog(
		resource_catalog: &ResourceCatalog,
		baseline_tool_names: &[&str],
	) -> Self {
		let enabled_tools = resource_catalog
			.entries()
			.iter()
			.filter(|entry| entry.kind == ResourceKind::Tool)
			.map(|entry| entry.name.clone())
			.collect::<BTreeSet<_>>();
		let baseline_visible_tools =
			collect_enabled_tool_names(&enabled_tools, baseline_tool_names.iter().copied());

		Self {
			enabled_tools,
			baseline_visible_tools,
		}
	}

	pub fn is_tool_enabled(&self, tool_name: &str) -> bool {
		self.enabled_tools.contains(tool_name)
	}

	pub fn has_enabled_tool_with_prefix(&self, tool_name_prefix: &str) -> bool {
		self.enabled_tools
			.iter()
			.any(|tool_name| tool_name.starts_with(tool_name_prefix))
	}

	pub fn filter_candidate_tools(
		&self,
		preferred_tool: Option<&str>,
		seed_tools: &[String],
	) -> Vec<String> {
		let mut tools = Vec::new();
		if let Some(preferred_tool) = preferred_tool
			&& self.is_tool_enabled(preferred_tool)
		{
			tools.push(preferred_tool.to_string());
		}
		append_enabled_tool_names(
			&mut tools,
			&self.enabled_tools,
			seed_tools.iter().map(String::as_str),
		);
		tools
	}

	/// Returns all enabled tools, with seed tools listed first for ordering/priority.
	///
	/// The `baseline_visible_tools` field is no longer the limiting factor; all enabled tools are
	/// always visible. Seed tools appear at the front of the list; remaining enabled tools follow.
	pub fn compose_visible_tools<'a>(
		&self,
		seed_tool_names: impl IntoIterator<Item = &'a str>,
	) -> Vec<String> {
		let mut visible_tools = Vec::new();
		// Seed tools first (ordering/priority).
		append_enabled_tool_names(&mut visible_tools, &self.enabled_tools, seed_tool_names);
		// Then ALL remaining enabled tools.
		for tool in &self.enabled_tools {
			if !visible_tools.contains(tool) {
				visible_tools.push(tool.clone());
			}
		}
		visible_tools
	}
}

pub fn build_runtime_visible_tool_availability_snapshot(
	resource_catalog: &ResourceCatalog,
	baseline_tool_names: &[&str],
) -> RuntimeVisibleToolAvailabilitySnapshot {
	RuntimeVisibleToolAvailabilitySnapshot::from_resource_catalog(
		resource_catalog,
		baseline_tool_names,
	)
}

fn append_enabled_tool_names<'a>(
	visible_tools: &mut Vec<String>,
	enabled_tools: &BTreeSet<String>,
	tool_names: impl IntoIterator<Item = &'a str>,
) {
	for tool_name in tool_names {
		if enabled_tools.contains(tool_name)
			&& !visible_tools.iter().any(|existing| existing == tool_name)
		{
			visible_tools.push(tool_name.to_string());
		}
	}
}

fn collect_enabled_tool_names<'a>(
	enabled_tools: &BTreeSet<String>,
	tool_names: impl IntoIterator<Item = &'a str>,
) -> Vec<String> {
	let mut collected = Vec::new();
	append_enabled_tool_names(&mut collected, enabled_tools, tool_names);
	collected
}

#[cfg(test)]
mod tests {
	use crate::{
		ToolCatalogConfig, build_resource_catalog_with_plugin_snapshot_and_runtime_capabilities,
	};
	use roku_plugin_host::PluginRegistrySnapshot;
	use roku_plugin_skills::SkillRegistry;

	use super::{
		RuntimeVisibleToolAvailabilitySnapshot, build_runtime_visible_tool_availability_snapshot,
	};

	fn runtime_catalog(skill_execution_enabled: bool) -> roku_common_types::ResourceCatalog {
		build_resource_catalog_with_plugin_snapshot_and_runtime_capabilities(
			&SkillRegistry::disabled(),
			&ToolCatalogConfig::default(),
			&PluginRegistrySnapshot::permissive(),
			skill_execution_enabled,
		)
	}

	#[test]
	fn snapshot_keeps_enabled_and_baseline_visible_tool_truth_aligned() {
		// Ownership proof surface:
		// - Contract owner: `RuntimeVisibleToolAvailabilitySnapshot` defines the enabled/baseline
		//   visibility contract and may align route/loop views, but must not encode approval policy.
		// - Registry owner: `ResourceCatalog` + `PluginRegistrySnapshot` decide which tool names are
		//   enabled before snapshot construction; adapter-local code must not override that truth.
		// - Execution owner: runtime route/loop code may derive shortlist and `visible_tools` seeds
		//   from this snapshot only, and must not re-infer local availability from tool adapters.
		// - Gating owner: policy bridge / tool runtime owns `policy_denied` and
		//   `approval_required` after invocation starts, not snapshot visibility.
		let snapshot = build_runtime_visible_tool_availability_snapshot(
			&runtime_catalog(false),
			&[
				"skill.execute",
				"inventory.describe",
				"fs.read_text",
				"inventory.describe",
				"not.enabled",
			],
		);

		assert!(!snapshot.is_tool_enabled("skill.execute"));
		assert!(snapshot.is_tool_enabled("inventory.describe"));
		assert!(
			snapshot.is_tool_enabled("command.run"),
			"policy-gated tools must remain enabled in the visibility contract"
		);
		assert_eq!(
			snapshot.baseline_visible_tools,
			vec!["inventory.describe".to_string(), "fs.read_text".to_string()]
		);
	}

	#[test]
	fn snapshot_filters_shortlist_candidates_and_compose_returns_all_enabled_tools() {
		let snapshot = RuntimeVisibleToolAvailabilitySnapshot::from_resource_catalog(
			&runtime_catalog(false),
			&["inventory.describe", "table.preview"],
		);

		assert_eq!(
			snapshot.filter_candidate_tools(
				Some("skill.execute"),
				&[
					"not.enabled".to_string(),
					"fs.read_text".to_string(),
					"inventory.describe".to_string(),
					"fs.read_text".to_string(),
				],
			),
			vec!["fs.read_text".to_string(), "inventory.describe".to_string()]
		);

		// compose_visible_tools now always returns ALL enabled tools.
		// Seed tools appear first; remaining enabled tools follow in BTreeSet order.
		let visible = snapshot.compose_visible_tools(["not.enabled", "fs.read_text"]);
		// fs.read_text is the only valid seed tool, so it must be first.
		assert_eq!(visible[0], "fs.read_text");
		// All enabled tools must be present.
		assert_eq!(
			{
				let mut v = visible.clone();
				v.sort();
				v
			},
			snapshot.enabled_tools.iter().cloned().collect::<Vec<_>>()
		);
	}

	#[test]
	fn snapshot_always_returns_all_enabled_tools_regardless_of_seed() {
		let snapshot = RuntimeVisibleToolAvailabilitySnapshot::from_resource_catalog(
			&runtime_catalog(false),
			&["not.enabled"],
		);

		// Even when no seed tools match, all enabled tools are returned.
		assert_eq!(
			snapshot.compose_visible_tools(["still.not.enabled"]),
			snapshot.enabled_tools.iter().cloned().collect::<Vec<_>>()
		);
	}
}
