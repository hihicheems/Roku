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
mod runtime_config;
mod runtime_loop;
mod tool_config;
mod tools;
mod workers;

pub use roku_plugin_core::PluginRegistrySnapshot;
pub use router::{
	DirectRouteExecutionResult, DirectRoutePlan, EscalationAction, EscalationReason, IntentFamily,
	RouteDecision, RouteDecisionResult, RouteEscalationPlan, RouteRisk,
};
pub use runtime::{AgentWorker, AwaitingUserResumeAssessment, GenericAgentRuntime, RuntimeWorker};
pub use runtime_config::{
	AgentRuntimeConfig, AgentRuntimeConfigError, AgentRuntimeConfigPatch,
	HARD_MAX_CANDIDATE_DESCRIPTION_MAX_CHARS, HARD_MAX_CANDIDATE_INVENTORY_LIMIT,
	HARD_MAX_INITIAL_RECOVERY_BUDGET, HARD_MAX_INITIAL_STEP_BUDGET,
	HARD_MAX_NEXT_STEP_BUDGET_COST_REMAINING_USD, HARD_MAX_NEXT_STEP_BUDGET_TOKENS_REMAINING,
	HARD_MAX_NEXT_STEP_EXPECTED_OUTPUT_TOKENS, HARD_MAX_ROUTE_BUDGET_COST_REMAINING_USD,
	HARD_MAX_ROUTE_BUDGET_TOKENS_REMAINING, HARD_MAX_ROUTE_EXPECTED_OUTPUT_TOKENS,
	HARD_MAX_VISIBLE_TOOL_HINT_MAX_CHARS, LoopRuntimeConfig, LoopRuntimeConfigPatch,
	NextStepRuntimeConfig, NextStepRuntimeConfigPatch, PromptCompactionRuntimeConfig,
	PromptCompactionRuntimeConfigPatch, RouteClassifierRuntimeConfig,
	RouteClassifierRuntimeConfigPatch,
};
pub use runtime_loop::{
	AskUserPayload, AskUserResumeContract, AskUserResumeDirective, CompactConfig,
	FinalAnswerPayload, InterpretedFlagExpectation, InterpretedObservation, LoopContext, LoopEvent,
	LoopEventSender, LoopRequest, LoopState, LoopStatus, NextStepAction, NextStepDecision,
	NextStepDecisionSchemaError, RegressionSuiteKind, RuntimeLoopRegressionCaseReport,
	RuntimeLoopRegressionExpectation, RuntimeLoopTraceCheckReport, StepAction, StepObservation,
	StepRecord, ToolObservation, ToolProbeCheckReport, check_runtime_loop_trace,
	check_seed_tool_probe, compact_history, estimate_context_tokens,
	evaluate_runtime_loop_regression_case, interpret_observation, runtime_loop_trace,
	summarize_discarded_steps,
};
pub use tool_config::{
	BuiltinToolRole, CommandToolRuntimeConfig, CommandToolRuntimeConfigPatch, ConfiguredTool,
	FsToolRuntimeConfig, FsToolRuntimeConfigPatch, PythonToolRuntimeConfig,
	PythonToolRuntimeConfigPatch, TableToolRuntimeConfig, TableToolRuntimeConfigPatch,
	ToolCatalogConfig, ToolCatalogConfigError, ToolWorkerRuntimeConfig,
	ToolWorkerRuntimeConfigPatch, ToolsRuntimeConfig, ToolsRuntimeConfigError,
	ToolsRuntimeConfigPatch, WebToolRuntimeConfig, WebToolRuntimeConfigPatch,
};
