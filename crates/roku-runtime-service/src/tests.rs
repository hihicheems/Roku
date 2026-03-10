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

use roku_agent_runtime::{GenericAgentRuntime, RuntimeWorker};
use roku_common_types::{
	AggregationMode, ApprovalDecision, ApprovalId, ApprovalStatus, CompensationAction,
	CompensationStatus, ConversationRole, ConversationTurn, ErrorClass, EvidenceItem, JoinPolicy,
	NodeId, PlanningModeHint, RecoveryEligibility, RequestEnvelope, RequestId, ResponseStatus,
	ResultEnvelope, ResultStatus, Task, TaskEdge, TaskEventKind, TaskGraph, TaskId, TaskNode,
	TaskNodeKind, TaskState,
};
use roku_plugin_skills::{
	DownloadedArchive, SkillArchiveFetcher, SkillRegistry, SkillRegistryError, SkillSource,
};
use roku_state_store::{
	DispatchClaim, DispatchEnvelope, DispatchLease, DispatchQueue, RetryClaim, StoreError,
	TaskRepository,
};
use roku_supervisor_agent::DefaultSupervisorAgent;
use std::collections::VecDeque;
use std::io::{Cursor, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{RunMode, RuntimeDataPlane, RuntimeService, compact_approval_id};

fn sample_request() -> RequestEnvelope {
	RequestEnvelope {
		request_id: RequestId("req-1".to_string()),
		session_id: "session-1".to_string(),
		goal: "analyze market".to_string(),
		planning_mode_hint: None,
		conversation_history: Vec::new(),
	}
}

#[derive(Clone)]
struct FileBackedPaths {
	state_db: PathBuf,
	artifacts: PathBuf,
	experiments: PathBuf,
}

fn unique_path(suffix: &str) -> PathBuf {
	let nanos = SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.expect("clock should be after epoch")
		.as_nanos();
	std::env::temp_dir().join(format!("roku-runtime-test-{suffix}-{nanos}"))
}

fn file_backed_paths(prefix: &str) -> FileBackedPaths {
	FileBackedPaths {
		state_db: unique_path(&format!("{prefix}-state")).join("control-plane.db"),
		artifacts: unique_path(&format!("{prefix}-artifacts")),
		experiments: unique_path(&format!("{prefix}-experiments")),
	}
}

fn file_backed_service_with_planner(
	paths: &FileBackedPaths,
	runtime: GenericAgentRuntime,
	planner: Box<dyn roku_task_planner::TaskPlanner + Send + Sync>,
) -> RuntimeService {
	let store_config = roku_state_store::SqliteStoreConfig::new(paths.state_db.clone());
	RuntimeService::new_with_runtime_data_plane_and_metrics(
		RuntimeDataPlane {
			task_repo: Box::new(
				roku_state_store::SqliteTaskRepository::connect(store_config.clone())
					.expect("sqlite task repo should open"),
			),
			event_repo: Box::new(
				roku_state_store::SqliteEventRepository::connect(store_config.clone())
					.expect("sqlite event repo should open"),
			),
			approval_repo: Box::new(
				roku_state_store::SqliteApprovalRepository::connect(store_config.clone())
					.expect("sqlite approval repo should open"),
			),
			result_repo: Box::new(
				roku_state_store::SqliteResultRepository::connect(store_config.clone())
					.expect("sqlite result repo should open"),
			),
			dispatch_queue: Box::new(
				roku_state_store::SqliteDispatchQueue::connect(store_config)
					.expect("sqlite dispatch queue should open"),
			),
			artifact_store: roku_artifact_store::ArtifactStore::file_backed(
				paths.artifacts.clone(),
			),
			experiment_registry: roku_experiment_registry::ExperimentRegistry::file_backed(
				paths.experiments.clone(),
			),
		},
		Arc::new(roku_observability::InMemoryAuditSink::default()),
		runtime,
		Arc::new(roku_observability::Metrics::default()),
		planner,
	)
}

#[test]
fn planning_input_scales_for_complex_goal() {
	let input = DefaultSupervisorAgent::default().planning_input_for_request(&RequestEnvelope {
		request_id: RequestId("req-complex".to_string()),
		session_id: "session-1".to_string(),
		goal: "build and integrate a workflow to research alternatives, compare options, and deploy a production-ready bot".to_string(),
		planning_mode_hint: None,
		conversation_history: Vec::new(),
	});

	assert!(input.complexity_score >= 6);
	assert!(input.budget_tokens >= 6_000);
}

#[test]
fn planning_input_marks_high_risk_requests() {
	let input = DefaultSupervisorAgent::default().planning_input_for_request(&RequestEnvelope {
		request_id: RequestId("req-risk".to_string()),
		session_id: "session-1".to_string(),
		goal: "delete the production secret and approve the mutation".to_string(),
		planning_mode_hint: None,
		conversation_history: Vec::new(),
	});

	assert!(matches!(
		input.risk_level,
		roku_planning_engine::RiskLevel::High
	));
	assert!(input.budget_tokens >= 8_000);
}

#[test]
fn compact_approval_id_stays_short_for_telegram_callbacks() {
	let approval_id = compact_approval_id("task-tg-919471825", "request_clarification-approval");

	assert!(approval_id.len() <= 19);
	assert!(format!("ap:a:{approval_id}").len() <= 64);
}

#[test]
fn service_succeeds_for_happy_path() {
	let service = RuntimeService::default();
	let response = service
		.execute(sample_request())
		.expect("runtime service should succeed");

	assert_eq!(response.status, ResponseStatus::Succeeded);
	assert_eq!(response.artifacts.len(), 3);
	let artifacts = service
		.list_artifacts(&TaskId("task-req-1".to_string()))
		.expect("artifacts should load");
	assert_eq!(artifacts.len(), 3);
	let experiment = service
		.get_experiment_run(&TaskId("task-req-1".to_string()))
		.expect("experiment load should succeed")
		.expect("experiment should exist");
	assert_eq!(experiment.summary.as_deref(), Some("task succeeded"));
	assert_eq!(experiment.artifact_ids.len(), 3);
}

#[derive(Clone)]
struct StaticArchiveFetcher {
	archive: DownloadedArchive,
}

impl SkillArchiveFetcher for StaticArchiveFetcher {
	fn fetch(&self, _source: &SkillSource) -> Result<DownloadedArchive, SkillRegistryError> {
		Ok(self.archive.clone())
	}
}

#[test]
fn service_surfaces_skill_install_success_message() {
	let paths = file_backed_paths("skill-install");
	let skill_root = unique_path("skill-root");
	let registry = SkillRegistry::file_backed(skill_root.clone()).with_fetcher(Arc::new(
		StaticArchiveFetcher {
			archive: DownloadedArchive {
				archive_url: "https://example.com/archive.zip".to_string(),
				bytes: test_skill_archive_bytes(),
				resolved_reference: Some("main".to_string()),
			},
		},
	));
	let runtime = GenericAgentRuntime::with_skill_registry(registry);
	let service = file_backed_service_with_planner(
		&paths,
		runtime,
		Box::new(roku_task_planner::AdaptiveTaskPlanner::default()),
	);
	let response = service
		.execute(RequestEnvelope {
			request_id: RequestId("req-skill".to_string()),
			session_id: "session-1".to_string(),
			goal: "Install skill from https://github.com/anthropics/skills/tree/main/skills/claude-api"
				.to_string(),
			planning_mode_hint: Some(PlanningModeHint::TaskDecomposition),
			conversation_history: Vec::new(),
		})
		.expect("runtime service should succeed");

	assert_eq!(response.status, ResponseStatus::Succeeded);
	assert!(response.message.contains("Installed skill `claude-api`"));
	assert!(
		skill_root
			.join("installed")
			.join("claude-api")
			.join("SKILL.md")
			.exists()
	);
}

#[test]
fn service_exposes_task_snapshot_and_event_timeline() {
	let service = RuntimeService::default();
	service
		.execute(sample_request())
		.expect("runtime service should succeed");

	let task_id = TaskId("task-req-1".to_string());
	let task = service
		.get_task(&task_id)
		.expect("task lookup should succeed")
		.expect("task should exist");
	let events = service
		.list_task_events(&task_id)
		.expect("event lookup should succeed");

	assert_eq!(task.state, TaskState::Succeeded);
	assert!(!events.is_empty());
	assert_eq!(
		events.first().expect("event should exist").from,
		TaskState::Queued
	);
	assert_eq!(
		events.first().expect("event should exist").to,
		TaskState::Planning
	);
	assert_eq!(
		events.last().expect("event should exist").to,
		TaskState::Succeeded
	);
}

fn test_skill_archive_bytes() -> Vec<u8> {
	let mut cursor = Cursor::new(Vec::new());
	{
		let mut writer = zip::ZipWriter::new(&mut cursor);
		let options = zip::write::SimpleFileOptions::default();
		writer
			.add_directory("skills-main/skills/claude-api/", options)
			.expect("dir should be added");
		writer
			.start_file("skills-main/skills/claude-api/SKILL.md", options)
			.expect("skill file should start");
		writer
			.write_all(
				br#"---
name: claude-api
description: Build apps with the Claude API.
---

# Claude API Skill

Use this skill when the user explicitly asks for Claude API integration help.
"#,
			)
			.expect("skill markdown should write");
		writer.finish().expect("zip should finish");
	}
	cursor.into_inner()
}

#[test]
fn service_builds_replay_report_from_persisted_events() {
	let service = RuntimeService::default();
	service
		.execute(sample_request())
		.expect("runtime service should succeed");

	let report = service
		.get_task_replay_report(&TaskId("task-req-1".to_string()))
		.expect("replay report lookup should succeed")
		.expect("replay report should exist");

	assert_eq!(report.persisted_state, TaskState::Succeeded);
	assert_eq!(report.replayed_state, TaskState::Succeeded);
	assert!(report.transitions_valid);
	assert!(report.chain_consistent);
	assert!(report.snapshot_matches_replay);
	assert!(!report.recoverable);
	assert!(!report.events.is_empty());
}

#[test]
fn service_persists_tool_runtime_evidence_for_execution_nodes() {
	let service = RuntimeService::default();
	service
		.execute(sample_request())
		.expect("runtime service should succeed");

	let results = service
		.list_results(&TaskId("task-req-1".to_string()))
		.expect("results should load");
	assert!(!results.is_empty());
	let execution_results = results
		.iter()
		.filter(|result| {
			!result.producer.starts_with("aggregation:")
				&& !result.producer.starts_with("validation:")
		})
		.collect::<Vec<_>>();
	assert!(!execution_results.is_empty());
	assert!(execution_results.iter().all(|result| {
		result
			.evidence
			.iter()
			.any(|item| item.kind == "tool" && !item.value.is_empty())
	}));
	assert!(execution_results.iter().all(|result| {
		result
			.evidence
			.iter()
			.any(|item| item.kind == "output_fingerprint" && !item.value.is_empty())
	}));
	assert!(results.iter().any(|result| {
		result.producer == "aggregation:aggregation-gate"
			&& result
				.evidence
				.iter()
				.any(|item| item.kind == "aggregation")
	}));
	assert!(results.iter().any(|result| {
		result.producer == "validation:validation-gate"
			&& result.evidence.iter().any(|item| item.kind == "validation")
	}));
	assert!(
		results
			.iter()
			.all(|result| { serde_json::from_str::<serde_json::Value>(&result.payload).is_ok() })
	);
}

#[test]
fn service_exposes_artifact_content_by_task_and_artifact() {
	let service = RuntimeService::default();
	let response = service
		.execute(sample_request())
		.expect("runtime service should succeed");
	assert_eq!(response.status, ResponseStatus::Succeeded);
	let task_id = TaskId("task-req-1".to_string());
	let artifacts = service
		.list_artifacts(&task_id)
		.expect("artifacts should load");
	let artifact = artifacts.first().expect("artifact should exist");
	let content = service
		.get_artifact_content(&task_id, &artifact.artifact_id)
		.expect("artifact content lookup should succeed")
		.expect("artifact content should exist");
	assert!(!content.is_empty());
}

#[test]
fn service_rejects_cross_task_artifact_content_access() {
	let service = RuntimeService::default();
	service
		.execute(sample_request())
		.expect("runtime service should succeed");
	let task_id = TaskId("task-req-1".to_string());
	let artifacts = service
		.list_artifacts(&task_id)
		.expect("artifacts should load");
	let artifact = artifacts.first().expect("artifact should exist");

	let error = service
		.get_artifact_content(&TaskId("task-other".to_string()), &artifact.artifact_id)
		.expect_err("cross-task artifact content should be rejected");
	assert!(error.message.contains("does not belong to task"));
}

#[test]
fn service_reports_validation_failure() {
	let service = RuntimeService::default();
	let response = service
		.execute_with_mode(sample_request(), RunMode::MissingEvidence)
		.expect("runtime service should return failed response");

	assert_eq!(response.status, ResponseStatus::Failed);
	assert!(response.message.contains("evidence is required"));
	let experiment = service
		.get_experiment_run(&TaskId("task-req-1".to_string()))
		.expect("experiment load should succeed")
		.expect("experiment should exist");
	assert!(
		experiment
			.failure_reason
			.as_deref()
			.is_some_and(|reason| reason.contains("evidence is required"))
	);
}

#[test]
fn service_reports_capability_denied() {
	let service = RuntimeService::default();
	let response = service
		.execute_with_mode(sample_request(), RunMode::CapabilityDenied)
		.expect("runtime service should return failed response");

	assert_eq!(response.status, ResponseStatus::Failed);
	assert!(response.message.contains("capability denied"));
	let metrics = service.metrics_snapshot();
	assert_eq!(metrics.experiments_failed_total, 1);
}

#[test]
fn service_surfaces_execution_failure_reason_before_validation() {
	struct FailingWorker;

	impl RuntimeWorker for FailingWorker {
		fn worker_id(&self) -> &'static str {
			"failing-worker"
		}

		fn supports(&self, _capabilities: &[String]) -> bool {
			true
		}

		fn execute(
			&self,
			spec: &roku_common_types::AgentInstanceSpec,
			node: &TaskNode,
		) -> ResultEnvelope {
			ResultEnvelope {
				task_id: spec.context.task_id.clone(),
				node_id: node.node_id.clone(),
				producer: spec.instance_id.clone(),
				schema_version: "result.v1".to_string(),
				status: ResultStatus::Error,
				payload: r#"{"message":"openrouter returned status 400: account default model is not configured"}"#.to_string(),
				evidence: vec![EvidenceItem {
					kind: "runtime".to_string(),
					value: "failing-worker".to_string(),
				}],
				confidence: 0.0,
			}
		}
	}

	let mut runtime = GenericAgentRuntime::default();
	runtime.register_worker(255, FailingWorker);
	let service = RuntimeService::in_memory_with_agent_runtime(runtime);
	let response = service
		.execute(sample_request())
		.expect("runtime service should return failure response");

	assert_eq!(response.status, ResponseStatus::Failed);
	assert!(response.message.contains("openrouter returned status 400"));
	assert!(!response.message.contains("confidence below minimum"));
}

#[test]
fn service_tracks_planning_metrics() {
	let service = RuntimeService::default();
	service
		.execute(sample_request())
		.expect("runtime service should succeed");

	let metrics = service.metrics_snapshot();
	assert_eq!(metrics.planning_runs_total, 1);
	assert_eq!(metrics.planning_react_total, 1);
}

#[test]
fn planning_input_override_selects_task_decomposition_mode() {
	let service = RuntimeService::default();
	let mut request = sample_request();
	request.planning_mode_hint = Some(PlanningModeHint::TaskDecomposition);

	let response = service
		.execute(request)
		.expect("runtime service should succeed");

	assert_eq!(response.status, ResponseStatus::Succeeded);
	let metrics = service.metrics_snapshot();
	assert_eq!(metrics.planning_task_decomposition_total, 1);
}

#[test]
fn planning_input_override_selects_tree_search_mode() {
	let service = RuntimeService::default();
	let mut request = sample_request();
	request.planning_mode_hint = Some(PlanningModeHint::TreeSearch);

	let response = service
		.execute(request)
		.expect("runtime service should succeed");

	assert_eq!(response.status, ResponseStatus::Succeeded);
	let metrics = service.metrics_snapshot();
	assert_eq!(metrics.planning_tree_search_total, 1);
}

#[test]
fn planning_input_override_selects_iterative_refinement_mode() {
	let service = RuntimeService::default();
	let mut request = sample_request();
	request.planning_mode_hint = Some(PlanningModeHint::IterativeRefinement);

	let response = service
		.execute_with_mode(request, RunMode::ApprovalRequired)
		.expect("runtime service should reach approval path");

	assert_eq!(response.status, ResponseStatus::PendingApproval);
	let metrics = service.metrics_snapshot();
	assert_eq!(metrics.planning_iterative_refinement_total, 1);
}

#[test]
fn service_returns_pending_approval_when_graph_contains_approval_gate() {
	let service = RuntimeService::default();
	let response = service
		.execute_with_mode(sample_request(), RunMode::ApprovalRequired)
		.expect("runtime service should stop at approval gate");

	assert_eq!(response.status, ResponseStatus::PendingApproval);
	assert!(response.message.contains("approval required"));
	assert_eq!(response.artifacts.len(), 1);
}

#[test]
fn service_dead_letters_when_retry_budget_is_exhausted() {
	let service = RuntimeService::default();
	let response = service
		.execute_with_mode(sample_request(), RunMode::RetryExhausted)
		.expect("runtime service should report dead-letter");

	assert_eq!(response.status, ResponseStatus::Failed);
	assert!(response.message.contains("dead-lettered"));
}

#[test]
fn service_resumes_after_approval_is_granted() {
	let service = RuntimeService::default();
	let response = service
		.execute_with_mode(sample_request(), RunMode::ApprovalRequired)
		.expect("runtime service should create approval ticket");
	let approval_id = ApprovalId(
		response.artifacts[0]
			.trim_start_matches("approval://")
			.to_string(),
	);

	let resumed = service
		.decide_approval(
			&approval_id,
			ApprovalDecision {
				actor: "reviewer".to_string(),
				approved: true,
				comment: Some("approved".to_string()),
			},
		)
		.expect("approval grant should resume task");

	assert_eq!(resumed.status, ResponseStatus::Succeeded);
}

#[test]
fn service_resume_task_returns_pending_approval_for_waiting_ticket() {
	let service = RuntimeService::default();
	let response = service
		.execute_with_mode(sample_request(), RunMode::ApprovalRequired)
		.expect("runtime service should create approval ticket");
	let approval_artifact = response.artifacts[0].clone();

	let resumed = service
		.resume_task(&TaskId("task-req-1".to_string()))
		.expect("resume should surface pending approval");

	assert_eq!(resumed.status, ResponseStatus::PendingApproval);
	assert_eq!(resumed.artifacts, vec![approval_artifact]);
}

#[test]
fn service_marks_waiting_approval_replay_report_as_recoverable() {
	let service = RuntimeService::default();
	service
		.execute_with_mode(sample_request(), RunMode::ApprovalRequired)
		.expect("runtime service should create approval ticket");

	let report = service
		.get_task_replay_report(&TaskId("task-req-1".to_string()))
		.expect("replay report lookup should succeed")
		.expect("replay report should exist");

	assert_eq!(report.persisted_state, TaskState::WaitingApproval);
	assert_eq!(report.replayed_state, TaskState::WaitingApproval);
	assert!(report.transitions_valid);
	assert!(report.chain_consistent);
	assert!(report.snapshot_matches_replay);
	assert!(report.recoverable);
	assert_eq!(
		report.recovery_eligibility,
		RecoveryEligibility::PendingApproval
	);
	assert_eq!(report.resume_candidates.len(), 1);
	assert_eq!(
		report.resume_candidates[0].eligibility,
		RecoveryEligibility::RequiresManualResume
	);
}

#[test]
fn service_resume_task_recovers_failed_execution() {
	let service = RuntimeService::default();
	let response = service
		.execute_with_mode(sample_request(), RunMode::CapabilityDenied)
		.expect("runtime service should fail before resume");
	assert_eq!(response.status, ResponseStatus::Failed);
	let report = service
		.get_task_replay_report(&TaskId("task-req-1".to_string()))
		.expect("replay report lookup should succeed")
		.expect("replay report should exist");
	assert_eq!(
		report.recovery_eligibility,
		RecoveryEligibility::ResumeReady
	);
	assert!(
		report
			.resume_candidates
			.iter()
			.any(|candidate| { candidate.eligibility == RecoveryEligibility::ResumeReady })
	);

	let resumed = service
		.resume_task(&TaskId("task-req-1".to_string()))
		.expect("resume should continue failed task");

	assert_eq!(resumed.status, ResponseStatus::Succeeded);
}

#[test]
fn service_cancel_task_records_compensation_and_cancels_pending_approval() {
	let service = RuntimeService::default();
	let response = service
		.execute_with_mode(sample_request(), RunMode::ApprovalRequired)
		.expect("runtime service should create approval ticket");
	let approval_id = ApprovalId(
		response.artifacts[0]
			.trim_start_matches("approval://")
			.to_string(),
	);

	let cancelled = service
		.cancel_task(&TaskId("task-req-1".to_string()), "operator")
		.expect("task cancellation should succeed");

	assert_eq!(cancelled.state, TaskState::Cancelled);
	assert!(cancelled.pending_approval_id.is_none());
	assert!(!cancelled.compensation_records.is_empty());
	assert!(cancelled.compensation_records.iter().all(|record| {
		record.status == CompensationStatus::Completed
			&& matches!(
				record.action,
				CompensationAction::AuditOnly | CompensationAction::Noop
			)
	}));

	let ticket = service
		.get_approval(&approval_id)
		.expect("approval lookup should succeed")
		.expect("approval ticket should exist");
	assert_eq!(ticket.status, ApprovalStatus::Cancelled);
	assert_eq!(ticket.decided_by.as_deref(), Some("operator"));

	let task = service
		.get_task(&TaskId("task-req-1".to_string()))
		.expect("task lookup should succeed")
		.expect("task should exist");
	assert_eq!(task.state, TaskState::Cancelled);
}

#[test]
fn service_timeout_recovery_resumes_timed_out_task() {
	let service = RuntimeService::default();
	let response = service
		.execute_with_mode(sample_request(), RunMode::TimeoutRecovery)
		.expect("runtime service should enter timeout recovery");
	assert_eq!(response.status, ResponseStatus::Failed);
	assert!(response.message.contains("timed out"));

	let task_id = TaskId("task-req-1".to_string());
	let report = service
		.get_task_replay_report(&task_id)
		.expect("replay report lookup should succeed")
		.expect("replay report should exist");
	assert_eq!(report.persisted_state, TaskState::TimeoutRecovering);
	assert_eq!(
		report.recovery_eligibility,
		RecoveryEligibility::ResumeReady
	);

	let resumed = service
		.recover_timed_out_task(&task_id)
		.expect("timeout recovery should resume the task");
	assert_eq!(resumed.status, ResponseStatus::Succeeded);

	let task = service
		.get_task(&task_id)
		.expect("task lookup should succeed")
		.expect("task should exist");
	assert_eq!(task.state, TaskState::Succeeded);
}

#[test]
fn service_reconstructs_execution_progress_from_persisted_results_after_restart() {
	#[derive(Clone)]
	struct CountingWorker {
		executions: Arc<AtomicUsize>,
	}

	struct FixedApprovalPlanner;

	impl roku_task_planner::TaskPlanner for FixedApprovalPlanner {
		fn build_outline(
			&self,
			request: &RequestEnvelope,
			_decision: &roku_planning_engine::PlanningDecision,
		) -> roku_common_types::PlanOutline {
			roku_common_types::PlanOutline {
				goal: request.goal.clone(),
				steps: vec![roku_common_types::PlanStep {
					step_id: "single-step".to_string(),
					summary: "single-step".to_string(),
					resource_selectors: Vec::new(),
					required_capabilities: Vec::new(),
					requires_approval: true,
					depends_on: Vec::new(),
					branch: None,
					loop_control: None,
				}],
			}
		}
	}

	impl RuntimeWorker for CountingWorker {
		fn worker_id(&self) -> &'static str {
			"counting-worker"
		}

		fn supports(&self, _capabilities: &[String]) -> bool {
			true
		}

		fn execute(
			&self,
			spec: &roku_common_types::AgentInstanceSpec,
			node: &TaskNode,
		) -> ResultEnvelope {
			self.executions.fetch_add(1, Ordering::SeqCst);
			ResultEnvelope {
				task_id: spec.context.task_id.clone(),
				node_id: node.node_id.clone(),
				producer: spec.instance_id.clone(),
				schema_version: "result.v1".to_string(),
				status: ResultStatus::Ok,
				payload: r#"{"message":"counted execution completed"}"#.to_string(),
				evidence: vec![EvidenceItem {
					kind: "runtime".to_string(),
					value: "counting-worker".to_string(),
				}],
				confidence: 0.95,
			}
		}
	}

	let paths = file_backed_paths("recovery-restart");
	let counter = Arc::new(AtomicUsize::new(0));
	let mut runtime = GenericAgentRuntime::default();
	runtime.register_worker(
		255,
		CountingWorker {
			executions: counter.clone(),
		},
	);
	let service = file_backed_service_with_planner(&paths, runtime, Box::new(FixedApprovalPlanner));
	let pending = service
		.execute_with_mode(sample_request(), RunMode::ApprovalRequired)
		.expect("runtime service should create approval ticket");
	assert_eq!(counter.load(Ordering::SeqCst), 1);

	let task_id = TaskId("task-req-1".to_string());
	let mut task_repo = roku_state_store::SqliteTaskRepository::connect(
		roku_state_store::SqliteStoreConfig::new(paths.state_db.clone()),
	)
	.expect("sqlite task repo should open");
	let mut persisted_task = task_repo
		.load_task(&task_id)
		.expect("task load should succeed")
		.expect("task should exist");
	persisted_task.completed_nodes.clear();
	persisted_task.next_node_index = 0;
	persisted_task.last_result = None;
	task_repo
		.save_task(persisted_task)
		.expect("task save should succeed");

	let approval_id = ApprovalId(
		pending.artifacts[0]
			.trim_start_matches("approval://")
			.to_string(),
	);
	let mut resumed_runtime = GenericAgentRuntime::default();
	resumed_runtime.register_worker(
		255,
		CountingWorker {
			executions: counter.clone(),
		},
	);
	let resumed_service =
		file_backed_service_with_planner(&paths, resumed_runtime, Box::new(FixedApprovalPlanner));
	let response = resumed_service
		.decide_approval(
			&approval_id,
			ApprovalDecision {
				actor: "reviewer".to_string(),
				approved: true,
				comment: Some("resume with persisted results".to_string()),
			},
		)
		.expect("approval decision should complete the task");

	assert_eq!(response.status, ResponseStatus::Succeeded);
	assert_eq!(counter.load(Ordering::SeqCst), 1);
}

#[test]
fn service_recovers_after_replay_log_is_compacted_into_snapshot() {
	let paths = file_backed_paths("recovery-compaction");
	let service = file_backed_service_with_planner(
		&paths,
		GenericAgentRuntime::default(),
		Box::new(roku_task_planner::AdaptiveTaskPlanner::default()),
	);
	let pending = service
		.execute_with_mode(sample_request(), RunMode::ApprovalRequired)
		.expect("runtime service should create approval ticket");
	assert_eq!(pending.status, ResponseStatus::PendingApproval);

	let task_id = TaskId("task-req-1".to_string());
	service
		.compact_task_replay(&task_id, 1)
		.expect("replay log compaction should succeed");

	let mut task_repo = roku_state_store::SqliteTaskRepository::connect(
		roku_state_store::SqliteStoreConfig::new(paths.state_db.clone()),
	)
	.expect("sqlite task repo should open");
	let mut persisted_task = task_repo
		.load_task(&task_id)
		.expect("task load should succeed")
		.expect("task should exist");
	persisted_task.completed_nodes.clear();
	persisted_task.next_node_index = 0;
	persisted_task.last_result = None;
	task_repo
		.save_task(persisted_task)
		.expect("task save should succeed");

	let restarted_service = file_backed_service_with_planner(
		&paths,
		GenericAgentRuntime::default(),
		Box::new(roku_task_planner::AdaptiveTaskPlanner::default()),
	);
	let replay = restarted_service
		.get_task_replay_report(&task_id)
		.expect("replay report lookup should succeed")
		.expect("replay report should exist");
	assert!(replay.event_count > replay.events.len());
	assert!(replay.snapshot_matches_replay);

	let approval_id = ApprovalId(
		pending.artifacts[0]
			.trim_start_matches("approval://")
			.to_string(),
	);
	let resumed = restarted_service
		.decide_approval(
			&approval_id,
			ApprovalDecision {
				actor: "reviewer".to_string(),
				approved: true,
				comment: Some("resume after compaction".to_string()),
			},
		)
		.expect("approval should resume task after compaction");
	assert_eq!(resumed.status, ResponseStatus::Succeeded);
}

#[test]
fn service_reconstructs_approval_and_validation_progress_from_persistence() {
	let service = RuntimeService::default();
	let task = Task {
		task_id: TaskId("task-reconstruct-validation".to_string()),
		request_id: RequestId("req-reconstruct-validation".to_string()),
		session_id: "session-reconstruct-validation".to_string(),
		goal: "recover validation progress".to_string(),
		state: TaskState::Failed,
		attempts: 0,
		planning_mode_hint: None,
		conversation_history: Vec::new(),
		completed_nodes: Vec::new(),
		next_node_index: 0,
		pending_approval_id: None,
		last_result: None,
		compensation_records: Vec::new(),
		graph: Some(TaskGraph {
			task_id: TaskId("task-reconstruct-validation".to_string()),
			nodes: vec![
				TaskNode {
					node_id: NodeId("step-1".to_string()),
					kind: TaskNodeKind::Execution,
					description: "step 1".to_string(),
					capabilities: vec!["data.read".to_string()],
					join_policy: JoinPolicy::AllParents,
					aggregation_mode: AggregationMode::CollectAll,
					..TaskNode::default()
				},
				TaskNode {
					node_id: NodeId("step-1-approval".to_string()),
					kind: TaskNodeKind::Approval,
					description: "approval".to_string(),
					capabilities: vec!["approve.action".to_string()],
					join_policy: JoinPolicy::AllParents,
					aggregation_mode: AggregationMode::CollectAll,
					..TaskNode::default()
				},
				TaskNode {
					node_id: NodeId("validation-gate".to_string()),
					kind: TaskNodeKind::Validation,
					description: "validation".to_string(),
					capabilities: vec!["validate.result".to_string()],
					join_policy: JoinPolicy::AllParents,
					aggregation_mode: AggregationMode::CollectAll,
					..TaskNode::default()
				},
				TaskNode {
					node_id: NodeId("aggregation-gate".to_string()),
					kind: TaskNodeKind::Aggregation,
					description: "aggregation".to_string(),
					capabilities: vec!["aggregate.result".to_string()],
					join_policy: JoinPolicy::AllParents,
					aggregation_mode: AggregationMode::CollectAll,
					..TaskNode::default()
				},
			],
			edges: vec![
				TaskEdge {
					from: NodeId("step-1".to_string()),
					to: NodeId("step-1-approval".to_string()),
					condition: roku_common_types::TaskEdgeCondition::OnSuccess,
				},
				TaskEdge {
					from: NodeId("step-1-approval".to_string()),
					to: NodeId("validation-gate".to_string()),
					condition: roku_common_types::TaskEdgeCondition::OnApproved,
				},
				TaskEdge {
					from: NodeId("validation-gate".to_string()),
					to: NodeId("aggregation-gate".to_string()),
					condition: roku_common_types::TaskEdgeCondition::OnSuccess,
				},
			],
		}),
	};
	service
		.save_result(ResultEnvelope {
			task_id: task.task_id.clone(),
			node_id: NodeId("step-1".to_string()),
			producer: "agent-step-1".to_string(),
			schema_version: "result.v1".to_string(),
			status: ResultStatus::Ok,
			payload: r#"{"message":"step 1 complete"}"#.to_string(),
			evidence: vec![EvidenceItem {
				kind: "tool".to_string(),
				value: "data.execute".to_string(),
			}],
			confidence: 0.9,
		})
		.expect("execution result should persist");
	service
		.save_approval_ticket(roku_common_types::ApprovalTicket {
			approval_id: ApprovalId("approval-reconstruct".to_string()),
			task_id: task.task_id.clone(),
			request_id: task.request_id.clone(),
			node_id: NodeId("step-1-approval".to_string()),
			summary: "approval".to_string(),
			status: ApprovalStatus::Approved,
			decided_by: Some("reviewer".to_string()),
			comment: Some("approved".to_string()),
		})
		.expect("approval ticket should persist");
	service
		.save_result(ResultEnvelope {
			task_id: task.task_id.clone(),
			node_id: NodeId("validation-gate".to_string()),
			producer: "validation:validation-gate".to_string(),
			schema_version: "result.v1".to_string(),
			status: ResultStatus::Ok,
			payload: r#"{"message":"validated 1 result(s)"}"#.to_string(),
			evidence: vec![EvidenceItem {
				kind: "validation".to_string(),
				value: "accepted=1".to_string(),
			}],
			confidence: 1.0,
		})
		.expect("validation result should persist");

	let reconstructed = service
		.reconstruct_task_progress(&task)
		.expect("task progress should reconstruct");
	assert_eq!(
		reconstructed.completed_nodes,
		vec![
			NodeId("step-1".to_string()),
			NodeId("step-1-approval".to_string()),
			NodeId("validation-gate".to_string()),
		]
	);
	let analysis = service
		.analyze_task_recovery(&task)
		.expect("recovery analysis should succeed");
	assert_eq!(analysis.ready_nodes.len(), 1);
	assert_eq!(analysis.ready_nodes[0].node_id.0, "aggregation-gate");
}

#[test]
fn service_reconstructs_progress_from_node_events_without_snapshot_or_results() {
	let service = RuntimeService::default();
	let task = Task {
		task_id: TaskId("task-event-recovery".to_string()),
		request_id: RequestId("req-event-recovery".to_string()),
		session_id: "session-event-recovery".to_string(),
		goal: "recover from node events".to_string(),
		state: TaskState::Failed,
		attempts: 0,
		planning_mode_hint: None,
		conversation_history: Vec::new(),
		completed_nodes: Vec::new(),
		next_node_index: 0,
		pending_approval_id: None,
		last_result: None,
		compensation_records: Vec::new(),
		graph: Some(TaskGraph {
			task_id: TaskId("task-event-recovery".to_string()),
			nodes: vec![
				TaskNode {
					node_id: NodeId("step-1".to_string()),
					kind: TaskNodeKind::Execution,
					description: "step 1".to_string(),
					capabilities: vec!["data.read".to_string()],
					join_policy: JoinPolicy::AllParents,
					aggregation_mode: AggregationMode::CollectAll,
					..TaskNode::default()
				},
				TaskNode {
					node_id: NodeId("validation-gate".to_string()),
					kind: TaskNodeKind::Validation,
					description: "validation".to_string(),
					capabilities: vec!["validate.result".to_string()],
					join_policy: JoinPolicy::AllParents,
					aggregation_mode: AggregationMode::CollectAll,
					..TaskNode::default()
				},
			],
			edges: vec![TaskEdge {
				from: NodeId("step-1".to_string()),
				to: NodeId("validation-gate".to_string()),
				condition: roku_common_types::TaskEdgeCondition::OnSuccess,
			}],
		}),
	};
	let execution_node = task
		.graph
		.as_ref()
		.expect("graph should exist")
		.nodes
		.first()
		.expect("execution node should exist")
		.clone();
	service
		.append_node_event(
			&task,
			&execution_node,
			TaskEventKind::NodeCompleted,
			"execution completed before restart",
		)
		.expect("node event should persist");

	let reconstructed = service
		.reconstruct_task_progress(&task)
		.expect("task progress should reconstruct from events");
	assert_eq!(
		reconstructed.completed_nodes,
		vec![NodeId("step-1".to_string())]
	);

	let analysis = service
		.analyze_task_recovery(&task)
		.expect("recovery analysis should succeed");
	assert_eq!(analysis.ready_nodes.len(), 1);
	assert_eq!(
		analysis.ready_nodes[0].node_id,
		NodeId("validation-gate".to_string())
	);
	assert_eq!(
		analysis.recovery_eligibility,
		RecoveryEligibility::ResumeReady
	);
}

#[test]
fn service_finalize_uses_supervisor_selected_final_result() {
	let service = RuntimeService::default();
	let task_id = TaskId("task-final-selection".to_string());
	let task = Task {
		task_id: task_id.clone(),
		request_id: RequestId("req-final-selection".to_string()),
		session_id: "session-final-selection".to_string(),
		goal: "select final result".to_string(),
		state: TaskState::Aggregating,
		attempts: 0,
		planning_mode_hint: None,
		conversation_history: Vec::new(),
		completed_nodes: vec![
			NodeId("step-1".to_string()),
			NodeId("aggregation-gate".to_string()),
		],
		next_node_index: 2,
		pending_approval_id: None,
		last_result: Some(ResultEnvelope {
			task_id: task_id.clone(),
			node_id: NodeId("step-1".to_string()),
			producer: "agent-step-1".to_string(),
			schema_version: "result.v1".to_string(),
			status: ResultStatus::Ok,
			payload: serde_json::json!({"message": "stale execution summary"}).to_string(),
			evidence: Vec::new(),
			confidence: 0.9,
		}),
		compensation_records: Vec::new(),
		graph: Some(TaskGraph {
			task_id: task_id.clone(),
			nodes: vec![
				TaskNode {
					node_id: NodeId("aggregation-gate".to_string()),
					kind: TaskNodeKind::Aggregation,
					description: "aggregate".to_string(),
					capabilities: Vec::new(),
					join_policy: JoinPolicy::AllParents,
					aggregation_mode: AggregationMode::CollectAll,
					..TaskNode::default()
				},
				TaskNode {
					node_id: NodeId("step-1".to_string()),
					kind: TaskNodeKind::Execution,
					description: "step 1".to_string(),
					capabilities: Vec::new(),
					join_policy: JoinPolicy::AllParents,
					aggregation_mode: AggregationMode::CollectAll,
					..TaskNode::default()
				},
			],
			edges: vec![TaskEdge {
				from: NodeId("step-1".to_string()),
				to: NodeId("aggregation-gate".to_string()),
				condition: roku_common_types::TaskEdgeCondition::OnSuccess,
			}],
		}),
	};
	service.save_task(task).expect("task should persist");
	service
		.save_result(ResultEnvelope {
			task_id: task_id.clone(),
			node_id: NodeId("step-1".to_string()),
			producer: "agent-step-1".to_string(),
			schema_version: "result.v1".to_string(),
			status: ResultStatus::Ok,
			payload: serde_json::json!({"message": "execution summary"}).to_string(),
			evidence: Vec::new(),
			confidence: 0.95,
		})
		.expect("execution result should persist");
	service
		.save_result(ResultEnvelope {
			task_id: task_id.clone(),
			node_id: NodeId("aggregation-gate".to_string()),
			producer: "aggregation:aggregation-gate".to_string(),
			schema_version: "result.v1".to_string(),
			status: ResultStatus::Ok,
			payload: serde_json::json!({"message": "aggregated final summary"}).to_string(),
			evidence: Vec::new(),
			confidence: 0.4,
		})
		.expect("aggregation result should persist");
	service
		.start_experiment_run(
			&service
				.get_task(&task_id)
				.expect("task lookup should succeed")
				.expect("task should exist"),
			"select final result",
			"test",
		)
		.expect("experiment run should start");

	let response = service
		.resume_task(&task_id)
		.expect("aggregating task should finalize successfully");

	assert_eq!(response.status, ResponseStatus::Succeeded);
	assert_eq!(response.message, "aggregated final summary");
	let persisted = service
		.get_task(&task_id)
		.expect("task lookup should succeed")
		.expect("task should exist");
	assert_eq!(persisted.state, TaskState::Succeeded);
	assert_eq!(
		persisted
			.last_result
			.as_ref()
			.expect("final result should persist")
			.node_id,
		NodeId("aggregation-gate".to_string())
	);
}

#[test]
fn service_enters_timeout_recovery_when_node_deadline_is_exceeded() {
	struct SlowWorker;

	impl RuntimeWorker for SlowWorker {
		fn worker_id(&self) -> &'static str {
			"slow-worker"
		}

		fn supports(&self, _capabilities: &[String]) -> bool {
			true
		}

		fn execute(
			&self,
			spec: &roku_common_types::AgentInstanceSpec,
			node: &TaskNode,
		) -> ResultEnvelope {
			ResultEnvelope {
				task_id: spec.context.task_id.clone(),
				node_id: node.node_id.clone(),
				producer: spec.instance_id.clone(),
				schema_version: "result.v1".to_string(),
				status: ResultStatus::Ok,
				payload: r#"{"message":"slow execution completed","elapsed_ms":50}"#.to_string(),
				evidence: vec![EvidenceItem {
					kind: "runtime".to_string(),
					value: "slow-worker".to_string(),
				}],
				confidence: 0.9,
			}
		}
	}

	let mut runtime = GenericAgentRuntime::default();
	runtime.register_worker(255, SlowWorker);
	let service = RuntimeService::in_memory_with_agent_runtime(runtime);
	let mut task = Task {
		task_id: TaskId("task-time-budget".to_string()),
		request_id: RequestId("req-time-budget".to_string()),
		session_id: "session-time-budget".to_string(),
		goal: "enforce deadline".to_string(),
		state: TaskState::Delegating,
		attempts: 0,
		planning_mode_hint: None,
		conversation_history: Vec::new(),
		completed_nodes: Vec::new(),
		next_node_index: 0,
		pending_approval_id: None,
		last_result: None,
		compensation_records: Vec::new(),
		graph: None,
	};
	let mut node = TaskNode {
		node_id: NodeId("slow-step".to_string()),
		kind: TaskNodeKind::Execution,
		description: "slow step".to_string(),
		capabilities: vec!["data.read".to_string()],
		join_policy: JoinPolicy::AllParents,
		aggregation_mode: AggregationMode::CollectAll,
		..TaskNode::default()
	};
	node.budget_snapshot.time_budget_ms = 1;
	node.deadline_ms = 1;
	node.retry_policy.retry_on_timeout = true;
	service
		.start_experiment_run(&task, &task.goal, "test")
		.expect("experiment run should start");

	let response = service
		.process_execution_node(&mut task, &node, RunMode::Normal)
		.expect("execution should return a response")
		.expect("timeout recovery response should be returned");

	assert_eq!(response.status, ResponseStatus::Failed);
	assert!(response.message.contains("timed out"));
	assert_eq!(task.state, TaskState::TimeoutRecovering);
	assert!(task.completed_nodes.is_empty());
	let results = service
		.list_results(&task.task_id)
		.expect("results should load");
	assert_eq!(results.len(), 1);
	assert_eq!(results[0].status, ResultStatus::Error);
	assert!(results[0].payload.contains("node_deadline_exceeded"));
	let events = service
		.list_task_events(&task.task_id)
		.expect("events should load");
	assert_eq!(
		events.last().and_then(|event| event.error_class),
		Some(ErrorClass::Timeout)
	);
}

#[test]
fn service_fails_when_approval_is_rejected() {
	let service = RuntimeService::default();
	let response = service
		.execute_with_mode(sample_request(), RunMode::ApprovalRequired)
		.expect("runtime service should create approval ticket");
	let approval_id = ApprovalId(
		response.artifacts[0]
			.trim_start_matches("approval://")
			.to_string(),
	);

	let resumed = service
		.decide_approval(
			&approval_id,
			ApprovalDecision {
				actor: "reviewer".to_string(),
				approved: false,
				comment: Some("rejected".to_string()),
			},
		)
		.expect("approval rejection should finish task");

	assert_eq!(resumed.status, ResponseStatus::Failed);
	assert!(resumed.message.contains("approval rejected"));
}

#[test]
fn approval_ticket_cannot_be_decided_twice() {
	let service = RuntimeService::default();
	let response = service
		.execute_with_mode(sample_request(), RunMode::ApprovalRequired)
		.expect("runtime service should create approval ticket");
	let approval_id = ApprovalId(
		response.artifacts[0]
			.trim_start_matches("approval://")
			.to_string(),
	);

	service
		.decide_approval(
			&approval_id,
			ApprovalDecision {
				actor: "reviewer".to_string(),
				approved: true,
				comment: None,
			},
		)
		.expect("first decision should succeed");

	let duplicate = service.decide_approval(
		&approval_id,
		ApprovalDecision {
			actor: "reviewer".to_string(),
			approved: true,
			comment: None,
		},
	);
	assert!(duplicate.is_err());
}

#[test]
fn validation_collects_results_through_approval_nodes() {
	let service = RuntimeService::default();
	let task = Task {
		task_id: TaskId("task-1".to_string()),
		request_id: RequestId("req-1".to_string()),
		session_id: "session-1".to_string(),
		goal: "validate approval edge".to_string(),
		state: TaskState::Executing,
		attempts: 0,
		planning_mode_hint: None,
		conversation_history: Vec::new(),
		completed_nodes: vec![NodeId("extract".to_string())],
		next_node_index: 1,
		pending_approval_id: None,
		last_result: None,
		compensation_records: Vec::new(),
		graph: Some(TaskGraph {
			task_id: TaskId("task-1".to_string()),
			nodes: vec![
				TaskNode {
					node_id: NodeId("extract".to_string()),
					kind: TaskNodeKind::Execution,
					description: "extract".to_string(),
					capabilities: Vec::new(),
					join_policy: JoinPolicy::AllParents,
					aggregation_mode: AggregationMode::CollectAll,
					..TaskNode::default()
				},
				TaskNode {
					node_id: NodeId("extract-approval".to_string()),
					kind: TaskNodeKind::Approval,
					description: "approval".to_string(),
					capabilities: Vec::new(),
					join_policy: JoinPolicy::AllParents,
					aggregation_mode: AggregationMode::CollectAll,
					..TaskNode::default()
				},
				TaskNode {
					node_id: NodeId("validate".to_string()),
					kind: TaskNodeKind::Validation,
					description: "validate".to_string(),
					capabilities: Vec::new(),
					join_policy: JoinPolicy::AllParents,
					aggregation_mode: AggregationMode::CollectAll,
					..TaskNode::default()
				},
			],
			edges: vec![
				TaskEdge {
					from: NodeId("extract".to_string()),
					to: NodeId("extract-approval".to_string()),
					condition: roku_common_types::TaskEdgeCondition::OnSuccess,
				},
				TaskEdge {
					from: NodeId("extract-approval".to_string()),
					to: NodeId("validate".to_string()),
					condition: roku_common_types::TaskEdgeCondition::OnApproved,
				},
			],
		}),
	};
	service
		.save_result(ResultEnvelope {
			task_id: TaskId("task-1".to_string()),
			node_id: NodeId("extract".to_string()),
			producer: "agent".to_string(),
			schema_version: "result.v1".to_string(),
			status: ResultStatus::Ok,
			payload: "payload".to_string(),
			evidence: vec![EvidenceItem {
				kind: "artifact_ref".to_string(),
				value: "artifact://1".to_string(),
			}],
			confidence: 0.9,
		})
		.expect("result should be persisted");

	let results = service
		.collect_upstream_results(&task, &NodeId("validate".to_string()))
		.expect("upstream results should resolve through approval nodes");

	assert_eq!(results.len(), 1);
	assert_eq!(results[0].node_id.0, "extract");
}

#[test]
fn validation_evidence_resolves_persisted_artifacts() {
	let service = RuntimeService::default();
	service
		.execute(sample_request())
		.expect("runtime service should succeed");

	let task_id = TaskId("task-req-1".to_string());
	let task = {
		let state = service.lock_state().expect("state lock should succeed");
		state
			.task_repo
			.load_task(&task_id)
			.expect("task load should succeed")
			.expect("task should exist")
	};
	let evidence_sets = service
		.collect_validation_evidence(
			&task,
			&task.graph.as_ref().expect("graph should exist").nodes[2],
		)
		.expect("validation evidence should load");

	assert_eq!(evidence_sets.len(), 1);
	assert!(
		evidence_sets
			.iter()
			.all(|evidence_set| !evidence_set.artifacts.is_empty())
	);
}

#[test]
fn node_result_set_applies_highest_confidence_aggregation() {
	let service = RuntimeService::default();
	let task = Task {
		task_id: TaskId("task-aggregation".to_string()),
		request_id: RequestId("req-aggregation".to_string()),
		session_id: "session-aggregation".to_string(),
		goal: "aggregate branches".to_string(),
		state: TaskState::Executing,
		attempts: 0,
		planning_mode_hint: None,
		conversation_history: Vec::new(),
		completed_nodes: vec![
			NodeId("branch-a".to_string()),
			NodeId("branch-b".to_string()),
		],
		next_node_index: 2,
		pending_approval_id: None,
		last_result: None,
		compensation_records: Vec::new(),
		graph: Some(TaskGraph {
			task_id: TaskId("task-aggregation".to_string()),
			nodes: vec![
				TaskNode {
					node_id: NodeId("branch-a".to_string()),
					kind: TaskNodeKind::Execution,
					description: "branch a".to_string(),
					capabilities: Vec::new(),
					join_policy: JoinPolicy::AllParents,
					aggregation_mode: AggregationMode::CollectAll,
					..TaskNode::default()
				},
				TaskNode {
					node_id: NodeId("branch-b".to_string()),
					kind: TaskNodeKind::Execution,
					description: "branch b".to_string(),
					capabilities: Vec::new(),
					join_policy: JoinPolicy::AllParents,
					aggregation_mode: AggregationMode::CollectAll,
					..TaskNode::default()
				},
				TaskNode {
					node_id: NodeId("aggregate".to_string()),
					kind: TaskNodeKind::Aggregation,
					description: "aggregate".to_string(),
					capabilities: Vec::new(),
					join_policy: JoinPolicy::AllParents,
					aggregation_mode: AggregationMode::HighestConfidence,
					..TaskNode::default()
				},
			],
			edges: vec![
				TaskEdge {
					from: NodeId("branch-a".to_string()),
					to: NodeId("aggregate".to_string()),
					condition: roku_common_types::TaskEdgeCondition::OnSuccess,
				},
				TaskEdge {
					from: NodeId("branch-b".to_string()),
					to: NodeId("aggregate".to_string()),
					condition: roku_common_types::TaskEdgeCondition::OnSuccess,
				},
			],
		}),
	};
	service
		.save_result(ResultEnvelope {
			task_id: TaskId("task-aggregation".to_string()),
			node_id: NodeId("branch-a".to_string()),
			producer: "agent-a".to_string(),
			schema_version: "result.v1".to_string(),
			status: ResultStatus::Ok,
			payload: "low".to_string(),
			evidence: Vec::new(),
			confidence: 0.4,
		})
		.expect("branch a result should persist");
	service
		.save_result(ResultEnvelope {
			task_id: TaskId("task-aggregation".to_string()),
			node_id: NodeId("branch-b".to_string()),
			producer: "agent-b".to_string(),
			schema_version: "result.v1".to_string(),
			status: ResultStatus::Ok,
			payload: "high".to_string(),
			evidence: Vec::new(),
			confidence: 0.9,
		})
		.expect("branch b result should persist");

	let result_set = service
		.collect_node_result_set(
			&task,
			&task.graph.as_ref().expect("graph should exist").nodes[2],
		)
		.expect("result set should collect");

	assert_eq!(result_set.source_node_ids.len(), 2);
	assert_eq!(result_set.results.len(), 1);
	assert_eq!(result_set.results[0].node_id.0, "branch-b");
}

#[test]
fn node_result_set_enforces_quorum_policy() {
	let service = RuntimeService::default();
	let task = Task {
		task_id: TaskId("task-quorum".to_string()),
		request_id: RequestId("req-quorum".to_string()),
		session_id: "session-quorum".to_string(),
		goal: "quorum aggregation".to_string(),
		state: TaskState::Executing,
		attempts: 0,
		planning_mode_hint: None,
		conversation_history: Vec::new(),
		completed_nodes: vec![NodeId("branch-a".to_string())],
		next_node_index: 1,
		pending_approval_id: None,
		last_result: None,
		compensation_records: Vec::new(),
		graph: Some(TaskGraph {
			task_id: TaskId("task-quorum".to_string()),
			nodes: vec![
				TaskNode {
					node_id: NodeId("branch-a".to_string()),
					kind: TaskNodeKind::Execution,
					description: "branch a".to_string(),
					capabilities: Vec::new(),
					join_policy: JoinPolicy::AllParents,
					aggregation_mode: AggregationMode::CollectAll,
					..TaskNode::default()
				},
				TaskNode {
					node_id: NodeId("branch-b".to_string()),
					kind: TaskNodeKind::Execution,
					description: "branch b".to_string(),
					capabilities: Vec::new(),
					join_policy: JoinPolicy::AllParents,
					aggregation_mode: AggregationMode::CollectAll,
					..TaskNode::default()
				},
				TaskNode {
					node_id: NodeId("aggregate".to_string()),
					kind: TaskNodeKind::Aggregation,
					description: "aggregate".to_string(),
					capabilities: Vec::new(),
					join_policy: JoinPolicy::Quorum(2),
					aggregation_mode: AggregationMode::CollectAll,
					..TaskNode::default()
				},
			],
			edges: vec![
				TaskEdge {
					from: NodeId("branch-a".to_string()),
					to: NodeId("aggregate".to_string()),
					condition: roku_common_types::TaskEdgeCondition::OnSuccess,
				},
				TaskEdge {
					from: NodeId("branch-b".to_string()),
					to: NodeId("aggregate".to_string()),
					condition: roku_common_types::TaskEdgeCondition::OnSuccess,
				},
			],
		}),
	};
	service
		.save_result(ResultEnvelope {
			task_id: TaskId("task-quorum".to_string()),
			node_id: NodeId("branch-a".to_string()),
			producer: "agent-a".to_string(),
			schema_version: "result.v1".to_string(),
			status: ResultStatus::Ok,
			payload: "only branch".to_string(),
			evidence: Vec::new(),
			confidence: 0.8,
		})
		.expect("branch a result should persist");

	let error = service
		.collect_node_result_set(
			&task,
			&task.graph.as_ref().expect("graph should exist").nodes[2],
		)
		.expect_err("quorum should reject incomplete branches");
	assert!(error.message.contains("join policy"));
}

#[derive(Default)]
struct RecordingDispatchState {
	published: Vec<String>,
	claimed: Vec<String>,
	acked: Vec<String>,
	nacked: Vec<String>,
	queued: VecDeque<DispatchEnvelope>,
	lease_sequence: u64,
}

#[derive(Clone, Default)]
struct RecordingDispatchQueue {
	state: Arc<Mutex<RecordingDispatchState>>,
}

impl RecordingDispatchQueue {
	fn snapshot(&self) -> (Vec<String>, Vec<String>, Vec<String>, Vec<String>) {
		let state = self
			.state
			.lock()
			.expect("dispatch state lock should succeed");
		(
			state.published.clone(),
			state.claimed.clone(),
			state.acked.clone(),
			state.nacked.clone(),
		)
	}

	fn seed(&self, envelope: DispatchEnvelope) {
		let mut state = self
			.state
			.lock()
			.expect("dispatch state lock should succeed");
		state.queued.push_back(envelope);
	}
}

impl DispatchQueue for RecordingDispatchQueue {
	fn publish(&mut self, envelope: DispatchEnvelope) -> Result<(), StoreError> {
		let mut state = self
			.state
			.lock()
			.expect("dispatch state lock should succeed");
		state.published.push(envelope.node_id.0.clone());
		state.queued.push_back(envelope);
		Ok(())
	}

	fn claim(
		&mut self,
		consumer_id: &str,
		now_unix_ms: u64,
	) -> Result<Option<DispatchClaim>, StoreError> {
		let mut state = self
			.state
			.lock()
			.expect("dispatch state lock should succeed");
		let Some(envelope) = state.queued.pop_front() else {
			return Ok(None);
		};
		state.claimed.push(envelope.node_id.0.clone());
		state.lease_sequence = state.lease_sequence.saturating_add(1);
		Ok(Some(DispatchClaim {
			envelope: envelope.clone(),
			lease: DispatchLease {
				entry_id: envelope.entry_id,
				consumer_id: consumer_id.to_string(),
				lease_token: format!("lease-{}", state.lease_sequence),
				expires_at_unix_ms: now_unix_ms.saturating_add(1),
			},
		}))
	}

	fn ack(&mut self, lease: &DispatchLease) -> Result<(), StoreError> {
		let mut state = self
			.state
			.lock()
			.expect("dispatch state lock should succeed");
		state.acked.push(lease.entry_id.clone());
		Ok(())
	}

	fn nack(&mut self, lease: &DispatchLease, _retry: RetryClaim) -> Result<(), StoreError> {
		let mut state = self
			.state
			.lock()
			.expect("dispatch state lock should succeed");
		state.nacked.push(lease.entry_id.clone());
		Ok(())
	}

	fn renew_lease(
		&mut self,
		_lease: &DispatchLease,
		_now_unix_ms: u64,
	) -> Result<Option<DispatchLease>, StoreError> {
		Ok(None)
	}

	fn backpressure(&self) -> roku_state_store::BackpressureSnapshot {
		let state = self
			.state
			.lock()
			.expect("dispatch state lock should succeed");
		roku_state_store::BackpressureSnapshot {
			queued: state.queued.len(),
			leased: 0,
			max_in_flight: usize::MAX,
			available_slots: usize::MAX,
		}
	}
}

#[test]
fn service_routes_ready_nodes_through_dispatch_queue() {
	let dispatch = RecordingDispatchQueue::default();
	let snapshot = dispatch.clone();
	let service = RuntimeService::new_with_runtime_data_plane_and_metrics(
		RuntimeDataPlane {
			task_repo: Box::new(roku_state_store::InMemoryTaskRepository::default()),
			event_repo: Box::new(roku_state_store::InMemoryEventRepository::default()),
			approval_repo: Box::new(roku_state_store::InMemoryApprovalRepository::default()),
			result_repo: Box::new(roku_state_store::InMemoryResultRepository::default()),
			dispatch_queue: Box::new(dispatch),
			artifact_store: roku_artifact_store::ArtifactStore::default(),
			experiment_registry: roku_experiment_registry::ExperimentRegistry::default(),
		},
		Arc::new(roku_observability::InMemoryAuditSink::default()),
		GenericAgentRuntime::default(),
		Arc::new(roku_observability::Metrics::default()),
		Box::new(roku_task_planner::AdaptiveTaskPlanner::default()),
	);

	let response = service
		.execute(sample_request())
		.expect("runtime service should succeed");
	assert_eq!(response.status, ResponseStatus::Succeeded);

	let (published, claimed, acked, _) = snapshot.snapshot();
	assert_eq!(published.len(), claimed.len());
	assert_eq!(claimed.len(), acked.len());
	assert!(published.len() >= 2);
}

#[test]
fn service_ignores_foreign_dispatch_entries_for_other_tasks() {
	let dispatch = RecordingDispatchQueue::default();
	dispatch.seed(DispatchEnvelope {
		entry_id: "foreign-entry".to_string(),
		task_id: TaskId("task-foreign".to_string()),
		node_id: NodeId("use-tool-1".to_string()),
		attempt: 1,
		payload: "foreign payload".to_string(),
	});
	let snapshot = dispatch.clone();
	let service = RuntimeService::new_with_runtime_data_plane_and_metrics(
		RuntimeDataPlane {
			task_repo: Box::new(roku_state_store::InMemoryTaskRepository::default()),
			event_repo: Box::new(roku_state_store::InMemoryEventRepository::default()),
			approval_repo: Box::new(roku_state_store::InMemoryApprovalRepository::default()),
			result_repo: Box::new(roku_state_store::InMemoryResultRepository::default()),
			dispatch_queue: Box::new(dispatch),
			artifact_store: roku_artifact_store::ArtifactStore::default(),
			experiment_registry: roku_experiment_registry::ExperimentRegistry::default(),
		},
		Arc::new(roku_observability::InMemoryAuditSink::default()),
		GenericAgentRuntime::default(),
		Arc::new(roku_observability::Metrics::default()),
		Box::new(roku_task_planner::AdaptiveTaskPlanner::default()),
	);

	let response = service
		.execute(sample_request())
		.expect("runtime service should succeed");
	assert_eq!(response.status, ResponseStatus::Succeeded);

	let (_, claimed, acked, nacked) = snapshot.snapshot();
	assert!(claimed.contains(&"use-tool-1".to_string()));
	assert!(nacked.contains(&"foreign-entry".to_string()));
	assert!(!acked.contains(&"foreign-entry".to_string()));
}

#[test]
fn service_handles_bare_roku_ping_without_tool_failure() {
	let service = RuntimeService::in_memory();
	let mut request = sample_request();
	request.goal = "roku".to_string();

	let response = service
		.execute(request)
		.expect("runtime service should handle direct roku ping");

	assert_eq!(response.status, ResponseStatus::Succeeded);
}

#[test]
fn service_handles_greeting_with_roku_without_tool_failure() {
	let service = RuntimeService::in_memory();
	let mut request = sample_request();
	request.goal = "你好 roku".to_string();

	let response = service
		.execute(request)
		.expect("runtime service should handle greeting");

	assert_eq!(response.status, ResponseStatus::Succeeded);
}

#[test]
fn service_handles_inventory_format_followup_without_tool_failure() {
	let service = RuntimeService::in_memory();
	let mut request = sample_request();
	request.goal = "用无序列表列一下".to_string();
	request.conversation_history = vec![
		ConversationTurn {
			role: ConversationRole::User,
			content: "列出 skill、tool".to_string(),
			created_at_unix_ms: 0,
		},
		ConversationTurn {
			role: ConversationRole::Assistant,
			content: "已安装的 skills: xlsx（处理 Excel 文件）、skill-creator（创建和测试技能）、claude-api。可用工具: data.execute（数据处理）、research.synthesize（研究总结）、review.assess（评审）. 此外还有能力类别: data.read、information.read、review.check.".to_string(),
			created_at_unix_ms: 0,
		},
	];

	let response = service
		.execute(request)
		.expect("runtime service should handle inventory format follow-up");

	assert_eq!(response.status, ResponseStatus::Succeeded);
}
