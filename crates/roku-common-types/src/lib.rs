//! Shared types for Roku runtime.

use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TaskId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RequestId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ApprovalId(pub String);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestEnvelope {
	pub request_id: RequestId,
	pub session_id: String,
	pub goal: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResponseStatus {
	Succeeded,
	PendingApproval,
	Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseEnvelope {
	pub request_id: RequestId,
	pub status: ResponseStatus,
	pub message: String,
	pub artifacts: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorClass {
	Validation,
	Dependency,
	Timeout,
	Security,
	BudgetExhausted,
	NonRetriable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskState {
	Queued,
	Planning,
	GraphBuilding,
	Delegating,
	Executing,
	Validating,
	WaitingApproval,
	Aggregating,
	Succeeded,
	Failed,
	DeadLetter,
	Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskEvent {
	pub task_id: TaskId,
	pub from: TaskState,
	pub to: TaskState,
	pub reason: String,
	pub error_class: Option<ErrorClass>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
	pub task_id: TaskId,
	pub request_id: RequestId,
	pub state: TaskState,
	pub attempts: u32,
	#[serde(default)]
	pub completed_nodes: Vec<NodeId>,
	#[serde(default)]
	pub next_node_index: usize,
	#[serde(default)]
	pub pending_approval_id: Option<ApprovalId>,
	#[serde(default)]
	pub last_result: Option<ResultEnvelope>,
	pub graph: Option<TaskGraph>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanOutline {
	pub goal: String,
	pub steps: Vec<PlanStep>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanStep {
	pub step_id: String,
	pub summary: String,
	pub required_capabilities: Vec<String>,
	pub requires_approval: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskGraph {
	pub task_id: TaskId,
	pub nodes: Vec<TaskNode>,
	pub edges: Vec<TaskEdge>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskNodeKind {
	Execution,
	Validation,
	Approval,
	Aggregation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskNode {
	pub node_id: NodeId,
	pub kind: TaskNodeKind,
	pub description: String,
	pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskEdge {
	pub from: NodeId,
	pub to: NodeId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentContext {
	pub task_id: TaskId,
	pub node_id: NodeId,
	pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyBindings {
	pub budget_tokens: u64,
	pub time_budget_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInstanceSpec {
	pub instance_id: String,
	pub context: AgentContext,
	pub capabilities: Vec<String>,
	pub policy_bindings: PolicyBindings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityToken {
	pub token_id: String,
	pub subject: String,
	pub resource: String,
	pub actions: Vec<String>,
	pub expires_at_unix: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResultStatus {
	Ok,
	Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceItem {
	pub kind: String,
	pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResultEnvelope {
	pub task_id: TaskId,
	pub node_id: NodeId,
	pub producer: String,
	pub schema_version: String,
	pub status: ResultStatus,
	pub payload: String,
	pub evidence: Vec<EvidenceItem>,
	pub confidence: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationReport {
	pub accepted: bool,
	pub failures: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApprovalStatus {
	Pending,
	Approved,
	Rejected,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalDecision {
	pub actor: String,
	pub approved: bool,
	pub comment: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalTicket {
	pub approval_id: ApprovalId,
	pub task_id: TaskId,
	pub request_id: RequestId,
	pub node_id: NodeId,
	pub summary: String,
	pub status: ApprovalStatus,
	pub decided_by: Option<String>,
	pub comment: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodingWorkContract {
	pub repo_ref: String,
	pub goal: String,
	pub allowed_paths: Vec<String>,
	pub acceptance_checks: Vec<String>,
	pub output_schema: String,
	pub token_budget: u64,
	pub time_budget_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeChangeReport {
	pub modified_files: Vec<String>,
	pub patch_summary: String,
	pub commands_executed: Vec<String>,
	pub test_results: Vec<String>,
	pub artifacts: Vec<String>,
	pub residual_risks: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeError {
	pub message: String,
}

impl RuntimeError {
	pub fn new(message: impl Into<String>) -> Self {
		Self {
			message: message.into(),
		}
	}
}

impl fmt::Display for RuntimeError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "{}", self.message)
	}
}

impl std::error::Error for RuntimeError {}
