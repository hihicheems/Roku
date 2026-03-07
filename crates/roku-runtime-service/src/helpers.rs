use roku_common_types::{ApprovalId, ApprovalStatus, ResultEnvelope, TaskState};

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
	}
}

pub(crate) fn success_message(last_result: Option<&ResultEnvelope>) -> String {
	match last_result {
		Some(result) => extract_message(&result.payload).unwrap_or_else(|| result.payload.clone()),
		None => "task succeeded".to_string(),
	}
}

fn extract_message(payload: &str) -> Option<String> {
	let parsed = serde_json::from_str::<serde_json::Value>(payload).ok()?;
	parsed
		.get("message")
		.and_then(serde_json::Value::as_str)
		.map(std::string::ToString::to_string)
}
