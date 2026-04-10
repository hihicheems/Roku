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

use roku_common_types::{ApprovalId, ApprovalStatus, ResultEnvelope, TaskState};

pub(crate) fn failure_message(reason: &str, terminal_state: TaskState) -> String {
	let brief = humanize_error(reason);
	if terminal_state == TaskState::DeadLetter {
		format!("Request could not be processed: {brief}")
	} else {
		format!("Unable to complete request: {brief}")
	}
}

/// Reduce an internal error chain to a human-readable one-liner.
fn humanize_error(raw: &str) -> String {
	// Take only the last segment of a colon-separated error chain.
	let leaf = raw.rsplit_once(": ").map_or(raw, |(_, last)| last).trim();

	// Strip common internal prefixes.
	let cleaned = leaf
		.strip_prefix("failed to ")
		.or_else(|| leaf.strip_prefix("error "))
		.unwrap_or(leaf);

	// Capitalize first letter.
	let mut chars = cleaned.chars();
	match chars.next() {
		Some(first) => {
			let mut result = first.to_uppercase().to_string();
			result.push_str(chars.as_str());
			result
		}
		None => raw.to_string(),
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
