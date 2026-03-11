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

use std::path::PathBuf;

use roku_common_types::{ResourceSelector, ResultEnvelope, TaskNode};
use serde_json::Value;

use crate::router::RouteDecision;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectRouteKind {
	Inventory,
	Conversation,
	SkillInstall {
		source_url: String,
	},
	SkillAdvisory {
		selector: ResourceSelector,
	},
	SkillExecutable {
		selector: ResourceSelector,
	},
	ToolInvocation {
		selector: ResourceSelector,
		arguments: Value,
		attachments: Vec<PathBuf>,
	},
}

#[derive(Debug, Clone, PartialEq)]
pub struct DirectRoutePlan {
	pub decision: RouteDecision,
	pub kind: DirectRouteKind,
}

#[derive(Debug, Clone)]
pub struct DirectRouteExecutionResult {
	pub node: TaskNode,
	pub result: ResultEnvelope,
	pub message: String,
}
