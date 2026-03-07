use roku_common_types::{ApprovalStatus, ApprovalTicket, ResponseStatus};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize)]
pub struct HealthResponse {
	pub status: &'static str,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitRequest {
	pub session_id: String,
	pub goal: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitResponse {
	pub request_id: String,
	pub status: String,
	pub message: String,
	pub artifacts: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalDecisionRequest {
	pub actor: String,
	pub approved: bool,
	pub comment: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalTicketResponse {
	pub approval_id: String,
	pub task_id: String,
	pub request_id: String,
	pub node_id: String,
	pub status: String,
	pub summary: String,
	pub decided_by: Option<String>,
	pub comment: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorResponse {
	pub message: String,
}

pub(crate) fn response_status_label(status: ResponseStatus) -> &'static str {
	match status {
		ResponseStatus::Succeeded => "succeeded",
		ResponseStatus::PendingApproval => "pending_approval",
		ResponseStatus::Failed => "failed",
	}
}

pub(crate) fn approval_ticket_response(ticket: ApprovalTicket) -> ApprovalTicketResponse {
	ApprovalTicketResponse {
		approval_id: ticket.approval_id.0,
		task_id: ticket.task_id.0,
		request_id: ticket.request_id.0,
		node_id: ticket.node_id.0,
		status: approval_status_label(ticket.status).to_string(),
		summary: ticket.summary,
		decided_by: ticket.decided_by,
		comment: ticket.comment,
	}
}

fn approval_status_label(status: ApprovalStatus) -> &'static str {
	match status {
		ApprovalStatus::Pending => "pending",
		ApprovalStatus::Approved => "approved",
		ApprovalStatus::Rejected => "rejected",
	}
}
