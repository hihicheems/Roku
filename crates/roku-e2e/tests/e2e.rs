use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use actix_web::{App, web};
use roku_api_gateway::{
	ApprovalDecisionRequest, ApprovalExecutor, ApprovalTicketResponse, ArtifactContentResponse,
	ArtifactResponse, ExperimentResponse, GatewayAppState, RequestExecutor, RuntimeServiceExecutor,
	SubmitRequest, SubmitResponse, TaskDataExecutor, configure_routes,
};
use roku_cmd::{RunMode, run_once, run_with_mode};
use roku_common_types::{
	ApprovalDecision, ApprovalId, ApprovalStatus, Artifact, ArtifactId, EvidenceItem,
	ExperimentRun, PlanOutline, PlanStep, RequestEnvelope, RequestId, ResponseStatus,
	ResultEnvelope, ResultStatus, RuntimeError, TaskId, TaskNode, TaskReplayReport, TaskState,
};
use roku_planning_engine::PlanningDecision;
use roku_runtime_service::{RuntimeDataPlane, RuntimeService};
use roku_state_store::{
	DispatchClaim, DispatchEnvelope, DispatchLease, DispatchQueue, RetryClaim, StoreError,
	TaskRepository,
};

fn unique_path(suffix: &str) -> PathBuf {
	let nanos = SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.expect("clock should be after epoch")
		.as_nanos();
	std::env::temp_dir().join(format!("roku-e2e-{suffix}-{nanos}"))
}

#[derive(Clone)]
struct FileBackedPaths {
	state_db: PathBuf,
	artifacts: PathBuf,
	experiments: PathBuf,
}

fn file_backed_paths(prefix: &str) -> FileBackedPaths {
	FileBackedPaths {
		state_db: unique_path(&format!("{prefix}-state")).join("control-plane.db"),
		artifacts: unique_path(&format!("{prefix}-artifacts")),
		experiments: unique_path(&format!("{prefix}-experiments")),
	}
}

fn file_backed_runtime_service(paths: &FileBackedPaths) -> RuntimeService {
	file_backed_runtime_service_with_runtime_and_planner(
		paths,
		roku_agent_runtime::GenericAgentRuntime::default(),
		Box::new(roku_task_planner::AdaptiveTaskPlanner::default()),
	)
}

fn file_backed_runtime_service_with_runtime_and_planner(
	paths: &FileBackedPaths,
	runtime: roku_agent_runtime::GenericAgentRuntime,
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

fn in_memory_runtime_service_with_runtime_queue_and_planner(
	runtime: roku_agent_runtime::GenericAgentRuntime,
	queue: Box<dyn DispatchQueue + Send>,
	planner: Box<dyn roku_task_planner::TaskPlanner + Send + Sync>,
) -> RuntimeService {
	RuntimeService::new_with_runtime_data_plane_and_metrics(
		RuntimeDataPlane {
			task_repo: Box::new(roku_state_store::InMemoryTaskRepository::default()),
			event_repo: Box::new(roku_state_store::InMemoryEventRepository::default()),
			approval_repo: Box::new(roku_state_store::InMemoryApprovalRepository::default()),
			result_repo: Box::new(roku_state_store::InMemoryResultRepository::default()),
			dispatch_queue: queue,
			artifact_store: roku_artifact_store::ArtifactStore::default(),
			experiment_registry: roku_experiment_registry::ExperimentRegistry::default(),
		},
		Arc::new(roku_observability::InMemoryAuditSink::default()),
		runtime,
		Arc::new(roku_observability::Metrics::default()),
		planner,
	)
}

struct SingleStepPlanner;

impl roku_task_planner::TaskPlanner for SingleStepPlanner {
	fn build_outline(
		&self,
		request: &RequestEnvelope,
		_decision: &PlanningDecision,
	) -> PlanOutline {
		PlanOutline {
			goal: request.goal.clone(),
			steps: vec![PlanStep {
				step_id: "single-step".to_string(),
				summary: "single-step".to_string(),
				resource_selectors: Vec::new(),
				required_capabilities: Vec::new(),
				requires_approval: false,
				depends_on: Vec::new(),
				branch: None,
				loop_control: None,
			}],
		}
	}
}

struct FixedApprovalPlanner;

impl roku_task_planner::TaskPlanner for FixedApprovalPlanner {
	fn build_outline(
		&self,
		request: &RequestEnvelope,
		_decision: &PlanningDecision,
	) -> PlanOutline {
		PlanOutline {
			goal: request.goal.clone(),
			steps: vec![PlanStep {
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

#[derive(Clone)]
struct CountingWorker {
	executions: Arc<AtomicUsize>,
}

impl roku_agent_runtime::RuntimeWorker for CountingWorker {
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

#[derive(Clone)]
struct ProviderWorker {
	available: bool,
}

impl roku_agent_runtime::RuntimeWorker for ProviderWorker {
	fn worker_id(&self) -> &'static str {
		"provider-worker"
	}

	fn supports(&self, _capabilities: &[String]) -> bool {
		true
	}

	fn execute(
		&self,
		spec: &roku_common_types::AgentInstanceSpec,
		node: &TaskNode,
	) -> ResultEnvelope {
		if self.available {
			ResultEnvelope {
				task_id: spec.context.task_id.clone(),
				node_id: node.node_id.clone(),
				producer: spec.instance_id.clone(),
				schema_version: "result.v1".to_string(),
				status: ResultStatus::Ok,
				payload: r#"{"message":"provider recovered"}"#.to_string(),
				evidence: vec![EvidenceItem {
					kind: "runtime".to_string(),
					value: "provider-worker".to_string(),
				}],
				confidence: 0.9,
			}
		} else {
			ResultEnvelope {
				task_id: spec.context.task_id.clone(),
				node_id: node.node_id.clone(),
				producer: spec.instance_id.clone(),
				schema_version: "result.v1".to_string(),
				status: ResultStatus::Error,
				payload:
					r#"{"error_code":"provider_unavailable","message":"provider unavailable"}"#
						.to_string(),
				evidence: vec![EvidenceItem {
					kind: "tool_error".to_string(),
					value: "provider_unavailable".to_string(),
				}],
				confidence: 0.0,
			}
		}
	}
}

#[derive(Default)]
struct DuplicateDispatchQueueState {
	current: Option<DispatchEnvelope>,
	remaining_duplicates: usize,
	lease_sequence: u64,
}

#[derive(Clone, Default)]
struct DuplicateDispatchQueue {
	state: Arc<Mutex<DuplicateDispatchQueueState>>,
}

impl DispatchQueue for DuplicateDispatchQueue {
	fn publish(&mut self, envelope: DispatchEnvelope) -> Result<(), StoreError> {
		let mut state = self.state.lock().map_err(|_| {
			StoreError::Storage("duplicate dispatch queue lock poisoned".to_string())
		})?;
		state.current = Some(envelope);
		state.remaining_duplicates = 2;
		Ok(())
	}

	fn claim(
		&mut self,
		consumer_id: &str,
		now_unix_ms: u64,
	) -> Result<Option<DispatchClaim>, StoreError> {
		let mut state = self.state.lock().map_err(|_| {
			StoreError::Storage("duplicate dispatch queue lock poisoned".to_string())
		})?;
		let Some(envelope) = state.current.clone() else {
			return Ok(None);
		};
		if state.remaining_duplicates == 0 {
			state.current = None;
			return Ok(None);
		}
		state.remaining_duplicates = state.remaining_duplicates.saturating_sub(1);
		state.lease_sequence = state.lease_sequence.saturating_add(1);
		Ok(Some(DispatchClaim {
			envelope,
			lease: DispatchLease {
				entry_id: "duplicate-entry".to_string(),
				consumer_id: consumer_id.to_string(),
				lease_token: format!("dup-lease-{}", state.lease_sequence),
				expires_at_unix_ms: now_unix_ms.saturating_add(1_000),
			},
		}))
	}

	fn ack(&mut self, _lease: &DispatchLease) -> Result<(), StoreError> {
		Ok(())
	}

	fn nack(&mut self, _lease: &DispatchLease, _retry: RetryClaim) -> Result<(), StoreError> {
		Ok(())
	}

	fn renew_lease(
		&mut self,
		_lease: &DispatchLease,
		now_unix_ms: u64,
	) -> Result<Option<DispatchLease>, StoreError> {
		Ok(Some(DispatchLease {
			entry_id: "duplicate-entry".to_string(),
			consumer_id: "duplicate-consumer".to_string(),
			lease_token: "duplicate-renewed".to_string(),
			expires_at_unix_ms: now_unix_ms.saturating_add(1_000),
		}))
	}

	fn backpressure(&self) -> roku_state_store::BackpressureSnapshot {
		roku_state_store::BackpressureSnapshot {
			queued: 0,
			leased: 0,
			max_in_flight: 1,
			available_slots: 1,
		}
	}
}

fn counting_runtime(executions: Arc<AtomicUsize>) -> roku_agent_runtime::GenericAgentRuntime {
	let mut runtime = roku_agent_runtime::GenericAgentRuntime::default();
	runtime.register_worker(100, CountingWorker { executions });
	runtime
}

fn provider_runtime(available: bool) -> roku_agent_runtime::GenericAgentRuntime {
	let mut runtime = roku_agent_runtime::GenericAgentRuntime::default();
	runtime.register_worker(100, ProviderWorker { available });
	runtime
}

#[test]
fn e2e_happy_path_succeeds() {
	let response = run_once("build execution graph").expect("pipeline should run");
	assert!(matches!(response.status, ResponseStatus::Succeeded));
}

#[test]
fn e2e_validation_failure_path_is_reported() {
	let response = run_with_mode("build execution graph", RunMode::MissingEvidence)
		.expect("pipeline should return failed response");
	assert!(matches!(response.status, ResponseStatus::Failed));
	assert!(response.message.contains("evidence is required"));
}

#[test]
fn e2e_capability_denied_path_is_reported() {
	let response = run_with_mode("build execution graph", RunMode::CapabilityDenied)
		.expect("pipeline should return failed response");
	assert!(matches!(response.status, ResponseStatus::Failed));
	assert!(response.message.contains("capability denied"));
}

#[test]
fn e2e_pending_approval_path_is_reported() {
	let response = run_with_mode("build execution graph", RunMode::ApprovalRequired)
		.expect("pipeline should return pending approval response");
	assert!(matches!(response.status, ResponseStatus::PendingApproval));
	assert!(response.message.contains("approval required"));
}

#[test]
fn e2e_dead_letter_path_is_reported() {
	let response = run_with_mode("build execution graph", RunMode::RetryExhausted)
		.expect("pipeline should return dead-letter response");
	assert!(matches!(response.status, ResponseStatus::Failed));
	assert!(response.message.contains("dead-lettered"));
}

#[test]
fn e2e_timeout_recovery_roundtrip_succeeds() {
	let service = RuntimeService::default();
	let response = service
		.execute_with_mode(
			RequestEnvelope {
				request_id: RequestId("req-timeout".to_string()),
				session_id: "timeout-session".to_string(),
				goal: "build execution graph".to_string(),
				planning_mode_hint: None,
				conversation_history: Vec::new(),
			},
			RunMode::TimeoutRecovery,
		)
		.expect("pipeline should enter timeout recovery");
	assert!(matches!(response.status, ResponseStatus::Failed));

	let resumed = service
		.recover_timed_out_task(&TaskId("task-req-timeout".to_string()))
		.expect("timeout recovery should resume the task");
	assert!(matches!(resumed.status, ResponseStatus::Succeeded));

	let task = service
		.get_task(&TaskId("task-req-timeout".to_string()))
		.expect("task lookup should succeed")
		.expect("task should exist");
	assert_eq!(task.state, TaskState::Succeeded);
}

#[test]
fn e2e_cancelled_approval_flow_records_cancelled_ticket() {
	let service = RuntimeService::default();
	let pending = service
		.execute_with_mode(
			RequestEnvelope {
				request_id: RequestId("req-cancel".to_string()),
				session_id: "cancel-session".to_string(),
				goal: "build execution graph".to_string(),
				planning_mode_hint: None,
				conversation_history: Vec::new(),
			},
			RunMode::ApprovalRequired,
		)
		.expect("pipeline should stop for approval");
	assert!(matches!(pending.status, ResponseStatus::PendingApproval));
	let approval_id = ApprovalId(
		pending.artifacts[0]
			.trim_start_matches("approval://")
			.to_string(),
	);

	let task = service
		.cancel_task(&TaskId("task-req-cancel".to_string()), "operator")
		.expect("task cancellation should succeed");
	assert_eq!(task.state, TaskState::Cancelled);

	let ticket = service
		.get_approval(&approval_id)
		.expect("approval lookup should succeed")
		.expect("approval ticket should exist");
	assert_eq!(ticket.status, ApprovalStatus::Cancelled);
}

#[test]
fn e2e_replay_reconstructs_progress_after_restart_with_stale_snapshot() {
	let paths = file_backed_paths("replay-restart");
	let service = file_backed_runtime_service(&paths);
	let pending = service
		.execute_with_mode(
			RequestEnvelope {
				request_id: RequestId("req-replay".to_string()),
				session_id: "replay-session".to_string(),
				goal: "build execution graph".to_string(),
				planning_mode_hint: None,
				conversation_history: Vec::new(),
			},
			RunMode::ApprovalRequired,
		)
		.expect("pipeline should stop for approval");
	assert!(matches!(pending.status, ResponseStatus::PendingApproval));

	let task_id = TaskId("task-req-replay".to_string());
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

	let restarted_service = file_backed_runtime_service(&paths);
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
				comment: Some("recover after restart".to_string()),
			},
		)
		.expect("approval should resume task after restart");
	assert!(matches!(resumed.status, ResponseStatus::Succeeded));

	let task = restarted_service
		.get_task(&task_id)
		.expect("task lookup should succeed")
		.expect("task should exist");
	assert_eq!(task.state, TaskState::Succeeded);
}

#[test]
fn e2e_replay_snapshot_compaction_preserves_restart_recovery() {
	let paths = file_backed_paths("replay-compaction");
	let service = file_backed_runtime_service(&paths);
	let pending = service
		.execute_with_mode(
			RequestEnvelope {
				request_id: RequestId("req-replay-compact".to_string()),
				session_id: "replay-compact-session".to_string(),
				goal: "build execution graph".to_string(),
				planning_mode_hint: None,
				conversation_history: Vec::new(),
			},
			RunMode::ApprovalRequired,
		)
		.expect("pipeline should stop for approval");
	assert!(matches!(pending.status, ResponseStatus::PendingApproval));

	let task_id = TaskId("task-req-replay-compact".to_string());
	service
		.compact_task_replay(&task_id, 1)
		.expect("replay compaction should succeed");

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

	let restarted_service = file_backed_runtime_service(&paths);
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
				comment: Some("recover after compaction".to_string()),
			},
		)
		.expect("approval should resume task after compaction");
	assert!(matches!(resumed.status, ResponseStatus::Succeeded));
}

#[test]
fn e2e_duplicate_delivery_executes_node_once() {
	let executions = Arc::new(AtomicUsize::new(0));
	let service = in_memory_runtime_service_with_runtime_queue_and_planner(
		counting_runtime(executions.clone()),
		Box::new(DuplicateDispatchQueue::default()),
		Box::new(SingleStepPlanner),
	);

	let response = service
		.execute(RequestEnvelope {
			request_id: RequestId("req-duplicate-delivery".to_string()),
			session_id: "duplicate-delivery-session".to_string(),
			goal: "exercise duplicate dispatch delivery".to_string(),
			planning_mode_hint: None,
			conversation_history: Vec::new(),
		})
		.expect("duplicate delivery run should succeed");

	assert!(matches!(response.status, ResponseStatus::Succeeded));
	assert_eq!(executions.load(Ordering::SeqCst), 1);
}

#[test]
fn e2e_sqlite_lease_expiry_requeues_to_another_consumer() {
	let paths = file_backed_paths("lease-expiry");
	let store_config = roku_state_store::SqliteStoreConfig::new(paths.state_db.clone());
	let mut queue = roku_state_store::SqliteDispatchQueue::with_limits(store_config, 1, 10)
		.expect("sqlite dispatch queue should open");
	queue
		.publish(DispatchEnvelope {
			entry_id: "lease-expiry-entry".to_string(),
			task_id: TaskId("task-lease-expiry".to_string()),
			node_id: roku_common_types::NodeId("node-lease-expiry".to_string()),
			attempt: 1,
			payload: "lease-expiry".to_string(),
		})
		.expect("dispatch publish should succeed");

	let first = queue
		.claim("worker-a", 100)
		.expect("first claim should succeed")
		.expect("entry should be claimable");
	assert_eq!(first.lease.consumer_id, "worker-a");

	let blocked = queue
		.claim("worker-b", 105)
		.expect("second claim should succeed before expiry");
	assert!(blocked.is_none());

	let reclaimed = queue
		.claim("worker-b", 111)
		.expect("claim after expiry should succeed")
		.expect("expired lease should be requeued");
	assert_eq!(reclaimed.envelope.entry_id, first.envelope.entry_id);
	assert_eq!(reclaimed.lease.consumer_id, "worker-b");
}

#[test]
fn e2e_provider_loss_is_reported_and_recoverable_after_restart() {
	let paths = file_backed_paths("provider-loss");
	let unavailable_service = file_backed_runtime_service_with_runtime_and_planner(
		&paths,
		provider_runtime(false),
		Box::new(SingleStepPlanner),
	);
	let task_id = TaskId("task-req-provider-loss".to_string());

	let failed = unavailable_service
		.execute(RequestEnvelope {
			request_id: RequestId("req-provider-loss".to_string()),
			session_id: "provider-loss-session".to_string(),
			goal: "exercise provider loss".to_string(),
			planning_mode_hint: None,
			conversation_history: Vec::new(),
		})
		.expect("provider loss run should return a failure response");
	assert!(matches!(failed.status, ResponseStatus::Failed));
	assert!(failed.message.contains("provider unavailable"));

	let restarted_service = file_backed_runtime_service_with_runtime_and_planner(
		&paths,
		provider_runtime(true),
		Box::new(SingleStepPlanner),
	);
	let replay = restarted_service
		.get_task_replay_report(&task_id)
		.expect("replay report lookup should succeed")
		.expect("replay report should exist");
	assert!(replay.recoverable);
	assert!(matches!(
		replay.recovery_eligibility,
		roku_common_types::RecoveryEligibility::ResumeReady
	));

	let resumed = restarted_service
		.resume_task(&task_id)
		.expect("restart should resume after provider recovery");
	assert!(matches!(resumed.status, ResponseStatus::Succeeded));
}

#[test]
fn e2e_restart_stress_preserves_progress_without_duplicate_execution() {
	let paths = file_backed_paths("restart-stress");
	let executions = Arc::new(AtomicUsize::new(0));
	let service = file_backed_runtime_service_with_runtime_and_planner(
		&paths,
		counting_runtime(executions.clone()),
		Box::new(FixedApprovalPlanner),
	);
	let pending = service
		.execute(RequestEnvelope {
			request_id: RequestId("req-restart-stress".to_string()),
			session_id: "restart-stress-session".to_string(),
			goal: "exercise restart stress".to_string(),
			planning_mode_hint: None,
			conversation_history: Vec::new(),
		})
		.expect("approval-gated run should pause");
	assert!(matches!(pending.status, ResponseStatus::PendingApproval));
	assert_eq!(executions.load(Ordering::SeqCst), 1);

	let task_id = TaskId("task-req-restart-stress".to_string());
	let approval_id = ApprovalId(
		pending.artifacts[0]
			.trim_start_matches("approval://")
			.to_string(),
	);

	for _ in 0..3 {
		let restarted_service = file_backed_runtime_service_with_runtime_and_planner(
			&paths,
			counting_runtime(executions.clone()),
			Box::new(FixedApprovalPlanner),
		);
		let task = restarted_service
			.get_task(&task_id)
			.expect("task lookup should succeed after restart")
			.expect("task should exist after restart");
		assert_eq!(task.state, TaskState::WaitingApproval);
		assert_eq!(executions.load(Ordering::SeqCst), 1);
	}

	let restarted_service = file_backed_runtime_service_with_runtime_and_planner(
		&paths,
		counting_runtime(executions.clone()),
		Box::new(FixedApprovalPlanner),
	);
	let resumed = restarted_service
		.decide_approval(
			&approval_id,
			ApprovalDecision {
				actor: "reviewer".to_string(),
				approved: true,
				comment: Some("survived repeated restarts".to_string()),
			},
		)
		.expect("approval should resume after restart stress");
	assert!(matches!(resumed.status, ResponseStatus::Succeeded));
	assert_eq!(executions.load(Ordering::SeqCst), 1);
}

#[actix_web::test]
async fn http_gateway_executes_runtime_service() {
	let service = Arc::new(RuntimeService::default());
	let state = web::Data::new(GatewayAppState::new(Arc::new(RuntimeServiceExecutor::new(
		service,
	))));
	let app = actix_web::test::init_service(
		App::new()
			.app_data(state)
			.app_data(web::JsonConfig::default().limit(8 * 1024))
			.configure(configure_routes),
	)
	.await;

	let request = actix_web::test::TestRequest::post()
		.uri("/v1/requests")
		.set_json(&SubmitRequest {
			session_id: "http-session".to_string(),
			goal: "build execution graph".to_string(),
		})
		.to_request();

	let response: SubmitResponse = actix_web::test::call_and_read_body_json(&app, request).await;
	assert_eq!(response.status, "succeeded");
	assert!(!response.message.is_empty());
	assert_eq!(response.message, "generic execution completed");
	assert!(response.request_id.starts_with("req-"));
}

#[actix_web::test]
async fn http_gateway_exposes_task_artifacts_and_experiment() {
	let service = Arc::new(RuntimeService::default());
	let state = web::Data::new(GatewayAppState::new(Arc::new(RuntimeServiceExecutor::new(
		service,
	))));
	let app = actix_web::test::init_service(
		App::new()
			.app_data(state)
			.app_data(web::JsonConfig::default().limit(8 * 1024))
			.configure(configure_routes),
	)
	.await;

	let request = actix_web::test::TestRequest::post()
		.uri("/v1/requests")
		.set_json(&SubmitRequest {
			session_id: "http-session".to_string(),
			goal: "build execution graph".to_string(),
		})
		.to_request();

	let response: SubmitResponse = actix_web::test::call_and_read_body_json(&app, request).await;
	let task_id = format!("task-{}", response.request_id);

	let artifacts_request = actix_web::test::TestRequest::get()
		.uri(&format!("/v1/tasks/{task_id}/artifacts"))
		.to_request();
	let artifacts: Vec<ArtifactResponse> =
		actix_web::test::call_and_read_body_json(&app, artifacts_request).await;
	assert_eq!(artifacts.len(), 3);
	assert!(
		artifacts
			.iter()
			.all(|artifact| artifact.uri.starts_with("artifact://"))
	);
	let artifact_id = artifacts
		.first()
		.expect("artifact should exist")
		.artifact_id
		.clone();

	let content_request = actix_web::test::TestRequest::get()
		.uri(&format!(
			"/v1/tasks/{task_id}/artifacts/{artifact_id}/content"
		))
		.to_request();
	let content_response: ArtifactContentResponse =
		actix_web::test::call_and_read_body_json(&app, content_request).await;
	assert_eq!(content_response.artifact_id, artifact_id);
	assert!(!content_response.content.is_empty());

	let download_request = actix_web::test::TestRequest::get()
		.uri(&format!(
			"/v1/tasks/{task_id}/artifacts/{artifact_id}/download"
		))
		.to_request();
	let download_body = actix_web::test::call_and_read_body(&app, download_request).await;
	assert!(!download_body.is_empty());

	let experiment_request = actix_web::test::TestRequest::get()
		.uri(&format!("/v1/tasks/{task_id}/experiment"))
		.to_request();
	let experiment: ExperimentResponse =
		actix_web::test::call_and_read_body_json(&app, experiment_request).await;
	assert_eq!(experiment.status, "succeeded");
	assert_eq!(experiment.artifact_ids.len(), 3);

	let replay_request = actix_web::test::TestRequest::get()
		.uri(&format!("/v1/tasks/{task_id}/replay"))
		.to_request();
	let replay: TaskReplayReport =
		actix_web::test::call_and_read_body_json(&app, replay_request).await;
	assert_eq!(replay.task_id.0, task_id);
	assert_eq!(replay.persisted_state, TaskState::Succeeded);
	assert!(replay.snapshot_matches_replay);
}

struct FixedModeExecutor {
	service: Arc<RuntimeService>,
	mode: RunMode,
}

impl RequestExecutor for FixedModeExecutor {
	fn execute(
		&self,
		request: RequestEnvelope,
	) -> Result<roku_common_types::ResponseEnvelope, RuntimeError> {
		self.service.execute_with_mode(request, self.mode)
	}
}

impl ApprovalExecutor for FixedModeExecutor {
	fn get_approval(
		&self,
		approval_id: &ApprovalId,
	) -> Result<Option<roku_common_types::ApprovalTicket>, RuntimeError> {
		self.service.get_approval(approval_id)
	}

	fn decide_approval(
		&self,
		approval_id: &ApprovalId,
		decision: ApprovalDecision,
	) -> Result<roku_common_types::ResponseEnvelope, RuntimeError> {
		self.service.decide_approval(approval_id, decision)
	}
}

impl TaskDataExecutor for FixedModeExecutor {
	fn list_artifacts(&self, task_id: &TaskId) -> Result<Vec<Artifact>, RuntimeError> {
		self.service.list_artifacts(task_id)
	}

	fn get_experiment_run(&self, task_id: &TaskId) -> Result<Option<ExperimentRun>, RuntimeError> {
		self.service.get_experiment_run(task_id)
	}

	fn get_artifact_content(
		&self,
		task_id: &TaskId,
		artifact_id: &ArtifactId,
	) -> Result<Option<String>, RuntimeError> {
		self.service.get_artifact_content(task_id, artifact_id)
	}

	fn get_task_replay_report(
		&self,
		task_id: &TaskId,
	) -> Result<Option<TaskReplayReport>, RuntimeError> {
		self.service.get_task_replay_report(task_id)
	}
}

#[actix_web::test]
async fn http_gateway_reports_validation_failure() {
	let executor = Arc::new(FixedModeExecutor {
		service: Arc::new(RuntimeService::default()),
		mode: RunMode::MissingEvidence,
	});
	let state = web::Data::new(GatewayAppState::new(executor));
	let app = actix_web::test::init_service(
		App::new()
			.app_data(state)
			.app_data(web::JsonConfig::default().limit(8 * 1024))
			.configure(configure_routes),
	)
	.await;

	let request = actix_web::test::TestRequest::post()
		.uri("/v1/requests")
		.set_json(&SubmitRequest {
			session_id: "http-session".to_string(),
			goal: "build execution graph".to_string(),
		})
		.to_request();

	let response: SubmitResponse = actix_web::test::call_and_read_body_json(&app, request).await;
	assert_eq!(response.status, "failed");
	assert!(response.message.contains("evidence is required"));
}

#[actix_web::test]
async fn http_gateway_reports_pending_approval() {
	let executor = Arc::new(FixedModeExecutor {
		service: Arc::new(RuntimeService::default()),
		mode: RunMode::ApprovalRequired,
	});
	let state = web::Data::new(GatewayAppState::new(executor));
	let app = actix_web::test::init_service(
		App::new()
			.app_data(state)
			.app_data(web::JsonConfig::default().limit(8 * 1024))
			.configure(configure_routes),
	)
	.await;

	let request = actix_web::test::TestRequest::post()
		.uri("/v1/requests")
		.set_json(&SubmitRequest {
			session_id: "http-session".to_string(),
			goal: "build execution graph".to_string(),
		})
		.to_request();

	let response: SubmitResponse = actix_web::test::call_and_read_body_json(&app, request).await;
	assert_eq!(response.status, "pending_approval");
	assert!(response.message.contains("approval required"));
}

#[actix_web::test]
async fn http_gateway_approval_roundtrip_resumes_task() {
	let service = Arc::new(RuntimeService::default());
	let executor = Arc::new(FixedModeExecutor {
		service: service.clone(),
		mode: RunMode::ApprovalRequired,
	});
	let state = web::Data::new(GatewayAppState::new(executor));
	let app = actix_web::test::init_service(
		App::new()
			.app_data(state)
			.app_data(web::JsonConfig::default().limit(8 * 1024))
			.configure(configure_routes),
	)
	.await;

	let submit_request = actix_web::test::TestRequest::post()
		.uri("/v1/requests")
		.set_json(&SubmitRequest {
			session_id: "http-session".to_string(),
			goal: "build execution graph".to_string(),
		})
		.to_request();

	let pending: SubmitResponse =
		actix_web::test::call_and_read_body_json(&app, submit_request).await;
	assert_eq!(pending.status, "pending_approval");
	let approval_id = pending.artifacts[0]
		.trim_start_matches("approval://")
		.to_string();

	let get_request = actix_web::test::TestRequest::get()
		.uri(&format!("/v1/approvals/{approval_id}"))
		.to_request();
	let ticket: ApprovalTicketResponse =
		actix_web::test::call_and_read_body_json(&app, get_request).await;
	assert_eq!(ticket.status, "pending");

	let decision_request = actix_web::test::TestRequest::post()
		.uri(&format!("/v1/approvals/{approval_id}/decision"))
		.set_json(&ApprovalDecisionRequest {
			actor: "reviewer".to_string(),
			approved: true,
			comment: Some("ship it".to_string()),
		})
		.to_request();
	let resumed: SubmitResponse =
		actix_web::test::call_and_read_body_json(&app, decision_request).await;
	assert_eq!(resumed.status, "succeeded");

	let get_request = actix_web::test::TestRequest::get()
		.uri(&format!("/v1/approvals/{approval_id}"))
		.to_request();
	let ticket: ApprovalTicketResponse =
		actix_web::test::call_and_read_body_json(&app, get_request).await;
	assert_eq!(ticket.status, "approved");
	assert_eq!(ticket.decided_by.as_deref(), Some("reviewer"));
}
