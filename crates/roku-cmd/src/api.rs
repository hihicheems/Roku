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

//! HTTP gateway bootstrap for command-owned local deployments.
//!
//! The CLI crate only owns process startup and env parsing here. Request validation and runtime
//! execution semantics stay inside the API gateway and runtime service crates.

use std::env;
use std::sync::Arc;

use actix_web::{App, HttpServer, web};
use roku_api_gateway::{GatewayAppState, RuntimeServiceExecutor, configure_routes};
use roku_observability::{LogLevel, LogRecord, emit_global_log};

use crate::CommandError;
use crate::runtime::build_live_runtime_service_from_env;

/// Process-local server settings for the embedded API gateway.
///
/// These values are intentionally small and startup-scoped so the command surface can keep HTTP
/// bootstrap concerns separate from runtime config owned by other crates.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ApiGatewayServerConfig {
	bind_addr: String,
	json_limit_bytes: usize,
}

impl ApiGatewayServerConfig {
	fn from_env() -> Result<Self, CommandError> {
		let bind_addr = env::var("ROKU_API_BIND_ADDR")
			.ok()
			.filter(|value| !value.trim().is_empty())
			.unwrap_or_else(|| "127.0.0.1:8787".to_string());
		let json_limit_bytes = match env::var("ROKU_API_JSON_LIMIT_BYTES") {
			Ok(value) if !value.trim().is_empty() => value.parse::<usize>().map_err(|error| {
				CommandError::ApiGatewayBootstrap(format!(
					"invalid ROKU_API_JSON_LIMIT_BYTES: {error}"
				))
			})?,
			Ok(_) | Err(env::VarError::NotPresent) => 8 * 1024,
			Err(error) => {
				return Err(CommandError::ApiGatewayBootstrap(format!(
					"failed to read ROKU_API_JSON_LIMIT_BYTES: {error}"
				)));
			}
		};

		Ok(Self {
			bind_addr,
			json_limit_bytes,
		})
	}
}

/// Boots the HTTP gateway and blocks the current process until the server exits.
///
/// This command shares the same live runtime bootstrap path as Telegram and `live-once`, so all
/// three surfaces observe the same plugin inventory and fallback mode report.
pub(crate) fn run_api_gateway_from_env() -> Result<(), CommandError> {
	let config = ApiGatewayServerConfig::from_env()?;
	let service = Arc::new(build_live_runtime_service_from_env()?);
	let executor = Arc::new(RuntimeServiceExecutor::new(service));
	let state = web::Data::new(GatewayAppState::new(executor));
	let bind_addr = config.bind_addr.clone();
	let json_limit_bytes = config.json_limit_bytes;
	let _ = emit_global_log(
		LogRecord::new("roku-cmd", LogLevel::Info, "starting api gateway server")
			.with_field("bind_addr", bind_addr.clone())
			.with_field("json_limit_bytes", json_limit_bytes.to_string()),
	);

	actix_web::rt::System::new()
		.block_on(async move {
			HttpServer::new(move || {
				App::new()
					.app_data(state.clone())
					.app_data(web::JsonConfig::default().limit(json_limit_bytes))
					.configure(configure_routes)
			})
			.bind(&bind_addr)?
			.run()
			.await
		})
		.map_err(|error| CommandError::ApiGatewayBootstrap(error.to_string()))
}

#[cfg(test)]
mod tests {
	use std::collections::HashMap;
	use std::sync::{Arc, Mutex};

	use actix_web::test as actix_test;
	use actix_web::{App, web};
	use roku_api_gateway::{
		ExperimentResponse, GatewayAppState, RuntimeServiceExecutor, SubmitRequest, SubmitResponse,
		configure_routes,
	};
	use roku_memory::{PendingLoopSnapshot, PendingLoopSnapshotBackend, PendingLoopSnapshotError};
	use roku_runtime_service::RuntimeService;

	use super::ApiGatewayServerConfig;
	use crate::pending_loop_substrate::MemoryPendingLoopSnapshotStore;
	use crate::test_support::pending_inventory_resume_success_loop_state;

	#[derive(Clone, Default)]
	struct RecordingPendingLoopSnapshotBackend {
		snapshots: Arc<Mutex<HashMap<String, PendingLoopSnapshot>>>,
		events: Arc<Mutex<Vec<String>>>,
	}

	impl RecordingPendingLoopSnapshotBackend {
		fn seed(&self, session_id: &str, snapshot: PendingLoopSnapshot) {
			self.snapshots
				.lock()
				.expect("snapshot seed lock should not be poisoned")
				.insert(session_id.to_string(), snapshot);
		}

		fn snapshot(&self, session_id: &str) -> Option<PendingLoopSnapshot> {
			self.snapshots
				.lock()
				.expect("snapshot load lock should not be poisoned")
				.get(session_id)
				.cloned()
		}

		fn events(&self) -> Vec<String> {
			self.events
				.lock()
				.expect("event log lock should not be poisoned")
				.clone()
		}
	}

	impl PendingLoopSnapshotBackend for RecordingPendingLoopSnapshotBackend {
		fn load_pending_loop_snapshot(
			&self,
			session_id: &str,
		) -> Result<Option<PendingLoopSnapshot>, PendingLoopSnapshotError> {
			self.events
				.lock()
				.expect("event log lock should not be poisoned")
				.push(format!("load:{session_id}"));
			Ok(self.snapshot(session_id))
		}

		fn save_pending_loop_snapshot(
			&self,
			session_id: &str,
			snapshot: Option<PendingLoopSnapshot>,
		) -> Result<(), PendingLoopSnapshotError> {
			let event = if snapshot.is_some() { "save" } else { "clear" };
			self.events
				.lock()
				.expect("event log lock should not be poisoned")
				.push(format!("{event}:{session_id}"));
			let mut snapshots = self
				.snapshots
				.lock()
				.expect("snapshot save lock should not be poisoned");
			if let Some(snapshot) = snapshot {
				snapshots.insert(session_id.to_string(), snapshot);
			} else {
				snapshots.remove(session_id);
			}
			Ok(())
		}
	}

	#[test]
	fn api_gateway_server_config_defaults_are_stable() {
		let config = ApiGatewayServerConfig {
			bind_addr: "127.0.0.1:8787".to_string(),
			json_limit_bytes: 8 * 1024,
		};

		assert_eq!(config.bind_addr, "127.0.0.1:8787");
		assert_eq!(config.json_limit_bytes, 8 * 1024);
	}

	#[tokio::test(flavor = "multi_thread")]
	async fn submit_route_resumes_pending_loop_snapshots_from_shared_memory_substrate() {
		let backend = RecordingPendingLoopSnapshotBackend::default();
		let (pending_loop, selected_topic) = pending_inventory_resume_success_loop_state();
		let session_id = pending_loop.session_id.clone();
		backend.seed(
			&session_id,
			PendingLoopSnapshot {
				run_id: pending_loop.run_id.clone(),
				loop_state_json: serde_json::to_string(&pending_loop)
					.expect("pending loop should encode"),
			},
		);
		let service = RuntimeService::default().with_pending_loop_snapshot_store(Arc::new(
			MemoryPendingLoopSnapshotStore::new(Box::new(backend.clone())),
		));
		let state = web::Data::new(GatewayAppState::new(Arc::new(RuntimeServiceExecutor::new(
			Arc::new(service),
		))));
		let app = actix_test::init_service(
			App::new()
				.app_data(state)
				.app_data(web::JsonConfig::default().limit(8 * 1024))
				.configure(configure_routes),
		)
		.await;

		let submit_request = actix_test::TestRequest::post()
			.uri("/v1/requests")
			.set_json(&SubmitRequest {
				session_id: session_id.clone(),
				goal: selected_topic,
			})
			.to_request();
		let submit_response: SubmitResponse =
			actix_test::call_and_read_body_json(&app, submit_request).await;

		assert_eq!(submit_response.request_id, "req-1");
		assert_eq!(submit_response.status, "succeeded");

		let experiment_request = actix_test::TestRequest::get()
			.uri("/v1/tasks/task-req-1/experiment")
			.to_request();
		let experiment_response: ExperimentResponse =
			actix_test::call_and_read_body_json(&app, experiment_request).await;

		assert_eq!(experiment_response.strategy, "runtime_loop_resume");
		assert_eq!(experiment_response.status, "succeeded");
		assert!(
			backend.snapshot(&session_id).is_none(),
			"API submit should consume the shared pending loop snapshot"
		);

		let events = backend.events();
		assert!(
			events.contains(&format!("load:{session_id}")),
			"shared pending loop substrate should be read through the memory adapter"
		);
		assert!(
			events.contains(&format!("clear:{session_id}")),
			"shared pending loop substrate should clear the consumed snapshot"
		);
	}
}
