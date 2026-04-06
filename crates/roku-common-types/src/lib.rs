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

pub mod approval;
pub mod canonical_execution;
pub mod execution_policy;
pub mod execution_preview;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub use approval::{
	ApprovalDecision, ApprovalStatus, ApprovalTicket, ApprovalTicketStatus, ApprovedExecutionRef,
	PendingExecutionApproval,
};
pub use canonical_execution::{
	CanonicalDigest, CanonicalExecution, ExecutionActionClass, ExecutionEnvPolicy,
	ExecutionEnvPolicyMode, ExecutionResourceScope, ExecutionShellContext, InvocationMode,
};
pub use execution_policy::{
	ApprovalRequirement, ApprovalRequirementScope, PolicyDecision, PolicyOutcome, PolicyReasonCode,
};
pub use execution_preview::{ExecutionPreview, project_execution_preview};

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
	pub pending_loop: Option<PendingLoopBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingLoopBinding {
	pub run_id: String,
	pub loop_state_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestEnvelope {
	pub request_id: RequestId,
	pub session_id: String,
	pub goal: String,
	#[serde(default)]
	/// Deprecated compatibility hint; new requests stay on the direct runtime path.
	pub planning_mode_hint: Option<PlanningModeHint>,
	#[serde(default)]
	/// Short-term continuity only; long-term recall is injected through runtime-owned context.
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
	#[serde(default)]
	pub kind: TaskEventKind,
	#[serde(default)]
	pub node_id: Option<NodeId>,
	#[serde(default)]
	pub node_kind: Option<TaskNodeKind>,
	#[serde(default)]
	pub attempt: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum TaskEventKind {
	#[default]
	StateTransition,
	NodeCompleted,
	ApprovalPending,
	ApprovalApproved,
	ApprovalRejected,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskReplaySnapshot {
	pub task_id: TaskId,
	pub compacted_event_count: usize,
	pub replayed_state: TaskState,
	#[serde(default)]
	pub completed_nodes: Vec<NodeId>,
	#[serde(default)]
	pub pending_approval_id: Option<ApprovalId>,
	#[serde(default)]
	pub last_result: Option<ResultEnvelope>,
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
	/// Legacy persisted short-term continuity snapshot; not a canonical long-term memory source.
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
	#[serde(default)]
	pub resource_selectors: Vec<ResourceSelector>,
	#[serde(default)]
	pub required_capabilities: Vec<String>,
	pub requires_approval: bool,
	#[serde(default)]
	pub depends_on: Vec<String>,
	#[serde(default)]
	pub branch: Option<PlanBranch>,
	#[serde(default)]
	pub loop_control: Option<PlanLoopControl>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PlanBranch {
	pub branch_group: String,
	pub branch_label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PlanLoopControl {
	pub loop_id: String,
	pub iteration: u8,
	pub max_iterations: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum TaskNodeKind {
	#[default]
	Execution,
	Validation,
	Approval,
	Aggregation,
	Retry,
	DeadLetter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum TaskNodeDispatchPolicy {
	#[default]
	Automatic,
	ManualRecovery,
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
	#[serde(default)]
	pub resources: Vec<ResourceSelector>,
	#[serde(default)]
	pub capabilities: Vec<String>,
	#[serde(default)]
	pub dispatch_policy: TaskNodeDispatchPolicy,
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
pub struct AgentContext {
	pub task_id: TaskId,
	pub node_id: NodeId,
	pub summary: String,
	#[serde(default)]
	pub resources: Vec<ResourceSelector>,
	#[serde(default)]
	/// Legacy short-term continuity carrier; does not transport long-term recall hits.
	pub conversation_history: Vec<ConversationTurn>,
	#[serde(default)]
	pub runtime_memory_sections: RuntimeMemorySections,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeMemorySections {
	#[serde(default)]
	pub short_term_continuity: String,
	#[serde(default)]
	pub long_term_recall: String,
	#[serde(default)]
	pub working_memory: String,
}

impl RuntimeMemorySections {
	pub fn is_empty(&self) -> bool {
		self.short_term_continuity.trim().is_empty()
			&& self.long_term_recall.trim().is_empty()
			&& self.working_memory.trim().is_empty()
	}

	pub fn named_sections_text(&self) -> String {
		let mut sections = Vec::new();
		push_named_section(
			&mut sections,
			"Short-term continuity",
			&self.short_term_continuity,
		);
		push_named_section(&mut sections, "Long-term recall", &self.long_term_recall);
		push_named_section(&mut sections, "Working memory", &self.working_memory);
		sections.join("\n\n")
	}
}

fn push_named_section(sections: &mut Vec<String>, title: &str, content: &str) {
	let trimmed = content.trim();
	if !trimmed.is_empty() {
		sections.push(format!("{title}:\n{trimmed}"));
	}
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
	#[serde(default)]
	pub capability_tokens: Vec<CapabilityToken>,
	pub policy_bindings: PolicyBindings,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ResourceSelector {
	Tool { name: String },
	Skill { name: String },
}

impl ResourceSelector {
	pub fn tool(name: impl Into<String>) -> Self {
		Self::Tool { name: name.into() }
	}

	pub fn skill(name: impl Into<String>) -> Self {
		Self::Skill { name: name.into() }
	}

	pub fn name(&self) -> &str {
		match self {
			Self::Tool { name } | Self::Skill { name } => name,
		}
	}

	pub fn display_key(&self) -> String {
		match self {
			Self::Tool { name } => format!("tool:{name}"),
			Self::Skill { name } => format!("skill:{name}"),
		}
	}
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ToolContract {
	#[serde(default)]
	pub selection: ToolSelectionContract,
	#[serde(default)]
	pub input: ToolInputContract,
	#[serde(default)]
	pub output: ToolOutputContract,
	#[serde(default)]
	pub runtime: ToolRuntimeContract,
}

impl ToolContract {
	pub fn searchable_text(&self) -> String {
		[
			self.selection.searchable_text(),
			self.input.searchable_text(),
			self.output.searchable_text(),
			self.runtime.searchable_text(),
		]
		.into_iter()
		.filter(|value| !value.is_empty())
		.collect::<Vec<_>>()
		.join(" ")
	}
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ToolSelectionContract {
	#[serde(default)]
	pub use_when: Vec<String>,
	#[serde(default)]
	pub avoid_when: Vec<String>,
	#[serde(default)]
	pub common_confusions: Vec<String>,
}

impl ToolSelectionContract {
	pub fn searchable_text(&self) -> String {
		[
			self.use_when.join(" "),
			self.avoid_when.join(" "),
			self.common_confusions.join(" "),
		]
		.into_iter()
		.filter(|value| !value.is_empty())
		.collect::<Vec<_>>()
		.join(" ")
	}
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ToolInputContract {
	#[serde(default)]
	pub fields: Vec<ToolInputFieldContract>,
	#[serde(default)]
	pub preconditions: Vec<String>,
}

impl ToolInputContract {
	pub fn field_names(&self) -> Vec<String> {
		self.fields
			.iter()
			.map(|field| field.name.clone())
			.collect::<Vec<_>>()
	}

	pub fn required_field_names(&self) -> Vec<String> {
		self.fields
			.iter()
			.filter(|field| field.required)
			.map(|field| field.name.clone())
			.collect::<Vec<_>>()
	}

	pub fn searchable_text(&self) -> String {
		let field_text = self
			.fields
			.iter()
			.map(ToolInputFieldContract::searchable_text)
			.collect::<Vec<_>>()
			.join(" ");
		[field_text, self.preconditions.join(" ")]
			.into_iter()
			.filter(|value| !value.is_empty())
			.collect::<Vec<_>>()
			.join(" ")
	}
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ToolInputFieldContract {
	pub name: String,
	#[serde(default)]
	pub required: bool,
	pub semantics: String,
	#[serde(default)]
	pub invalid_when: Vec<String>,
}

impl ToolInputFieldContract {
	pub fn searchable_text(&self) -> String {
		[
			self.name.clone(),
			self.semantics.clone(),
			self.invalid_when.join(" "),
		]
		.into_iter()
		.filter(|value| !value.is_empty())
		.collect::<Vec<_>>()
		.join(" ")
	}
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolOutputContract {
	#[serde(default = "default_tool_observation_schema")]
	pub observation_schema: String,
	pub success_semantics: String,
	pub empty_result_semantics: String,
	#[serde(default)]
	pub error_semantics: Vec<String>,
	#[serde(default)]
	pub non_terminal_success: bool,
	#[serde(default)]
	pub terminal_success: bool,
}

impl Default for ToolOutputContract {
	fn default() -> Self {
		Self {
			observation_schema: default_tool_observation_schema(),
			success_semantics: String::new(),
			empty_result_semantics: String::new(),
			error_semantics: Vec::new(),
			non_terminal_success: false,
			terminal_success: false,
		}
	}
}

impl ToolOutputContract {
	pub fn searchable_text(&self) -> String {
		[
			self.observation_schema.clone(),
			self.success_semantics.clone(),
			self.empty_result_semantics.clone(),
			self.error_semantics.join(" "),
		]
		.into_iter()
		.filter(|value| !value.is_empty())
		.collect::<Vec<_>>()
		.join(" ")
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ToolSideEffectPolicy {
	#[default]
	None,
	ReadOnly,
	WorkspaceWrite,
	ExternalMutation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ToolRetryPolicy {
	#[default]
	Never,
	RuntimeMayRetry,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ToolIsolationProfile {
	#[default]
	NoIsolation,
	ReadOnlyFs,
	PythonResearch,
	ContainerRestricted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ToolRuntimeContract {
	#[serde(default)]
	pub side_effects: ToolSideEffectPolicy,
	#[serde(default)]
	pub retry_policy: ToolRetryPolicy,
	#[serde(default)]
	pub timeout_ms: u64,
	#[serde(default)]
	pub isolation_profile: ToolIsolationProfile,
}

impl ToolRuntimeContract {
	pub fn searchable_text(&self) -> String {
		format!(
			"{:?} {:?} {} {:?}",
			self.side_effects, self.retry_policy, self.timeout_ms, self.isolation_profile
		)
	}
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolOutputEnvelope {
	pub ok: bool,
	#[serde(default)]
	pub error_type: Option<String>,
	pub terminal: bool,
	pub message: String,
	#[serde(default)]
	pub data: Value,
}

impl ToolOutputEnvelope {
	pub fn new(
		ok: bool,
		error_type: Option<impl Into<String>>,
		terminal: bool,
		message: impl Into<String>,
		data: Value,
	) -> Self {
		Self {
			ok,
			error_type: error_type.map(Into::into),
			terminal,
			message: message.into(),
			data,
		}
	}

	pub fn into_value(self) -> Value {
		json!({
			"ok": self.ok,
			"error_type": self.error_type,
			"terminal": self.terminal,
			"message": self.message,
			"data": self.data,
		})
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GeneralCompletionKind {
	GroundedAnswer,
	NeedsMoreInformation,
	InsufficientEvidence,
}

impl GeneralCompletionKind {
	pub fn error_type(self) -> Option<&'static str> {
		match self {
			Self::GroundedAnswer => None,
			Self::NeedsMoreInformation => Some("needs_more_information"),
			Self::InsufficientEvidence => Some("insufficient_evidence"),
		}
	}

	pub fn terminal(self, terminal_output: bool) -> bool {
		matches!(self, Self::GroundedAnswer) && terminal_output
	}

	pub fn ok(self) -> bool {
		matches!(self, Self::GroundedAnswer)
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GeneralEvidenceStatus {
	Grounded,
	MissingRequiredInput,
	MissingExecutionEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneralExecuteCompletion {
	pub final_message: String,
	pub completion_kind: GeneralCompletionKind,
	pub evidence_status: GeneralEvidenceStatus,
	#[serde(default)]
	pub missing_information: Vec<String>,
}

impl GeneralExecuteCompletion {
	pub fn grounded(final_message: impl Into<String>) -> Self {
		Self {
			final_message: final_message.into(),
			completion_kind: GeneralCompletionKind::GroundedAnswer,
			evidence_status: GeneralEvidenceStatus::Grounded,
			missing_information: Vec::new(),
		}
	}

	pub fn needs_more_information(
		final_message: impl Into<String>,
		missing_information: Vec<String>,
	) -> Self {
		Self {
			final_message: final_message.into(),
			completion_kind: GeneralCompletionKind::NeedsMoreInformation,
			evidence_status: GeneralEvidenceStatus::MissingRequiredInput,
			missing_information,
		}
	}

	pub fn insufficient_evidence(final_message: impl Into<String>) -> Self {
		Self {
			final_message: final_message.into(),
			completion_kind: GeneralCompletionKind::InsufficientEvidence,
			evidence_status: GeneralEvidenceStatus::MissingExecutionEvidence,
			missing_information: Vec::new(),
		}
	}
}

fn default_tool_observation_schema() -> String {
	"tool_observation.v1".to_string()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeLoopTrace {
	#[serde(default = "default_runtime_loop_trace_schema")]
	pub schema_version: String,
	pub run_id: String,
	pub status: String,
	pub step_count: usize,
	#[serde(default)]
	pub steps: Vec<RuntimeLoopTraceStep>,
	pub final_outcome: RuntimeLoopTraceOutcome,
}

impl RuntimeLoopTrace {
	pub fn schema_version() -> &'static str {
		"runtime_loop_trace.v1"
	}
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeLoopTraceStep {
	pub step_index: u32,
	pub decision: RuntimeLoopTraceDecision,
	#[serde(default)]
	pub visible_tools_before: Vec<String>,
	#[serde(default)]
	pub visible_resources_before: Option<Vec<ResourceSelector>>,
	pub started_at: String,
	pub finished_at: String,
	#[serde(default)]
	pub tool_latency_ms: Option<u64>,
	#[serde(default)]
	pub raw_tool_output: Option<Value>,
	#[serde(default)]
	pub observation: Option<Value>,
	#[serde(default)]
	pub interpreted_observation: Option<Value>,
	#[serde(default)]
	pub execution_trace: Option<RuntimeLoopExecutionTrace>,
	pub remaining_step_budget_after: u32,
	pub remaining_recovery_budget_after: u32,
	pub working_directory_after: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeLoopExecutionTrace {
	pub tool_name: String,
	pub digest: String,
	#[serde(default)]
	pub stages: Vec<RuntimeLoopExecutionTraceStageRecord>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeLoopExecutionTraceStageRecord {
	pub stage: RuntimeLoopExecutionTraceStage,
	#[serde(default)]
	pub policy_decision: Option<PolicyDecision>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeLoopExecutionTraceStage {
	Canonicalized,
	PolicyDecided,
	ApprovalRequested,
	ApprovalResolved,
	ExecutionStarted,
	ExecutionFinished,
	ObservationRecorded,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeLoopTraceDecision {
	pub action: String,
	#[serde(default)]
	pub tool_name: Option<String>,
	#[serde(default)]
	pub arguments: Option<Value>,
	pub reason: String,
	#[serde(default)]
	pub final_message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeLoopTraceOutcome {
	pub status: String,
	#[serde(default)]
	pub terminal_action: Option<String>,
	#[serde(default)]
	pub final_message: Option<String>,
}

fn default_runtime_loop_trace_schema() -> String {
	RuntimeLoopTrace::schema_version().to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityToken {
	pub token_id: String,
	pub subject: String,
	pub resource: ResourceSelector,
	pub actions: Vec<String>,
	#[serde(default)]
	pub granted_capabilities: Vec<String>,
	pub expires_at_unix: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillExecutionMode {
	Advisory,
	Executable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SkillExecutionRequest {
	pub selected_skill: String,
	pub goal: String,
	#[serde(default)]
	pub execution_mode: Option<SkillExecutionMode>,
	#[serde(default)]
	pub allowed_script_paths: Vec<String>,
	#[serde(default)]
	pub allowed_output_root: Option<String>,
	#[serde(default)]
	pub expected_artifacts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SkillExecutionPlan {
	pub selected_skill: String,
	#[serde(default)]
	pub execution_mode: Option<SkillExecutionMode>,
	#[serde(default)]
	pub script_relpath: Option<String>,
	#[serde(default)]
	pub script_args: Vec<String>,
	#[serde(default)]
	pub generated_skill_name: Option<String>,
	#[serde(default)]
	pub expected_artifacts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SkillExecutionResult {
	pub selected_skill: String,
	#[serde(default)]
	pub execution_mode: Option<SkillExecutionMode>,
	pub success: bool,
	pub message: String,
	#[serde(default)]
	pub created_paths: Vec<String>,
	#[serde(default)]
	pub executed_scripts: Vec<String>,
	#[serde(default)]
	pub validation_status: Option<String>,
	#[serde(default)]
	pub generated_skill_name: Option<String>,
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
