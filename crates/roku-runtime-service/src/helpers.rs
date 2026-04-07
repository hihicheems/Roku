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

use std::future::Future;

use roku_common_types::{ApprovalId, ApprovalStatus, ResultEnvelope, TaskState};

/// Bridge an `async` function call back to synchronous caller code.
///
/// Three cases are handled:
///
/// 1. **No current runtime** – a fresh multi-thread tokio runtime is created and driven to
///    completion with `block_on`.
///
/// 2. **Current-thread runtime** (e.g. `#[actix_web::test]`, `#[tokio::test]`) – `block_in_place`
///    cannot be used here because it panics on a current-thread runtime. Instead a scoped OS
///    thread is spawned so the future runs on a fresh multi-thread runtime without touching the
///    caller's executor.
///
/// 3. **Multi-thread runtime** – `tokio::task::block_in_place` parks the current worker thread
///    and drives the future on the caller's executor handle.
///
/// A **multi-thread** runtime is always used to drive the future because `execute_tool_loop`
/// calls `tokio::task::block_in_place` internally for synchronous tool invocations, and
/// `block_in_place` panics on a current-thread runtime.
pub(crate) fn bridge_async_to_sync<F>(future: F) -> F::Output
where
	F: Future + Send,
	F::Output: Send,
{
	match tokio::runtime::Handle::try_current() {
		Err(_) => {
			// No runtime – spin up a fresh multi-thread runtime.
			tokio::runtime::Builder::new_multi_thread()
				.enable_all()
				.build()
				.expect("bridge async-to-sync runtime should build")
				.block_on(future)
		}
		Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::CurrentThread => {
			// Current-thread runtime: block_in_place would panic. Escape to a scoped OS thread
			// that owns a fresh multi-thread runtime, keeping lifetime safety via thread::scope.
			std::thread::scope(|s| {
				s.spawn(|| {
					tokio::runtime::Builder::new_multi_thread()
						.enable_all()
						.build()
						.expect("bridge async-to-sync scoped runtime should build")
						.block_on(future)
				})
				.join()
				.expect("bridge async-to-sync scoped thread should not panic")
			})
		}
		Ok(handle) => {
			// Multi-thread runtime: park the current worker and drive the future here.
			tokio::task::block_in_place(|| handle.block_on(future))
		}
	}
}

pub(crate) fn failure_message(reason: &str, terminal_state: TaskState) -> String {
	if terminal_state == TaskState::DeadLetter {
		format!("task dead-lettered: {reason}")
	} else {
		format!("task failed: {reason}")
	}
}

pub(crate) fn approval_artifact(approval_id: &ApprovalId) -> String {
	format!("approval://{}", approval_id.0)
}

pub(crate) fn ticket_status_label(status: ApprovalStatus) -> &'static str {
	match status {
		ApprovalStatus::Pending => "pending",
		ApprovalStatus::Approved => "approved",
		ApprovalStatus::Rejected => "rejected",
		ApprovalStatus::Cancelled => "cancelled",
	}
}

pub(crate) fn result_message(result: &ResultEnvelope) -> String {
	extract_message(&result.payload).unwrap_or_else(|| result.payload.clone())
}

fn extract_message(payload: &str) -> Option<String> {
	let parsed = serde_json::from_str::<serde_json::Value>(payload).ok()?;
	parsed
		.get("message")
		.and_then(serde_json::Value::as_str)
		.map(std::string::ToString::to_string)
}
