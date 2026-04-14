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

//! RenderState — mutable state variables for the interactive render engine.

/// Mutable state for the interactive render engine.
pub(crate) struct RenderState {
	pub(crate) stream_renderer: super::StreamRenderer,
	pub(crate) start_time: std::time::Instant,
	pub(crate) current_step: u32,
	pub(crate) current_tool: Option<String>,
	pub(crate) status_visible: bool,
	pub(crate) streaming_active: bool,
	pub(crate) pending_tool_name: Option<String>,
	pub(crate) last_tool_event: std::time::Instant,
	pub(crate) had_text_output: bool,
}

impl RenderState {
	pub(crate) fn new() -> Self {
		let now = std::time::Instant::now();
		Self {
			stream_renderer: super::StreamRenderer::new(),
			start_time: now,
			current_step: 0,
			current_tool: None,
			status_visible: false,
			streaming_active: false,
			pending_tool_name: None,
			last_tool_event: now,
			had_text_output: false,
		}
	}
}
