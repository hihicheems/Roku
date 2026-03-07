use roku_common_types::{
	ApprovalStatus, ApprovalTicket, Artifact, ExperimentMetric, ExperimentRun, ExperimentStatus,
	ResponseStatus,
};
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactResponse {
	pub artifact_id: String,
	pub task_id: String,
	pub node_id: String,
	pub kind: String,
	pub uri: String,
	pub schema_version: String,
	pub checksum: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExperimentMetricResponse {
	pub name: String,
	pub value: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExperimentResponse {
	pub run_id: String,
	pub task_id: String,
	pub request_id: String,
	pub goal: String,
	pub strategy: String,
	pub status: String,
	pub summary: Option<String>,
	pub metrics: Vec<ExperimentMetricResponse>,
	pub artifact_ids: Vec<String>,
	pub failure_reason: Option<String>,
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

pub(crate) fn artifact_response(artifact: Artifact) -> ArtifactResponse {
	ArtifactResponse {
		artifact_id: artifact.artifact_id.0,
		task_id: artifact.task_id.0,
		node_id: artifact.node_id.0,
		kind: artifact.kind,
		uri: artifact.uri,
		schema_version: artifact.schema_version,
		checksum: artifact.checksum,
	}
}

pub(crate) fn experiment_response(run: ExperimentRun) -> ExperimentResponse {
	ExperimentResponse {
		run_id: run.run_id.0,
		task_id: run.task_id.0,
		request_id: run.request_id.0,
		goal: run.goal,
		strategy: run.strategy,
		status: experiment_status_label(run.status).to_string(),
		summary: run.summary,
		metrics: run.metrics.into_iter().map(metric_response).collect(),
		artifact_ids: run
			.artifact_ids
			.into_iter()
			.map(|artifact_id| artifact_id.0)
			.collect(),
		failure_reason: run.failure_reason,
	}
}

fn approval_status_label(status: ApprovalStatus) -> &'static str {
	match status {
		ApprovalStatus::Pending => "pending",
		ApprovalStatus::Approved => "approved",
		ApprovalStatus::Rejected => "rejected",
	}
}

fn experiment_status_label(status: ExperimentStatus) -> &'static str {
	match status {
		ExperimentStatus::Running => "running",
		ExperimentStatus::Succeeded => "succeeded",
		ExperimentStatus::Failed => "failed",
	}
}

fn metric_response(metric: ExperimentMetric) -> ExperimentMetricResponse {
	ExperimentMetricResponse {
		name: metric.name,
		value: metric.value,
	}
}
