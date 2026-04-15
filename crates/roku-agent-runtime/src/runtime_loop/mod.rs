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

pub mod approval;
mod ask_user;
mod compact;
mod context_assembly;
pub(crate) mod environment;
mod execution_trace;
mod grounding;
pub(crate) mod loop_event;
mod loop_state;
mod next_step;
mod observation;
mod probe_check;
mod regression;
mod request_intake;
mod state_update;
mod step_record;
mod summarizer;
pub(crate) mod system_prompt;
mod tool_loop;
mod trace;

pub use approval::{
	ApprovalDecision as ToolApprovalDecision, AutoApproveGate, RiskBasedGate, ToolApprovalGate,
	ToolRiskLevel, classify_tool_risk,
};
pub(crate) use ask_user::effective_ask_user_payload;
pub use ask_user::{AskUserPayload, AskUserResumeContract, AskUserResumeDirective};
pub use compact::{
	CompactConfig, MICROCOMPACT_RETAIN_RECENT, MID_WATER_TRIGGER_RATIO, MidCompactOutcome,
	compact_history, compact_history_with_llm, compact_messages,
	compact_messages_with_structured_summary, estimate_context_tokens, estimate_prompt_pressure,
	estimate_prompt_tokens_calibrated, microcompact_old_tool_results, mid_compact_messages,
	summarize_discarded_steps, truncate_large_tool_results,
};
// Structured-summary compaction contract surface. Exported so downstream
// crates can construct / inspect the outcome directly and reuse the
// validation helper. These names are not referenced by name inside this
// crate (the return type flows through without destructuring by name), so
// the re-exports get an explicit `unused_imports` allow.
#[allow(unused_imports)]
pub use compact::{
	MAX_DROP_OLDEST_RETRIES, STRUCTURED_SUMMARY_SECTIONS, StructuredCompactError,
	StructuredCompactOutcome, validate_structured_summary,
};
pub use context_assembly::LoopContext;
pub(crate) use context_assembly::build_loop_context;
pub use loop_event::{LoopEvent, LoopEventSender};
pub use loop_state::{LoopState, LoopStatus};
pub use next_step::{NextStepAction, NextStepDecision, NextStepDecisionSchemaError};
pub use observation::{StepObservation, ToolObservation};
pub use probe_check::{ToolProbeCheckReport, check_seed_tool_probe};
pub use regression::{
	InterpretedFlagExpectation, RegressionSuiteKind, RuntimeLoopRegressionCaseReport,
	RuntimeLoopRegressionExpectation, evaluate_runtime_loop_regression_case,
};
pub use request_intake::LoopRequest;
pub(crate) use request_intake::intake_request;
pub use state_update::{InterpretedObservation, interpret_observation};
pub use step_record::{StepAction, StepRecord};
pub use summarizer::FinalAnswerPayload;
pub(crate) use tool_loop::{
	attachments_for_tool, build_tool_definitions, ground_tool_arguments,
	next_working_directory_from_observation,
};
pub use trace::{RuntimeLoopTraceCheckReport, check_runtime_loop_trace, runtime_loop_trace};
