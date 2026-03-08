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

//! Shared types for Roku runtime.

use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TaskId(pub String);

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RequestId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ApprovalId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ArtifactId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ExperimentRunId(pub String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlanningModeHint {
	ReAct,
	TaskDecomposition,
	TreeSearch,
	IterativeRefinement,
}

impl fmt::Display for PlanningModeHint {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		let label = match self {
			Self::ReAct => "ReAct",
			Self::TaskDecomposition => "TaskDecomposition",
			Self::TreeSearch => "TreeSearch",
			Self::IterativeRefinement => "IterativeRefinement",
		};
		write!(f, "{label}")
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConversationRole {
	User,
	Assistant,
	System,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationTurn {
	pub role: ConversationRole,
	pub content: String,
	#[serde(default)]
	pub created_at_unix_ms: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionPreferences {
	#[serde(default)]
	pub planning_mode: Option<PlanningModeHint>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestEnvelope {
	pub request_id: RequestId,
	pub session_id: String,
	pub goal: String,
	#[serde(default)]
	pub planning_mode_hint: Option<PlanningModeHint>,
	#[serde(default)]
	pub conversation_history: Vec<ConversationTurn>,
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
	CancelRequested,
	Compensating,
	TimeoutRecovering,
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
pub struct TaskReplayReport {
	pub task_id: TaskId,
	pub persisted_state: TaskState,
	pub replayed_state: TaskState,
	pub event_count: usize,
	pub transitions_valid: bool,
	pub chain_consistent: bool,
	pub snapshot_matches_replay: bool,
	pub recoverable: bool,
	pub replay_cursor: TaskReplayCursor,
	pub consistency_status: ReplayConsistencyStatus,
	pub recovery_eligibility: RecoveryEligibility,
	#[serde(default)]
	pub resume_candidates: Vec<ResumeCandidate>,
	pub events: Vec<TaskEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskReplayCursor {
	pub replayed_state: TaskState,
	pub event_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ReplayConsistencyStatus {
	#[default]
	Consistent,
	InvalidTransitions,
	BrokenTransitionChain,
	SnapshotMismatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum RecoveryEligibility {
	ResumeReady,
	FinalizeReady,
	PendingApproval,
	RequiresManualResume,
	Blocked,
	#[default]
	NotRecoverable,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResumeCandidate {
	pub node_id: NodeId,
	pub kind: TaskNodeKind,
	pub resume_point_id: String,
	pub eligibility: RecoveryEligibility,
	pub deadline_ms: u64,
	pub capability_requirements: Vec<String>,
	pub rerun_policy: RerunPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
	pub task_id: TaskId,
	pub request_id: RequestId,
	pub session_id: String,
	pub goal: String,
	pub state: TaskState,
	pub attempts: u32,
	#[serde(default)]
	pub planning_mode_hint: Option<PlanningModeHint>,
	#[serde(default)]
	pub conversation_history: Vec<ConversationTurn>,
	#[serde(default)]
	pub completed_nodes: Vec<NodeId>,
	#[serde(default)]
	pub next_node_index: usize,
	#[serde(default)]
	pub pending_approval_id: Option<ApprovalId>,
	#[serde(default)]
	pub last_result: Option<ResultEnvelope>,
	#[serde(default)]
	pub compensation_records: Vec<CompensationRecord>,
	pub graph: Option<TaskGraph>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum CompensationAction {
	Noop,
	#[default]
	AuditOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum CompensationStatus {
	#[default]
	Pending,
	Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CompensationRecord {
	pub node_id: NodeId,
	#[serde(default)]
	pub action: CompensationAction,
	#[serde(default)]
	pub status: CompensationStatus,
	#[serde(default)]
	pub note: String,
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
	#[serde(default)]
	pub depends_on: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskGraph {
	pub task_id: TaskId,
	pub nodes: Vec<TaskNode>,
	pub edges: Vec<TaskEdge>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum TaskNodeKind {
	#[default]
	Execution,
	Validation,
	Approval,
	Aggregation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum JoinPolicy {
	#[default]
	AllParents,
	AnyParent,
	Quorum(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AggregationMode {
	#[default]
	CollectAll,
	HighestConfidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct NodeRecoveryAnchor {
	pub resume_point_id: String,
	#[serde(default)]
	pub requires_manual_resume: bool,
	#[serde(default)]
	pub allows_partial_rerun: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct NodeBudgetSnapshot {
	pub token_budget: u64,
	pub time_budget_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryPolicy {
	pub max_attempts: u8,
	pub retry_on_timeout: bool,
}

impl Default for RetryPolicy {
	fn default() -> Self {
		Self {
			max_attempts: 1,
			retry_on_timeout: false,
		}
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum RerunPolicy {
	#[default]
	SafeToRerun,
	RequiresManualResume,
	Never,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskNode {
	pub node_id: NodeId,
	pub kind: TaskNodeKind,
	pub description: String,
	pub capabilities: Vec<String>,
	#[serde(default)]
	pub join_policy: JoinPolicy,
	#[serde(default)]
	pub aggregation_mode: AggregationMode,
	#[serde(default)]
	pub recovery_anchor: NodeRecoveryAnchor,
	#[serde(default)]
	pub budget_snapshot: NodeBudgetSnapshot,
	#[serde(default)]
	pub deadline_ms: u64,
	#[serde(default)]
	pub capability_requirements_snapshot: Vec<String>,
	#[serde(default)]
	pub retry_policy: RetryPolicy,
	#[serde(default)]
	pub rerun_policy: RerunPolicy,
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
	#[serde(default)]
	pub conversation_history: Vec<ConversationTurn>,
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
pub struct ArtifactMetadataEntry {
	pub key: String,
	pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
	pub artifact_id: ArtifactId,
	pub task_id: TaskId,
	pub node_id: NodeId,
	pub kind: String,
	pub uri: String,
	pub schema_version: String,
	pub checksum: String,
	pub metadata: Vec<ArtifactMetadataEntry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExperimentStatus {
	Running,
	Succeeded,
	Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExperimentMetric {
	pub name: String,
	pub value: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExperimentRun {
	pub run_id: ExperimentRunId,
	pub task_id: TaskId,
	pub request_id: RequestId,
	pub goal: String,
	pub strategy: String,
	pub status: ExperimentStatus,
	pub summary: Option<String>,
	pub metrics: Vec<ExperimentMetric>,
	pub artifact_ids: Vec<ArtifactId>,
	pub failure_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationEvidenceSet {
	pub result: ResultEnvelope,
	pub artifacts: Vec<Artifact>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeResultSet {
	pub node_id: NodeId,
	pub join_policy: JoinPolicy,
	pub aggregation_mode: AggregationMode,
	pub source_node_ids: Vec<NodeId>,
	pub missing_source_nodes: Vec<NodeId>,
	pub results: Vec<ResultEnvelope>,
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
	Cancelled,
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
