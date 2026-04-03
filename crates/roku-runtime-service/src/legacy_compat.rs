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

use roku_common_types::{
	ApprovalTicket, ResponseEnvelope, RuntimeError, Task, TaskEventKind, TaskState,
};

use crate::{RunMode, RuntimeService};

impl RuntimeService {
	/// Explicit compatibility boundary for graph-backed approval tasks that still
	/// resume through the historical scheduler path.
	pub(super) fn continue_legacy_graph_after_approval(
		&self,
		task: &mut Task,
		ticket: &ApprovalTicket,
	) -> Result<ResponseEnvelope, RuntimeError> {
		let graph_node = task
			.graph
			.as_ref()
			.and_then(|graph| {
				graph
					.nodes
					.iter()
					.find(|node| node.node_id == ticket.node_id)
			})
			.cloned();

		self.mark_node_completed_by_id(task, &ticket.node_id);
		if let Some(graph_node) = graph_node.as_ref() {
			self.append_node_event(
				task,
				graph_node,
				TaskEventKind::ApprovalApproved,
				"approval granted",
			)?;
		}
		self.record_transition(task, TaskState::Executing, "approval granted")?;
		self.process_task(task, RunMode::Normal)
	}
}
