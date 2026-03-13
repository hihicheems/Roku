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

//! Capability-aware dynamic agent runtime.

mod result;
mod router;
mod runtime;
mod runtime_loop;
mod tool_config;
mod tools;
mod workers;

pub use roku_plugin_core::PluginRegistrySnapshot;
pub use router::{
	DirectRouteExecutionResult, DirectRoutePlan, EscalationAction, EscalationReason, IntentFamily,
	RouteDecision, RouteDecisionResult, RouteEscalationPlan, RouteRisk,
};
pub use runtime::{AgentWorker, GenericAgentRuntime, RuntimeWorker};
pub use runtime_loop::{
	AskUserPayload, AskUserResumeContract, AskUserResumeDirective, FinalAnswerPayload,
	InterpretedObservation, LoopContext, LoopRequest, LoopState, LoopStatus, NextStepAction,
	NextStepDecision, NextStepDecisionSchemaError, StepAction, StepObservation, StepRecord,
	ToolObservation, interpret_observation, should_resume_awaiting_user,
};
pub use tool_config::{BuiltinToolRole, ConfiguredTool, ToolCatalogConfig, ToolCatalogConfigError};
