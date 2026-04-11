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

use crate::{DirectRoutePlan, IntentFamily, LoopEventSender, RouteDecision, RouteRisk};
use roku_common_types::{
	RequestEnvelope, ResponseEnvelope, RuntimeError, RuntimeMemorySections, Task,
};

use super::{ContextBundle, RuntimeMemoryLayers, RuntimeService};

pub(super) struct RuntimeLoopOwner<'a> {
	service: &'a RuntimeService,
}

struct PreparedRuntimeLoopRequest {
	context_bundle: ContextBundle,
	runtime_memory_layers: RuntimeMemoryLayers,
}

impl PreparedRuntimeLoopRequest {
	fn runtime_memory_sections(&self) -> RuntimeMemorySections {
		self.runtime_memory_layers.structured_sections()
	}
}

impl<'a> RuntimeLoopOwner<'a> {
	pub(super) fn new(service: &'a RuntimeService) -> Self {
		Self { service }
	}

	pub(super) async fn execute_request(
		&self,
		task: &mut Task,
		request: &RequestEnvelope,
		event_sender: Option<&LoopEventSender>,
	) -> Result<ResponseEnvelope, RuntimeError> {
		let mut resumable_loop = self.service.take_resumable_pending_loop(request).await?;
		let mut prepared = self.prepare_request_context(
			task,
			request,
			resumable_loop.is_some(),
			resumable_loop
				.as_ref()
				.map(|loop_state| loop_state.working_summary.as_str()),
		)?;

		if let Some(mut loop_state) = resumable_loop.take() {
			self.service
				.attach_resumed_loop_resources(&mut prepared.context_bundle, &loop_state);
			let runtime_memory_sections = prepared.runtime_memory_sections();
			return self
				.service
				.resume_pending_loop(
					task,
					request,
					&mut loop_state,
					&prepared.context_bundle,
					&runtime_memory_sections,
					event_sender,
				)
				.await;
		}

		// Direct route — no classifier. All requests go straight into the turn
		// loop. The model decides which tools to use via system prompt guidance.
		let plan = default_direct_route();
		self.service
			.attach_visible_resources_for_plan(&mut prepared.context_bundle, &plan);
		let mut loop_state = self
			.service
			.initialize_runtime_loop_for_plan(request, &plan);

		let runtime_memory_sections = prepared.runtime_memory_sections();
		self.service.metrics.inc_direct_route_hits();
		self.service
			.process_direct_route(
				task,
				request,
				&plan,
				&mut loop_state,
				&prepared.context_bundle,
				&runtime_memory_sections,
				event_sender,
			)
			.await
	}

	fn prepare_request_context(
		&self,
		task: &Task,
		request: &RequestEnvelope,
		pending_loop_active: bool,
		working_memory: Option<&str>,
	) -> Result<PreparedRuntimeLoopRequest, RuntimeError> {
		let context_bundle = self
			.service
			.build_context_bundle(request, pending_loop_active)?;
		let runtime_memory_layers = context_bundle
			.runtime_memory_layers_with_working_memory(working_memory.unwrap_or_default());
		self.service
			.cache_runtime_memory_layers(&task.task_id, &runtime_memory_layers);
		Ok(PreparedRuntimeLoopRequest {
			context_bundle,
			runtime_memory_layers,
		})
	}
}

/// Default direct route used for all requests. Empty candidate_tools causes
/// `compose_visible_tools()` to return all enabled tools — the model picks.
fn default_direct_route() -> DirectRoutePlan {
	DirectRoutePlan {
		decision: RouteDecision::new(
			IntentFamily::Chat,
			1.0,
			false,
			RouteRisk::Low,
			vec![],
			vec![],
			vec![],
			"direct route (no classifier)",
		),
		bound_resources: vec![],
	}
}
