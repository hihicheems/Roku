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

use std::collections::HashMap;
use std::env;
use std::sync::{Arc, Mutex};

use roku_agent_runtime::{
	AskUserPayload, AskUserResumeContract, AskUserResumeDirective, DirectRoutePlan, IntentFamily,
	LoopContext, LoopState, RouteDecision, RouteDecisionResult, RouteRisk, StepObservation,
	StepRecord, ToolObservation, runtime_loop_trace,
};
use roku_common_types::ResourceSelector;
use roku_common_types::{
	AggregationMode, ApprovalDecision, ApprovalId, ApprovalRequirement, ApprovalRequirementScope,
	ApprovalStatus, ApprovalTicket, ApprovedExecutionRef, CanonicalDigest, CanonicalExecution,
	ExecutionActionClass, ExecutionEnvPolicy, ExecutionEnvPolicyMode, ExecutionResourceScope,
	InvocationMode, JoinPolicy, NodeId, PendingExecutionApproval, PlanningModeHint, PolicyDecision,
	PolicyOutcome, PolicyReasonCode, RecoveryEligibility, RequestEnvelope, RequestId,
	ResponseStatus, ResultEnvelope, ResultStatus, RuntimeError, Task, TaskEdge, TaskGraph, TaskId,
	TaskNode, TaskNodeKind, TaskState,
};
use roku_memory::{
	DisabledMemoryLifecyclePolicy, InMemoryLongTermMemoryBackend, LongTermMemoryBackend,
	MemoryBackendHealth, MemoryBackendStatus, MemoryDeleteSelector, MemoryError, MemoryKind,
	MemoryLifecyclePolicy, MemoryQuery, MemoryRecallInput, MemoryScope, MemoryWriteAck,
	MemoryWritePolicyInput, MemoryWriteReason, MemoryWriteRequest,
};

use crate::{
	PendingLoopSnapshotStore, RuntimeExecutionMode, RuntimeMemoryLayers, RuntimeModeReport,
	RuntimeService, compact_approval_id,
};
use tempfile::tempdir;

#[derive(Default)]
struct RecordingPendingLoopSnapshotStore {
	snapshots: Mutex<HashMap<String, LoopState>>,
	events: Mutex<Vec<String>>,
	deleted_snapshots: Mutex<Vec<LoopState>>,
}

impl RecordingPendingLoopSnapshotStore {
	fn seed(&self, loop_state: LoopState) {
		self.snapshots
			.lock()
			.expect("snapshot seed lock should not be poisoned")
			.insert(loop_state.session_id.clone(), loop_state);
	}

	fn events(&self) -> Vec<String> {
		self.events
			.lock()
			.expect("event log lock should not be poisoned")
			.clone()
	}

	fn is_empty(&self) -> bool {
		self.snapshots
			.lock()
			.expect("snapshot store lock should not be poisoned")
			.is_empty()
	}

	fn deleted_snapshots(&self) -> Vec<LoopState> {
		self.deleted_snapshots
			.lock()
			.expect("deleted snapshot log lock should not be poisoned")
			.clone()
	}
}

impl PendingLoopSnapshotStore for RecordingPendingLoopSnapshotStore {
	fn load(&self, session_id: &str) -> Result<Option<LoopState>, RuntimeError> {
		self.events
			.lock()
			.expect("event log lock should not be poisoned")
			.push(format!("load:{session_id}"));
		Ok(self
			.snapshots
			.lock()
			.expect("snapshot store lock should not be poisoned")
			.get(session_id)
			.cloned())
	}

	fn store(&self, loop_state: &LoopState) -> Result<(), RuntimeError> {
		self.events
			.lock()
			.expect("event log lock should not be poisoned")
			.push(format!("store:{}", loop_state.session_id));
		self.snapshots
			.lock()
			.expect("snapshot store lock should not be poisoned")
			.insert(loop_state.session_id.clone(), loop_state.clone());
		Ok(())
	}

	fn delete(&self, session_id: &str) -> Result<(), RuntimeError> {
		self.events
			.lock()
			.expect("event log lock should not be poisoned")
			.push(format!("delete:{session_id}"));
		let removed = self
			.snapshots
			.lock()
			.expect("snapshot store lock should not be poisoned")
			.remove(session_id);
		if let Some(snapshot) = removed {
			self.deleted_snapshots
				.lock()
				.expect("deleted snapshot log lock should not be poisoned")
				.push(snapshot);
		}
		Ok(())
	}
}

fn request(goal: &str) -> RequestEnvelope {
	RequestEnvelope {
		request_id: RequestId("req-1".to_string()),
		session_id: "session-1".to_string(),
		goal: goal.to_string(),
		planning_mode_hint: None,
		conversation_history: Vec::new(),
	}
}

fn pending_filesystem_candidate_loop_state() -> LoopState {
	let cwd = env::current_dir().expect("cwd should resolve");
	let root_manifest = cwd.join("Cargo.toml").display().to_string();
	let nested_manifest = cwd
		.join("crates/roku-agent-runtime/Cargo.toml")
		.display()
		.to_string();
	let context = LoopContext {
		request_id: "req-pending-tool-loop".to_string(),
		session_id: "session-1".to_string(),
		goal: "帮我定位 Cargo.toml，然后告诉我这个 workspace 的 crate 组织".to_string(),
		workspace_root: cwd.display().to_string(),
		working_directory: cwd.display().to_string(),
		visible_tools: vec![
			"fs.read_text".to_string(),
			"fs.find".to_string(),
			"fs.inspect".to_string(),
		],
		bound_resources: vec![ResourceSelector::tool("fs.read_text".to_string())],
		route_decision: RouteDecision::new(
			IntentFamily::FilesystemRead,
			0.94,
			false,
			RouteRisk::Low,
			vec!["fs.read_text".to_string(), "fs.find".to_string()],
			Vec::new(),
			Vec::new(),
			"filesystem request",
		),
		last_observation: None,
	};
	let mut loop_state = LoopState::new("loop-pending-tool-loop", &context);
	let observation = ToolObservation {
		ok: false,
		tool_name: "fs.read_text".to_string(),
		error_type: Some("multiple_candidates".to_string()),
		terminal: false,
		data: serde_json::json!({
			"path": "Cargo.toml",
			"matches": [root_manifest, nested_manifest],
		}),
		message: "Found 2 matching candidates for `Cargo.toml`.".to_string(),
	};
	let interpreted =
		roku_agent_runtime::interpret_observation(&loop_state, observation.clone(), None);
	loop_state.record_step(StepRecord::tool_call(
		1,
		roku_agent_runtime::NextStepDecision {
			action: roku_agent_runtime::NextStepAction::CallTool,
			tool_name: Some("fs.read_text".to_string()),
			arguments: Some(serde_json::json!({ "path": "Cargo.toml" })),
			reason: "Read the grounded workspace manifest first.".to_string(),
			final_message: None,
		},
		loop_state.visible_tools.clone(),
		loop_state.bound_resources.clone(),
		serde_json::json!({
			"ok": false,
			"error_type": "multiple_candidates",
			"terminal": false,
			"message": "Found 2 matching candidates for `Cargo.toml`.",
			"data": observation.data.clone(),
		}),
		StepObservation::Tool(observation),
		interpreted.clone(),
		Some(12),
		interpreted.remaining_step_budget,
		interpreted.remaining_recovery_budget,
		cwd.display().to_string(),
	));
	loop_state.record_step(StepRecord::terminal(
		2,
		roku_agent_runtime::StepAction::AskUser,
		roku_agent_runtime::NextStepDecision {
			action: roku_agent_runtime::NextStepAction::AskUser,
			tool_name: None,
			arguments: None,
			reason: "Runtime paused for user clarification after the latest tool observation."
				.to_string(),
			final_message: Some("你想看哪一个 Cargo.toml？".to_string()),
		},
		loop_state.visible_tools.clone(),
		loop_state.bound_resources.clone(),
		Some(StepObservation::AskUser {
			final_message: "你想看哪一个 Cargo.toml？".to_string(),
		}),
		3,
		2,
		cwd.display().to_string(),
	));
	loop_state.awaiting_user = Some(AskUserPayload {
		final_message: "你想看哪一个 Cargo.toml？".to_string(),
		resume_contract: AskUserResumeContract::CandidateSelection {
			candidates: vec![
				cwd.join("Cargo.toml").display().to_string(),
				cwd.join("crates/roku-agent-runtime/Cargo.toml")
					.display()
					.to_string(),
			],
		},
		resume_directive: Some(AskUserResumeDirective::RepeatToolWithSelectedCandidate {
			tool_name: "fs.read_text".to_string(),
			argument_key: "path".to_string(),
		}),
	});
	loop_state
}

fn pending_filesystem_resume_success_loop_state() -> (LoopState, String) {
	let context = LoopContext {
		request_id: "req-pending-inventory-loop-success".to_string(),
		session_id: "session-1".to_string(),
		goal: "告诉我你当前暴露的 tools 和 skills，并按我选择的主题继续".to_string(),
		workspace_root: "/workspace".to_string(),
		working_directory: "/workspace".to_string(),
		visible_tools: vec!["inventory.describe".to_string()],
		bound_resources: vec![ResourceSelector::tool("inventory.describe".to_string())],
		route_decision: RouteDecision::new(
			IntentFamily::Chat,
			0.93,
			false,
			RouteRisk::Low,
			vec!["inventory.describe".to_string()],
			Vec::new(),
			Vec::new(),
			"inventory resume success request",
		),
		last_observation: None,
	};
	let mut loop_state = LoopState::new("loop-pending-inventory-loop-success", &context);
	let observation = ToolObservation {
		ok: false,
		tool_name: "inventory.describe".to_string(),
		error_type: Some("multiple_candidates".to_string()),
		terminal: false,
		data: serde_json::json!({
			"matches": ["tools", "skills"],
		}),
		message: "I can continue with either `tools` or `skills`.".to_string(),
	};
	let interpreted =
		roku_agent_runtime::interpret_observation(&loop_state, observation.clone(), None);
	loop_state.record_step(StepRecord::tool_call(
		1,
		roku_agent_runtime::NextStepDecision {
			action: roku_agent_runtime::NextStepAction::CallTool,
			tool_name: Some("inventory.describe".to_string()),
			arguments: Some(serde_json::json!({})),
			reason: "Inspect the runtime inventory before answering.".to_string(),
			final_message: None,
		},
		loop_state.visible_tools.clone(),
		loop_state.bound_resources.clone(),
		serde_json::json!({
			"ok": false,
			"error_type": "multiple_candidates",
			"terminal": false,
			"message": "I can continue with either `tools` or `skills`.",
			"data": observation.data.clone(),
		}),
		StepObservation::Tool(observation),
		interpreted.clone(),
		Some(12),
		interpreted.remaining_step_budget,
		interpreted.remaining_recovery_budget,
		"/workspace",
	));
	loop_state.record_step(StepRecord::terminal(
		2,
		roku_agent_runtime::StepAction::AskUser,
		roku_agent_runtime::NextStepDecision {
			action: roku_agent_runtime::NextStepAction::AskUser,
			tool_name: None,
			arguments: None,
			reason: "Runtime paused for user clarification after the latest tool observation."
				.to_string(),
			final_message: Some("你想继续看 `tools` 还是 `skills`？".to_string()),
		},
		loop_state.visible_tools.clone(),
		loop_state.bound_resources.clone(),
		Some(StepObservation::AskUser {
			final_message: "你想继续看 `tools` 还是 `skills`？".to_string(),
		}),
		3,
		2,
		"/workspace",
	));
	loop_state.awaiting_user = Some(AskUserPayload {
		final_message: "你想继续看 `tools` 还是 `skills`？".to_string(),
		resume_contract: AskUserResumeContract::CandidateSelection {
			candidates: vec!["tools".to_string(), "skills".to_string()],
		},
		resume_directive: Some(AskUserResumeDirective::RepeatToolWithSelectedCandidate {
			tool_name: "inventory.describe".to_string(),
			argument_key: "topic".to_string(),
		}),
	});
	(loop_state, "tools".to_string())
}

fn node(node_id: &str, kind: TaskNodeKind) -> TaskNode {
	TaskNode {
		node_id: NodeId(node_id.to_string()),
		kind,
		description: node_id.to_string(),
		capabilities: Vec::new(),
		join_policy: JoinPolicy::AllParents,
		aggregation_mode: AggregationMode::CollectAll,
		..TaskNode::default()
	}
}

fn execution_result(task_id: &TaskId, node_id: &str, message: &str) -> ResultEnvelope {
	ResultEnvelope {
		task_id: task_id.clone(),
		node_id: NodeId(node_id.to_string()),
		producer: format!("legacy:{node_id}"),
		schema_version: "result.v1".to_string(),
		status: ResultStatus::Ok,
		payload: serde_json::json!({
			"message": message,
		})
		.to_string(),
		evidence: Vec::new(),
		confidence: 0.9,
	}
}

fn graph_task(
	task_id: &str,
	request_id: &str,
	goal: &str,
	state: TaskState,
	graph: TaskGraph,
) -> Task {
	Task {
		task_id: TaskId(task_id.to_string()),
		request_id: RequestId(request_id.to_string()),
		session_id: "session-1".to_string(),
		goal: goal.to_string(),
		state,
		attempts: 0,
		planning_mode_hint: None,
		conversation_history: Vec::new(),
		completed_nodes: Vec::new(),
		next_node_index: 0,
		pending_approval_id: None,
		last_result: None,
		compensation_records: Vec::new(),
		graph: Some(graph),
	}
}

fn sample_execution_policy_decision() -> PolicyDecision {
	PolicyDecision {
		outcome: PolicyOutcome::RequireApproval,
		reason_code: PolicyReasonCode::ApprovalRequiredByUntrustedProgram,
		approval_requirement: Some(ApprovalRequirement {
			scope: ApprovalRequirementScope::Invocation,
			reason_code: PolicyReasonCode::ApprovalRequiredByUntrustedProgram,
		}),
	}
}

fn sample_frozen_command_execution(cwd: &str, digest: &str) -> CanonicalExecution {
	CanonicalExecution {
		tool_name: "command.run".to_string(),
		program: "pwd".to_string(),
		argv: vec!["pwd".to_string()],
		invocation_mode: InvocationMode::DirectExec,
		shell_context: None,
		cwd: cwd.to_string(),
		env_policy: ExecutionEnvPolicy {
			mode: ExecutionEnvPolicyMode::InheritSelected,
			allowed_keys: vec![
				"ROKU_COMMAND_SCOPE_ROOT".to_string(),
				"ROKU_ALLOWED_READ_ROOTS".to_string(),
			],
		},
		resource_scope: ExecutionResourceScope {
			working_directory: cwd.to_string(),
			resolved_targets: Vec::new(),
			effective_read_roots: vec![cwd.to_string()],
			effective_write_roots: Vec::new(),
		},
		action_class: ExecutionActionClass::Exec,
		digest: CanonicalDigest(digest.to_string()),
	}
}

fn persist_frozen_execution_snapshot_artifact(
	service: &RuntimeService,
	task_id: &TaskId,
	node_id: &NodeId,
	digest: &CanonicalDigest,
	payload: &str,
) -> String {
	service
		.lock_state()
		.expect("runtime state lock should succeed")
		.artifact_store
		.persist_frozen_execution_snapshot_artifact(task_id, node_id, digest, "result.v1", payload)
		.expect("frozen execution snapshot should persist")
		.uri
}

struct FailingRecallBackend;

impl LongTermMemoryBackend for FailingRecallBackend {
	fn backend_name(&self) -> &'static str {
		"failing-recall"
	}

	fn search(&self, _query: &MemoryQuery) -> Result<Vec<roku_memory::MemoryHit>, MemoryError> {
		Err(MemoryError::Unavailable(
			"simulated recall outage".to_string(),
		))
	}

	fn write(&self, _request: &MemoryWriteRequest) -> Result<MemoryWriteAck, MemoryError> {
		Ok(MemoryWriteAck {
			accepted: true,
			record_id: Some("memory-record-1".to_string()),
		})
	}

	fn delete(&self, _selector: &MemoryDeleteSelector) -> Result<(), MemoryError> {
		Ok(())
	}

	fn health(&self) -> Result<MemoryBackendHealth, MemoryError> {
		Ok(MemoryBackendHealth {
			backend: self.backend_name().to_string(),
			status: MemoryBackendStatus::Degraded,
			detail: Some("simulated recall outage".to_string()),
		})
	}
}

struct AlwaysWriteMemoryPolicy;

impl MemoryLifecyclePolicy for AlwaysWriteMemoryPolicy {
	fn build_recall_query(&self, _input: &MemoryRecallInput) -> Option<MemoryQuery> {
		None
	}

	fn build_write_request(&self, input: &MemoryWritePolicyInput) -> Option<MemoryWriteRequest> {
		if input.response_status != ResponseStatus::Succeeded {
			return None;
		}

		let mut request = MemoryWriteRequest::new(
			MemoryKind::HistoricalCase,
			MemoryScope::Session,
			format!("goal={} response={}", input.goal, input.response_message),
			"Successful runtime response".to_string(),
			MemoryWriteReason::TaskSucceeded,
		);
		request.session_id = Some(input.session_id.clone());
		Some(request)
	}
}

#[test]
fn context_bundle_separates_short_term_continuity_from_long_term_hits() {
	let backend = Arc::new(InMemoryLongTermMemoryBackend::default());
	let mut seed = MemoryWriteRequest::new(
		MemoryKind::UserPreference,
		MemoryScope::Session,
		"User prefers Rust code snippets.",
		"Rust preference".to_string(),
		MemoryWriteReason::OperatorRequested,
	);
	seed.session_id = Some("session-1".to_string());
	backend
		.write(&seed)
		.expect("seed long-term memory write should succeed");

	let service = RuntimeService::default().with_long_term_memory_backend(backend);
	let request = RequestEnvelope {
		request_id: RequestId("req-memory-context".to_string()),
		session_id: "session-1".to_string(),
		goal: "Rust preference".to_string(),
		planning_mode_hint: None,
		conversation_history: vec![roku_common_types::ConversationTurn {
			role: roku_common_types::ConversationRole::User,
			content: "Please use concise answers.".to_string(),
			created_at_unix_ms: 0,
		}],
	};

	let bundle = service
		.build_context_bundle(&request, false)
		.expect("context bundle should build");

	assert_eq!(request.conversation_history.len(), 1);
	assert_eq!(bundle.short_term_continuity.len(), 1);
	assert_eq!(bundle.long_term_memory_hits.len(), 1);
	assert_eq!(
		bundle.long_term_memory_hits[0].record.summary,
		"Rust preference"
	);
}

#[test]
fn context_bundle_renders_continuity_and_recall_as_named_memory_sections() {
	let backend = Arc::new(InMemoryLongTermMemoryBackend::default());
	let mut seed = MemoryWriteRequest::new(
		MemoryKind::UserPreference,
		MemoryScope::Session,
		"User prefers Rust snippets.",
		"Rust preference".to_string(),
		MemoryWriteReason::OperatorRequested,
	);
	seed.session_id = Some("session-1".to_string());
	backend
		.write(&seed)
		.expect("seed long-term memory write should succeed");

	let service = RuntimeService::default().with_long_term_memory_backend(backend);
	let request = RequestEnvelope {
		request_id: RequestId("req-memory-context".to_string()),
		session_id: "session-1".to_string(),
		goal: "Rust preference".to_string(),
		planning_mode_hint: None,
		conversation_history: vec![roku_common_types::ConversationTurn {
			role: roku_common_types::ConversationRole::User,
			content: "Please use concise answers.".to_string(),
			created_at_unix_ms: 0,
		}],
	};

	let bundle = service
		.build_context_bundle(&request, false)
		.expect("context bundle should build");
	let memory_context_text = bundle.memory_context_text();

	assert_eq!(
		memory_context_text,
		"Short-term continuity:\n- user: Please use concise answers.\n\nLong-term recall:\n- memory-record-1 | UserPreference | Rust preference"
	);
	assert!(!memory_context_text.is_empty());
	assert_eq!(bundle.short_term_continuity.len(), 1);
	assert_eq!(request.conversation_history.len(), 1);
}

#[test]
fn runtime_memory_layers_keep_continuity_recall_and_working_memory_distinct() {
	let backend = Arc::new(InMemoryLongTermMemoryBackend::default());
	let mut seed = MemoryWriteRequest::new(
		MemoryKind::UserPreference,
		MemoryScope::Session,
		"User prefers Rust snippets.",
		"Rust preference".to_string(),
		MemoryWriteReason::OperatorRequested,
	);
	seed.session_id = Some("session-1".to_string());
	backend
		.write(&seed)
		.expect("seed long-term memory write should succeed");

	let service = RuntimeService::default().with_long_term_memory_backend(backend);
	let request = RequestEnvelope {
		request_id: RequestId("req-memory-layers".to_string()),
		session_id: "session-1".to_string(),
		goal: "Rust preference".to_string(),
		planning_mode_hint: None,
		conversation_history: vec![roku_common_types::ConversationTurn {
			role: roku_common_types::ConversationRole::User,
			content: "Please use concise answers.".to_string(),
			created_at_unix_ms: 0,
		}],
	};

	let bundle = service
		.build_context_bundle(&request, false)
		.expect("context bundle should build");
	let layers = bundle
		.runtime_memory_layers_with_working_memory("WORKING_MEMORY_ONLY::follow the runtime seam");

	assert_eq!(
		layers.short_term_continuity, request.conversation_history,
		"continuity should stay in its own layer"
	);
	assert_eq!(layers.long_term_recall.len(), 1);
	assert_eq!(
		layers.long_term_recall[0].record.summary, "Rust preference",
		"recall should stay in its own layer"
	);
	assert_eq!(
		layers.working_memory, "WORKING_MEMORY_ONLY::follow the runtime seam",
		"working memory should stay in its own layer"
	);
	assert_eq!(
		bundle.runtime_memory_layers(),
		RuntimeMemoryLayers::new(
			request.conversation_history.clone(),
			layers.long_term_recall.clone(),
			""
		),
		"fresh request assembly should default to an empty working-memory layer"
	);
	assert!(
		!bundle.memory_context_text().contains("Working memory:"),
		"fresh request assembly should not render an empty working-memory section"
	);

	let sections = layers.structured_sections();
	assert_eq!(
		sections.short_term_continuity, "- user: Please use concise answers.",
		"continuity should stay distinguishable before any legacy string rendering"
	);
	assert_eq!(
		sections.long_term_recall, "- memory-record-1 | UserPreference | Rust preference",
		"recall should stay distinguishable before any legacy string rendering"
	);
	assert_eq!(
		sections.working_memory, "WORKING_MEMORY_ONLY::follow the runtime seam",
		"working memory should stay distinguishable before any legacy string rendering"
	);

	let memory_context_text = layers.memory_context_text();
	assert!(
		memory_context_text.contains("Short-term continuity:"),
		"continuity section should be named in assembled output"
	);
	assert!(
		memory_context_text.contains("- user: Please use concise answers."),
		"continuity content should remain distinguishable in assembled output"
	);
	assert!(
		memory_context_text.contains("Long-term recall:"),
		"recall section should be named in assembled output"
	);
	assert!(
		memory_context_text.contains("- memory-record-1 | UserPreference | Rust preference"),
		"recall content should remain distinguishable in assembled output"
	);
	assert!(
		memory_context_text.contains("Working memory:"),
		"working-memory section should be named in assembled output"
	);
	assert!(
		memory_context_text.contains("WORKING_MEMORY_ONLY::follow the runtime seam"),
		"working-memory content should remain distinguishable in assembled output"
	);
}

#[test]
fn resumed_pending_loops_project_bound_resources_into_context_bundle() {
	let service = RuntimeService::default();
	let cwd = env::current_dir().expect("cwd should resolve");
	let context = LoopContext {
		request_id: "req-pending-context".to_string(),
		session_id: "session-1".to_string(),
		goal: "继续".to_string(),
		workspace_root: cwd.display().to_string(),
		working_directory: cwd.display().to_string(),
		visible_tools: vec!["fs.read_text".to_string()],
		bound_resources: vec![ResourceSelector::tool("fs.read_text".to_string())],
		route_decision: RouteDecision::new(
			IntentFamily::FilesystemRead,
			0.91,
			false,
			RouteRisk::Low,
			vec!["fs.read_text".to_string()],
			Vec::new(),
			Vec::new(),
			"resume request",
		),
		last_observation: None,
	};
	let loop_state = LoopState::new("loop-pending-context", &context);
	let mut bundle = service
		.build_context_bundle(&request("继续"), true)
		.expect("context bundle should build");

	service.attach_resumed_loop_resources(&mut bundle, &loop_state);

	assert_eq!(
		bundle.visible_resources,
		vec![ResourceSelector::tool("fs.read_text".to_string())]
	);
	assert!(bundle.pending_loop_active);
}

#[test]
fn direct_routes_project_bound_resources_into_context_bundle() {
	let service = RuntimeService::default();
	let mut bundle = service
		.build_context_bundle(
			&request("Read Cargo.toml and explain the workspace layout."),
			false,
		)
		.expect("context bundle should build");
	let route = RouteDecisionResult::Direct(DirectRoutePlan {
		decision: RouteDecision::new(
			IntentFamily::FilesystemRead,
			0.96,
			false,
			RouteRisk::Low,
			vec!["fs.read_text".to_string(), "fs.find".to_string()],
			Vec::new(),
			Vec::new(),
			"direct resource projection request",
		),
		bound_resources: vec![
			ResourceSelector::tool("fs.read_text".to_string()),
			ResourceSelector::tool("fs.find".to_string()),
		],
	});

	service.attach_visible_resources(&mut bundle, &route);

	assert_eq!(
		bundle.visible_resources,
		vec![
			ResourceSelector::tool("fs.read_text".to_string()),
			ResourceSelector::tool("fs.find".to_string()),
		]
	);
	assert!(!bundle.pending_loop_active);
}

#[test]
fn compact_approval_id_stays_short_for_telegram_callbacks() {
	let approval_id = compact_approval_id("task-tg-919471825", "request-approval");

	assert!(approval_id.len() <= 19);
	assert!(format!("ap:a:{approval_id}").len() <= 64);
}

#[test]
fn new_requests_execute_without_graph_compilation_and_expose_direct_runtime_markers() {
	let service = RuntimeService::default();
	let response = service
		.execute(request("What skills and tools do you have right now?"))
		.expect("direct request should succeed");

	assert_eq!(
		response.status,
		ResponseStatus::Succeeded,
		"unexpected resume response: {response:?}"
	);

	let task = service
		.get_task(&TaskId("task-req-1".to_string()))
		.expect("task lookup should succeed")
		.expect("task should be persisted");
	assert!(task.graph.is_none());
	let last_result = task
		.last_result
		.as_ref()
		.expect("direct runtime execution should persist a terminal result");
	let payload: serde_json::Value =
		serde_json::from_str(&last_result.payload).expect("payload should be valid json");
	let trace: roku_common_types::RuntimeLoopTrace =
		serde_json::from_value(payload["probe_trace"].clone()).expect("probe trace should decode");
	assert_eq!(payload["runtime_loop"], "tool");
	assert_eq!(trace.status, "succeeded");
	assert_eq!(
		trace.final_outcome.terminal_action.as_deref(),
		Some("final_answer")
	);
	assert_eq!(
		trace
			.steps
			.first()
			.map(|step| step.decision.action.as_str()),
		Some("call_tool")
	);
	assert_eq!(
		trace
			.steps
			.first()
			.and_then(|step| step.decision.tool_name.as_deref()),
		Some("inventory.describe")
	);
	assert!(trace.steps.first().is_some_and(|step| {
		step.visible_tools_before
			.iter()
			.any(|tool| tool == "inventory.describe")
	}));
	assert_eq!(task.state, TaskState::Succeeded);
	let experiment = service
		.get_experiment_run(&TaskId("task-req-1".to_string()))
		.expect("experiment lookup should succeed")
		.expect("direct request should record an experiment run");
	assert_eq!(experiment.strategy, "direct_route");
}

#[test]
fn runtime_service_defaults_to_deterministic_runtime_mode() {
	let service = RuntimeService::default();
	let report = service.runtime_mode_report();

	assert_eq!(report.requested, RuntimeExecutionMode::Deterministic);
	assert_eq!(report.effective, RuntimeExecutionMode::Deterministic);
	assert_eq!(report.fallback_reason, None);
}

#[test]
fn runtime_service_can_expose_live_fallback_mode_report() {
	let service = RuntimeService::default().with_runtime_mode_report(
		RuntimeModeReport::live_react_fallback_to_deterministic(
			"openrouter plugin disabled by startup policy",
		),
	);
	let report = service.runtime_mode_report();

	assert_eq!(report.requested, RuntimeExecutionMode::LiveReact);
	assert_eq!(report.effective, RuntimeExecutionMode::Deterministic);
	assert_eq!(
		report.fallback_reason.as_deref(),
		Some("openrouter plugin disabled by startup policy")
	);
}

#[test]
fn recall_failures_do_not_block_direct_execution() {
	let service =
		RuntimeService::default().with_long_term_memory_backend(Arc::new(FailingRecallBackend));

	let response = service
		.execute(request("What skills and tools do you have right now?"))
		.expect("direct request should still succeed");

	assert_eq!(response.status, ResponseStatus::Succeeded);
}

#[test]
fn successful_requests_write_back_when_policy_effectively_enables_it() {
	let backend = Arc::new(InMemoryLongTermMemoryBackend::default());
	let service = RuntimeService::default()
		.with_long_term_memory_backend(backend.clone())
		.with_memory_lifecycle_policy(Arc::new(AlwaysWriteMemoryPolicy));

	let response = service
		.execute(request("What skills and tools do you have right now?"))
		.expect("direct request should succeed");

	assert_eq!(response.status, ResponseStatus::Succeeded);
	assert_eq!(backend.recorded_writes().len(), 1);
	assert_eq!(backend.stored_records().len(), 1);
}

#[test]
fn successful_requests_skip_write_back_when_policy_effectively_disables_it() {
	let backend = Arc::new(InMemoryLongTermMemoryBackend::default());
	let service = RuntimeService::default()
		.with_long_term_memory_backend(backend.clone())
		.with_memory_lifecycle_policy(Arc::new(DisabledMemoryLifecyclePolicy));

	let response = service
		.execute(request("What skills and tools do you have right now?"))
		.expect("direct request should succeed");

	assert_eq!(response.status, ResponseStatus::Succeeded);
	assert!(backend.recorded_writes().is_empty());
	assert!(backend.stored_records().is_empty());
}

#[test]
fn planning_mode_hint_returns_compatibility_fallback_without_graph() {
	let service = RuntimeService::default();
	let mut request = request("Read the first part of Cargo.toml.");
	request.planning_mode_hint = Some(PlanningModeHint::TreeSearch);

	let response = service
		.execute(request)
		.expect("compatibility fallback should succeed");

	assert_eq!(response.status, ResponseStatus::Succeeded);
	assert!(response.message.contains("planning-heavy"));

	let task = service
		.get_task(&TaskId("task-req-1".to_string()))
		.expect("task lookup should succeed")
		.expect("task should be persisted");
	assert!(task.graph.is_none());
}

#[test]
fn planning_mode_hints_clear_pending_loop_snapshots_without_resuming() {
	let store = Arc::new(RecordingPendingLoopSnapshotStore::default());
	store.seed(pending_filesystem_candidate_loop_state());
	let service = RuntimeService::default().with_pending_loop_snapshot_store(store.clone());
	let mut request = request("Read the first part of Cargo.toml.");
	request.planning_mode_hint = Some(PlanningModeHint::TreeSearch);

	let response = service
		.execute(request)
		.expect("compatibility fallback should bypass pending-loop resume");

	assert_eq!(response.status, ResponseStatus::Succeeded);
	assert_eq!(
		store.events(),
		vec![
			"delete:session-1".to_string(),
			"delete:session-1".to_string(),
		]
	);
	assert!(store.is_empty());
}

#[test]
fn multistep_requests_enter_the_generic_loop_for_new_requests() {
	let service = RuntimeService::default();
	let response = service
		.execute(request(
			"Compare two migration strategies and then execute the better one.",
		))
		.expect("multistep request should still execute through the direct runtime");

	assert_eq!(response.status, ResponseStatus::Succeeded);
	assert!(!response.message.contains("planning-heavy"));

	let task = service
		.get_task(&TaskId("task-req-1".to_string()))
		.expect("task lookup should succeed")
		.expect("task should be persisted");
	assert!(task.graph.is_none());
	let last_result = task
		.last_result
		.as_ref()
		.expect("direct loop execution should persist a terminal result");
	let payload: serde_json::Value =
		serde_json::from_str(&last_result.payload).expect("payload should be valid json");
	assert_eq!(payload["runtime_loop"], "tool");
}

#[test]
fn pending_filesystem_tool_loops_survive_resume_through_the_generic_loop_driver_when_work_fails() {
	let service = RuntimeService::default();
	let loop_state = pending_filesystem_candidate_loop_state();
	service
		.restore_pending_loop(loop_state)
		.expect("pending loop should restore");

	let response = service
		.execute(request("Cargo.toml"))
		.expect("pending loop should resume");

	assert_eq!(response.status, ResponseStatus::Failed);
	assert!(
		response
			.message
			.contains("general execution did not use a live runtime")
	);
	assert!(
		service
			.pending_loop("session-1")
			.expect("pending loop lookup should succeed")
			.is_none()
	);

	let task = service
		.get_task(&TaskId("task-req-1".to_string()))
		.expect("task lookup should succeed")
		.expect("task should be persisted");
	assert_eq!(task.state, TaskState::Failed);
	assert!(task.last_result.is_none());
	assert!(!response.artifacts.is_empty());
}

#[test]
fn configured_pending_loop_snapshot_store_survives_generic_loop_resume_failures() {
	let store = Arc::new(RecordingPendingLoopSnapshotStore::default());
	store.seed(pending_filesystem_candidate_loop_state());
	let service = RuntimeService::default().with_pending_loop_snapshot_store(store.clone());

	let response = service
		.execute(request("Cargo.toml"))
		.expect("pending loop should resume from the configured snapshot store");

	assert_eq!(response.status, ResponseStatus::Failed);
	assert!(
		response
			.message
			.contains("general execution did not use a live runtime")
	);
	let events = store.events();
	assert_eq!(events.first().map(String::as_str), Some("load:session-1"));
	assert!(events.iter().any(|event| event == "delete:session-1"));
	assert!(store.is_empty());
}

#[test]
fn reconstructed_service_instances_survive_generic_pending_loop_failures_from_shared_snapshot_store()
 {
	let store = Arc::new(RecordingPendingLoopSnapshotStore::default());
	let writer = RuntimeService::default().with_pending_loop_snapshot_store(store.clone());
	writer
		.restore_pending_loop(pending_filesystem_candidate_loop_state())
		.expect("pending loop should persist before service reconstruction");
	drop(writer);

	let service = RuntimeService::default().with_pending_loop_snapshot_store(store.clone());
	let response = service
		.execute(request("Cargo.toml"))
		.expect("reconstructed service should resume the stored pending loop");

	assert_eq!(response.status, ResponseStatus::Failed);
	assert!(
		response
			.message
			.contains("general execution did not use a live runtime")
	);
	assert!(!response.artifacts.is_empty());

	let task_id = TaskId("task-req-1".to_string());
	let task = service
		.get_task(&task_id)
		.expect("task lookup should succeed")
		.expect("resumed task should be persisted");
	assert_eq!(task.state, TaskState::Failed);
	assert!(task.graph.is_none());

	let experiment = service
		.get_experiment_run(&task_id)
		.expect("experiment lookup should succeed")
		.expect("resumed task should record an experiment run");
	assert_eq!(experiment.strategy, "runtime_loop_resume");

	assert_eq!(
		store.events(),
		vec![
			"store:session-1".to_string(),
			"load:session-1".to_string(),
			"delete:session-1".to_string(),
			"delete:session-1".to_string(),
		]
	);
	assert!(store.is_empty());
}

#[test]
fn reconstructed_service_instances_resume_generic_pending_loops_to_success_from_shared_snapshot_store()
 {
	let store = Arc::new(RecordingPendingLoopSnapshotStore::default());
	let (pending_loop, selected_manifest) = pending_filesystem_resume_success_loop_state();
	let writer = RuntimeService::default().with_pending_loop_snapshot_store(store.clone());
	writer
		.restore_pending_loop(pending_loop)
		.expect("pending loop should persist before service reconstruction");
	drop(writer);

	let service = RuntimeService::default().with_pending_loop_snapshot_store(store.clone());
	let response = service
		.execute(request(&selected_manifest))
		.expect("reconstructed service should resume the stored pending loop to success");

	assert_eq!(
		response.status,
		ResponseStatus::Succeeded,
		"unexpected resume response: {response:?}"
	);
	let task_id = TaskId("task-req-1".to_string());
	let task = service
		.get_task(&task_id)
		.expect("task lookup should succeed")
		.expect("resumed task should be persisted");
	assert_eq!(task.state, TaskState::Succeeded);
	assert!(task.graph.is_none());
	let last_result = task
		.last_result
		.as_ref()
		.expect("successful resumed task should persist a terminal result");
	let payload = serde_json::from_str::<serde_json::Value>(&last_result.payload)
		.expect("terminal payload should decode");
	let trace: roku_common_types::RuntimeLoopTrace =
		serde_json::from_value(payload["probe_trace"].clone()).expect("probe trace should decode");
	assert_eq!(trace.status, "succeeded");
	assert_eq!(
		trace.final_outcome.terminal_action.as_deref(),
		Some("final_answer")
	);
	assert!(trace.steps.iter().all(|step| {
		step.visible_resources_before.as_ref()
			== Some(&vec![ResourceSelector::tool(
				"inventory.describe".to_string(),
			)])
	}));

	let experiment = service
		.get_experiment_run(&task_id)
		.expect("experiment lookup should succeed")
		.expect("resumed task should record an experiment run");
	assert_eq!(experiment.strategy, "runtime_loop_resume");

	assert_eq!(
		store.events(),
		vec![
			"store:session-1".to_string(),
			"load:session-1".to_string(),
			"delete:session-1".to_string(),
			"delete:session-1".to_string(),
		]
	);
	assert!(store.is_empty());
}

#[test]
fn stale_freeform_pending_loops_are_discarded_before_new_intake() {
	let store = Arc::new(RecordingPendingLoopSnapshotStore::default());
	let backend = Arc::new(InMemoryLongTermMemoryBackend::default());
	let mut memory = MemoryWriteRequest::new(
		MemoryKind::UserPreference,
		MemoryScope::Session,
		"User prefers Rust snippets.",
		"Rust preference".to_string(),
		MemoryWriteReason::OperatorRequested,
	);
	memory.session_id = Some("session-1".to_string());
	backend
		.write(&memory)
		.expect("seed long-term memory write should succeed");
	let service = RuntimeService::default()
		.with_long_term_memory_backend(backend.clone())
		.with_pending_loop_snapshot_store(store.clone());
	let cwd = env::current_dir().expect("cwd should resolve");
	let context = LoopContext {
		request_id: "req-freeform-pending".to_string(),
		session_id: "session-1".to_string(),
		goal: "继续".to_string(),
		workspace_root: cwd.display().to_string(),
		working_directory: cwd.display().to_string(),
		visible_tools: vec![
			"general.execute".to_string(),
			"inventory.describe".to_string(),
			"fs.find".to_string(),
		],
		bound_resources: vec![ResourceSelector::tool("general.execute".to_string())],
		route_decision: RouteDecision::new(
			IntentFamily::Chat,
			0.88,
			false,
			RouteRisk::Low,
			vec!["general.execute".to_string()],
			Vec::new(),
			Vec::new(),
			"freeform clarification request",
		),
		last_observation: None,
	};
	let freeform_pause = AskUserPayload::freeform("您想继续什么任务？");
	let pause_message = freeform_pause.final_message.clone();
	let mut loop_state = LoopState::new("loop-freeform-pending", &context);
	loop_state.status = roku_agent_runtime::LoopStatus::AwaitingUser;
	loop_state.awaiting_user = Some(freeform_pause.clone());
	assert_eq!(
		freeform_pause.resume_contract,
		AskUserResumeContract::NoAutomaticResume
	);
	let assessment = service
		.runtime
		.assess_awaiting_user_resume(&loop_state, "What skills and tools do you have right now?");
	assert!(!assessment.should_resume);
	assert!(
		assessment.reason.contains("fresh intake"),
		"expected stale freeform pause to be discarded as a fresh intake, got: {}",
		assessment.reason
	);
	service
		.restore_pending_loop(loop_state)
		.expect("pending loop should restore");

	let response = service
		.execute(request("What skills and tools do you have right now?"))
		.expect("fresh intake should succeed");

	assert_eq!(response.status, ResponseStatus::Succeeded);
	assert_ne!(response.message, pause_message);
	assert_eq!(backend.recorded_queries().len(), 1);
	assert!(
		service
			.pending_loop("session-1")
			.expect("pending loop lookup should succeed")
			.is_none()
	);
	let task = service
		.get_task(&TaskId("task-req-1".to_string()))
		.expect("task lookup should succeed")
		.expect("task should be persisted");
	assert_eq!(task.state, TaskState::Succeeded);
	assert!(task.last_result.is_some());
	let discarded = store.deleted_snapshots();
	let stopped_snapshot = discarded
		.iter()
		.find(|snapshot| snapshot.status == roku_agent_runtime::LoopStatus::Stopped)
		.expect("stale discard should persist one stopped snapshot before deletion");
	let trace = runtime_loop_trace(stopped_snapshot);
	assert_eq!(trace.status, "stopped");
	assert_eq!(trace.final_outcome.terminal_action.as_deref(), Some("stop"));
	assert!(
		trace
			.final_outcome
			.final_message
			.as_deref()
			.expect("stop trace should include a terminal message")
			.contains("fresh intake")
	);
	assert_eq!(
		trace
			.steps
			.last()
			.and_then(|step| step.visible_resources_before.clone()),
		Some(vec![ResourceSelector::tool("general.execute".to_string())])
	);
}

#[test]
fn historical_graph_tasks_remain_resumable() {
	let service = RuntimeService::default();
	let task_id = TaskId("task-legacy-resume".to_string());
	let graph = TaskGraph {
		task_id: task_id.clone(),
		nodes: vec![node("legacy-step", TaskNodeKind::Execution)],
		edges: Vec::new(),
	};
	let task = graph_task(
		&task_id.0,
		"req-legacy-resume",
		"Resume the old graph task",
		TaskState::Failed,
		graph,
	);

	service
		.start_experiment_run(&task, &task.goal, "legacy_graph")
		.expect("experiment should start");
	service.save_task(task).expect("task should persist");

	let report = service
		.get_task_replay_report(&task_id)
		.expect("replay report lookup should succeed")
		.expect("replay report should exist");
	assert_eq!(
		report.recovery_eligibility,
		RecoveryEligibility::ResumeReady
	);
	assert!(!report.resume_candidates.is_empty());

	let response = service
		.resume_task(&task_id)
		.expect("historical graph task should resume");
	assert_eq!(response.status, ResponseStatus::Succeeded);

	let resumed = service
		.get_task(&task_id)
		.expect("task lookup should succeed")
		.expect("task should exist");
	assert_eq!(resumed.state, TaskState::Succeeded);
	assert!(resumed.graph.is_some());
}

#[test]
fn historical_waiting_approval_graph_tasks_can_finish_after_decision() {
	let service = RuntimeService::default();
	let task_id = TaskId("task-legacy-approval".to_string());
	let request_id = RequestId("req-legacy-approval".to_string());
	let execution_node = node("legacy-step", TaskNodeKind::Execution);
	let approval_node = node("legacy-approval", TaskNodeKind::Approval);
	let approval_id = ApprovalId(compact_approval_id(&task_id.0, &approval_node.node_id.0));
	let graph = TaskGraph {
		task_id: task_id.clone(),
		nodes: vec![execution_node.clone(), approval_node.clone()],
		edges: vec![TaskEdge {
			from: execution_node.node_id.clone(),
			to: approval_node.node_id.clone(),
			condition: roku_common_types::TaskEdgeCondition::OnSuccess,
		}],
	};
	let mut task = graph_task(
		&task_id.0,
		&request_id.0,
		"Approve the historical graph task",
		TaskState::WaitingApproval,
		graph,
	);
	task.completed_nodes = vec![execution_node.node_id.clone()];
	task.next_node_index = 1;
	task.pending_approval_id = Some(approval_id.clone());
	task.last_result = Some(execution_result(
		&task_id,
		&execution_node.node_id.0,
		"execution finished",
	));

	service
		.start_experiment_run(&task, &task.goal, "legacy_graph")
		.expect("experiment should start");
	service.save_task(task).expect("task should persist");
	service
		.save_result(execution_result(
			&task_id,
			&execution_node.node_id.0,
			"execution finished",
		))
		.expect("execution result should persist");
	service
		.save_approval_ticket(ApprovalTicket {
			approval_id: approval_id.clone(),
			task_id: task_id.clone(),
			request_id: request_id.clone(),
			node_id: approval_node.node_id.clone(),
			summary: approval_node.description.clone(),
			status: ApprovalStatus::Pending,
			decided_by: None,
			comment: None,
			pending_execution: None,
		})
		.expect("approval ticket should persist");

	let replay = service
		.get_task_replay_report(&task_id)
		.expect("replay report lookup should succeed")
		.expect("replay report should exist");
	assert_eq!(
		replay.recovery_eligibility,
		RecoveryEligibility::PendingApproval
	);

	let response = service
		.decide_approval(
			&approval_id,
			ApprovalDecision {
				actor: "reviewer".to_string(),
				approved: true,
				comment: Some("approved".to_string()),
			},
		)
		.expect("approval should complete the historical graph task");
	assert_eq!(response.status, ResponseStatus::Succeeded);

	let task = service
		.get_task(&task_id)
		.expect("task lookup should succeed")
		.expect("task should exist");
	assert_eq!(task.state, TaskState::Succeeded);
	assert!(task.graph.is_some());
}

#[test]
fn execution_approval_tickets_resume_frozen_command_and_preserve_digest() {
	let service = RuntimeService::default();
	let directory = tempdir().expect("tempdir should succeed");
	let cwd = directory
		.path()
		.canonicalize()
		.expect("tempdir should canonicalize");
	let cwd_text = cwd.display().to_string();
	let task_id = TaskId("task-execution-approval".to_string());
	let request_id = RequestId("req-execution-approval".to_string());
	let execution_node = TaskNode {
		description: "Step: definitely not the approved command".to_string(),
		..node("approved-execution", TaskNodeKind::Execution)
	};
	let approval_id = ApprovalId(compact_approval_id(&task_id.0, &execution_node.node_id.0));
	let graph = TaskGraph {
		task_id: task_id.clone(),
		nodes: vec![execution_node.clone()],
		edges: Vec::new(),
	};
	let mut task = graph_task(
		&task_id.0,
		&request_id.0,
		"Resume the approved command",
		TaskState::WaitingApproval,
		graph,
	);
	task.pending_approval_id = Some(approval_id.clone());
	let canonical_execution = sample_frozen_command_execution(&cwd_text, "digest-from-ticket");
	let frozen_payload = serde_json::json!({
		"error_code": "approval_required",
		"message": "approval required",
		"tool_name": "command.run",
		"tool_input": {
			"command": "pwd",
			"cwd": cwd_text
		},
		"policy_decision": sample_execution_policy_decision(),
		"canonical_execution": canonical_execution,
		"digest": "digest-from-ticket",
	})
	.to_string();
	let mutable_node_result = ResultEnvelope {
		task_id: task_id.clone(),
		node_id: execution_node.node_id.clone(),
		producer: "worker-1".to_string(),
		schema_version: "result.v1".to_string(),
		status: ResultStatus::Error,
		payload: serde_json::json!({
			"error_code": "approval_required",
			"message": "approval required",
			"tool_name": "command.run",
			"tool_input": {
				"command": "pwd",
				"cwd": cwd_text
			},
			"policy_decision": sample_execution_policy_decision(),
			"canonical_execution": sample_frozen_command_execution(&cwd_text, "mutable-node-digest"),
			"digest": "mutable-node-digest",
		})
		.to_string(),
		evidence: Vec::new(),
		confidence: 0.0,
	};
	let mutable_node_artifact = service
		.persist_result_artifact(&mutable_node_result)
		.expect("mutable node artifact should persist");
	let frozen_snapshot_ref = persist_frozen_execution_snapshot_artifact(
		&service,
		&task_id,
		&execution_node.node_id,
		&CanonicalDigest("digest-from-ticket".to_string()),
		&frozen_payload,
	);
	assert_ne!(frozen_snapshot_ref, mutable_node_artifact.uri);

	service
		.start_experiment_run(&task, &task.goal, "legacy_graph")
		.expect("experiment should start");
	service.save_task(task).expect("task should persist");
	service
		.save_approval_ticket(ApprovalTicket {
			approval_id: approval_id.clone(),
			task_id: task_id.clone(),
			request_id: request_id.clone(),
			node_id: execution_node.node_id.clone(),
			summary: execution_node.description.clone(),
			status: ApprovalStatus::Pending,
			decided_by: None,
			comment: None,
			pending_execution: Some(PendingExecutionApproval {
				approval_id: approval_id.clone(),
				digest: CanonicalDigest("digest-from-ticket".to_string()),
				canonical_execution: sample_frozen_command_execution(
					&cwd_text,
					"digest-from-ticket",
				),
				policy_decision: sample_execution_policy_decision(),
				execution_ref: Some(ApprovedExecutionRef {
					digest: CanonicalDigest("digest-from-ticket".to_string()),
					frozen_payload_ref: Some(frozen_snapshot_ref.clone()),
				}),
			}),
		})
		.expect("approval ticket should persist");

	let pending_response = service
		.resume_task(&task_id)
		.expect("waiting execution approval should render a pending approval response");
	assert_eq!(pending_response.status, ResponseStatus::PendingApproval);
	assert_eq!(
		pending_response.message,
		format!(
			"🛡️ Approval Request\n\nTool: command.run\nAction: Run command pwd from {cwd_text}\nRisk: medium\nReason: the command is outside the constrained built-in allowlist"
		)
	);

	let response = service
		.decide_approval(
			&approval_id,
			ApprovalDecision {
				actor: "reviewer".to_string(),
				approved: true,
				comment: Some("approved".to_string()),
			},
		)
		.expect("execution approval should resume the frozen command");
	assert_eq!(response.status, ResponseStatus::Succeeded);

	let task = service
		.get_task(&task_id)
		.expect("task lookup should succeed")
		.expect("task should exist");
	assert_eq!(task.state, TaskState::Succeeded);
	assert_eq!(task.completed_nodes, vec![execution_node.node_id.clone()]);

	let result = service
		.list_results(&task_id)
		.expect("result lookup should succeed")
		.into_iter()
		.find(|result| result.node_id == execution_node.node_id)
		.expect("execution result should be persisted");
	let payload = serde_json::from_str::<serde_json::Value>(&result.payload)
		.expect("result payload should decode");
	assert_eq!(
		payload["output"]["data"]["digest"],
		serde_json::Value::String("digest-from-ticket".to_string())
	);
	assert_eq!(
		payload["output"]["data"]["stdout"],
		serde_json::Value::String(format!("{cwd_text}\n"))
	);
	assert_eq!(
		payload["output"]["data"]["command"],
		serde_json::Value::String("pwd".to_string())
	);
}

#[test]
fn execution_approval_tickets_reject_missing_frozen_payload_ref() {
	let service = RuntimeService::default();
	let task_id = TaskId("task-execution-approval-missing-ref".to_string());
	let request_id = RequestId("req-execution-approval-missing-ref".to_string());
	let execution_node = node("approved-execution", TaskNodeKind::Execution);
	let approval_id = ApprovalId(compact_approval_id(&task_id.0, &execution_node.node_id.0));
	let graph = TaskGraph {
		task_id: task_id.clone(),
		nodes: vec![execution_node.clone()],
		edges: Vec::new(),
	};
	let mut task = graph_task(
		&task_id.0,
		&request_id.0,
		"Resume the approved command",
		TaskState::WaitingApproval,
		graph,
	);
	task.pending_approval_id = Some(approval_id.clone());

	service.save_task(task).expect("task should persist");
	service
		.save_approval_ticket(ApprovalTicket {
			approval_id: approval_id.clone(),
			task_id: task_id.clone(),
			request_id: request_id.clone(),
			node_id: execution_node.node_id.clone(),
			summary: execution_node.description.clone(),
			status: ApprovalStatus::Pending,
			decided_by: None,
			comment: None,
			pending_execution: Some(PendingExecutionApproval {
				approval_id: approval_id.clone(),
				digest: CanonicalDigest("digest-from-ticket".to_string()),
				canonical_execution: sample_frozen_command_execution(".", "digest-from-ticket"),
				policy_decision: sample_execution_policy_decision(),
				execution_ref: Some(ApprovedExecutionRef {
					digest: CanonicalDigest("digest-from-ticket".to_string()),
					frozen_payload_ref: None,
				}),
			}),
		})
		.expect("approval ticket should persist");

	let error = service
		.decide_approval(
			&approval_id,
			ApprovalDecision {
				actor: "reviewer".to_string(),
				approved: true,
				comment: Some("approved".to_string()),
			},
		)
		.expect_err("missing frozen payload ref should be rejected");
	assert!(error.message.contains("frozen payload reference"));
}

#[test]
fn execution_approval_tickets_reject_mismatched_frozen_digest() {
	let service = RuntimeService::default();
	let directory = tempdir().expect("tempdir should succeed");
	let cwd = directory
		.path()
		.canonicalize()
		.expect("tempdir should canonicalize");
	let cwd_text = cwd.display().to_string();
	let task_id = TaskId("task-execution-approval-digest-mismatch".to_string());
	let request_id = RequestId("req-execution-approval-digest-mismatch".to_string());
	let execution_node = node("approved-execution", TaskNodeKind::Execution);
	let approval_id = ApprovalId(compact_approval_id(&task_id.0, &execution_node.node_id.0));
	let graph = TaskGraph {
		task_id: task_id.clone(),
		nodes: vec![execution_node.clone()],
		edges: Vec::new(),
	};
	let mut task = graph_task(
		&task_id.0,
		&request_id.0,
		"Resume the approved command",
		TaskState::WaitingApproval,
		graph,
	);
	task.pending_approval_id = Some(approval_id.clone());
	let frozen_result = ResultEnvelope {
		task_id: task_id.clone(),
		node_id: execution_node.node_id.clone(),
		producer: "worker-1".to_string(),
		schema_version: "result.v1".to_string(),
		status: ResultStatus::Error,
		payload: serde_json::json!({
			"error_code": "approval_required",
			"message": "approval required",
			"tool_name": "command.run",
			"tool_input": {
				"command": "pwd",
				"cwd": cwd_text
			},
			"policy_decision": sample_execution_policy_decision(),
			"canonical_execution": sample_frozen_command_execution(&cwd_text, "digest-from-ticket"),
			"digest": "different-digest",
		})
		.to_string(),
		evidence: Vec::new(),
		confidence: 0.0,
	};
	let frozen_snapshot_ref = persist_frozen_execution_snapshot_artifact(
		&service,
		&task_id,
		&execution_node.node_id,
		&CanonicalDigest("digest-from-ticket".to_string()),
		&frozen_result.payload,
	);

	service.save_task(task).expect("task should persist");
	service
		.save_approval_ticket(ApprovalTicket {
			approval_id: approval_id.clone(),
			task_id: task_id.clone(),
			request_id: request_id.clone(),
			node_id: execution_node.node_id.clone(),
			summary: execution_node.description.clone(),
			status: ApprovalStatus::Pending,
			decided_by: None,
			comment: None,
			pending_execution: Some(PendingExecutionApproval {
				approval_id: approval_id.clone(),
				digest: CanonicalDigest("digest-from-ticket".to_string()),
				canonical_execution: sample_frozen_command_execution(
					&cwd_text,
					"digest-from-ticket",
				),
				policy_decision: sample_execution_policy_decision(),
				execution_ref: Some(ApprovedExecutionRef {
					digest: CanonicalDigest("digest-from-ticket".to_string()),
					frozen_payload_ref: Some(frozen_snapshot_ref),
				}),
			}),
		})
		.expect("approval ticket should persist");

	let error = service
		.decide_approval(
			&approval_id,
			ApprovalDecision {
				actor: "reviewer".to_string(),
				approved: true,
				comment: Some("approved".to_string()),
			},
		)
		.expect_err("mismatched frozen digest should be rejected");
	assert!(error.message.contains("digest"));
}
