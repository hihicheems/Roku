use roku_common_types::{ApprovalId, ApprovalStatus, TaskState};

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
