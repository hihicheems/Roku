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

//! Gateway request normalization and HTTP integration.

mod executor;
mod models;
mod routes;

pub use executor::{
	ApprovalExecutor, Gateway, GatewayAppState, GatewayExecutor, NoopExecutor, RawRequest,
	RequestExecutor, RuntimeServiceExecutor, TaskDataExecutor,
};
pub use models::{
	ApprovalDecisionRequest, ApprovalTicketResponse, ArtifactContentResponse, ArtifactResponse,
	ErrorResponse, ExperimentMetricResponse, ExperimentResponse, HealthResponse, SubmitRequest,
	SubmitResponse,
};
pub use routes::{
	configure_routes, decide_approval_handler, download_artifact_handler, get_approval_handler,
	get_artifact_content_handler, get_experiment_handler, get_task_artifacts_handler,
	health_handler, submit_handler,
};
