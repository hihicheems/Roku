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

use std::env;

use roku_common_types::ResourceSelector;
use serde::{Deserialize, Serialize};

use crate::router::RouteDecision;
use crate::runtime_loop::{LoopRequest, ToolObservation};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoopContext {
	pub request_id: String,
	pub session_id: String,
	pub goal: String,
	pub workspace_root: String,
	pub working_directory: String,
	pub visible_tools: Vec<String>,
	pub bound_resources: Vec<ResourceSelector>,
	pub route_decision: RouteDecision,
	pub last_observation: Option<ToolObservation>,
}

pub(crate) fn build_loop_context(
	request: &LoopRequest,
	route_decision: &RouteDecision,
	visible_tools: Vec<String>,
	bound_resources: Vec<ResourceSelector>,
) -> LoopContext {
	let working_directory = env::current_dir()
		.ok()
		.unwrap_or_else(|| ".".into())
		.display()
		.to_string();

	LoopContext {
		request_id: request.request_id.0.clone(),
		session_id: request.session_id.clone(),
		goal: request.goal.clone(),
		workspace_root: working_directory.clone(),
		working_directory,
		visible_tools,
		bound_resources,
		route_decision: route_decision.clone(),
		last_observation: None,
	}
}
