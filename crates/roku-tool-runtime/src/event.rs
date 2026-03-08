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

use crate::SandboxProfile;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionEventKind {
	Dispatched,
	AttemptStarted,
	AttemptFailed,
	Retrying,
	Succeeded,
	TimedOut,
	Rejected,
}

impl ExecutionEventKind {
	pub(crate) fn as_str(&self) -> &'static str {
		match self {
			Self::Dispatched => "dispatched",
			Self::AttemptStarted => "attempt_started",
			Self::AttemptFailed => "attempt_failed",
			Self::Retrying => "retrying",
			Self::Succeeded => "succeeded",
			Self::TimedOut => "timed_out",
			Self::Rejected => "rejected",
		}
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionEvent {
	pub trace_id: String,
	pub invocation_key: String,
	pub tool_name: String,
	pub kind: ExecutionEventKind,
	pub attempt: u8,
	pub sandbox_profile: SandboxProfile,
	pub fingerprint: Option<String>,
	pub message: Option<String>,
}

pub trait ExecutionHook: Send + Sync {
	fn on_event(&self, event: &ExecutionEvent);
}
