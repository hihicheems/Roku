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

//! Tool visibility management.
//!
//! Computes the set of visible tool names for a given loop turn,
//! combining the route decision's candidate tools with the registry's
//! mode-aware filtering.

use super::registry::ToolRegistry;
use super::trait_def::LoopMode;

/// Compute visible tool names for the current turn.
///
/// Seed tools (from the route decision) are listed first for ordering priority.
/// All remaining tools visible in the given mode follow.
#[allow(dead_code)]
pub(crate) fn compose_visible_tools(
	registry: &ToolRegistry,
	seed_tool_names: &[String],
	mode: LoopMode,
) -> Vec<String> {
	let all_visible = registry.visible_names(mode);
	let mut result = Vec::with_capacity(all_visible.len());

	// Seed tools first (if they're visible in this mode).
	for seed in seed_tool_names {
		if all_visible.iter().any(|v| v == seed) && !result.iter().any(|r| r == seed) {
			result.push(seed.clone());
		}
	}

	// Then all remaining visible tools.
	for name in all_visible {
		if !result.iter().any(|r| r == &name) {
			result.push(name);
		}
	}

	result
}
