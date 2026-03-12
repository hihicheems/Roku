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

use roku_common_types::{ResourceSelector, ResultEnvelope, TaskNode};

use crate::router::RouteDecision;
use crate::runtime_loop::StepAction;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectRouteKind {
	FilesystemLoop {
		commands: Option<Vec<FsCommandStep>>,
	},
	ToolLoop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsCommandStep {
	ChangeDir { path: String },
	ListDir { path: Option<String> },
	ReadText { path: String },
	PrintWorkingDir,
	Inspect { path: String },
	Exists { path: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct DirectRoutePlan {
	pub decision: RouteDecision,
	pub kind: DirectRouteKind,
	pub bound_resources: Vec<ResourceSelector>,
}

#[derive(Debug, Clone)]
pub struct DirectRouteExecutionResult {
	pub node: TaskNode,
	pub result: ResultEnvelope,
	pub message: String,
	pub terminal_step_action: Option<StepAction>,
}
