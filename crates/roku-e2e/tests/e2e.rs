use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use actix_web::{App, web};
use roku_api_gateway::{
	ApprovalDecisionRequest, ApprovalExecutor, ApprovalTicketResponse, ArtifactContentResponse,
	ArtifactResponse, ExperimentResponse, GatewayAppState, RequestExecutor, RuntimeServiceExecutor,
	SubmitRequest, SubmitResponse, TaskDataExecutor, configure_routes,
};
use roku_cmd::{RunMode, run_once, run_with_mode};
use roku_common_types::{
	ApprovalDecision, ApprovalId, ApprovalStatus, Artifact, ArtifactId, ExperimentRun,
	RequestEnvelope, RequestId, ResponseStatus, RuntimeError, TaskId, TaskReplayReport, TaskState,
};
use roku_runtime_service::RuntimeService;
use roku_state_store::TaskRepository;

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
	let store_config = roku_state_store::SqliteStoreConfig::new(paths.state_db.clone());
	RuntimeService::new_with_runtime_data_plane_and_metrics(
		roku_runtime_service::RuntimeDataPlane {
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
		roku_agent_runtime::GenericAgentRuntime::default(),
		Arc::new(roku_observability::Metrics::default()),
		Box::new(roku_task_planner::AdaptiveTaskPlanner),
	)
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
