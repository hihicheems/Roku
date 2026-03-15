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

mod ask_user;
mod context_assembly;
mod context_projection;
mod grounding;
mod loop_state;
mod next_step;
mod observation;
mod probe_check;
mod recovery;
mod regression;
mod request_intake;
mod route_classifier;
mod state_update;
mod step_record;
mod summarizer;
mod tool_loop;
mod trace;

pub(crate) use ask_user::effective_ask_user_payload;
pub use ask_user::{AskUserPayload, AskUserResumeContract, AskUserResumeDirective};
pub use context_assembly::LoopContext;
pub(crate) use context_assembly::build_loop_context;
pub(crate) use context_projection::build_context_projection;
pub use context_projection::{ContextProjection, VisibleToolHint};
pub(crate) use grounding::explanatory_python_code_request;
pub(crate) use grounding::explanatory_shell_command_request;
pub(crate) use grounding::extract_explicit_shell_command;
pub(crate) use grounding::extract_glob_pattern;
pub(crate) use grounding::extract_path_candidates;
pub(crate) use grounding::extract_skill_source_url;
pub(crate) use grounding::extract_web_query;
pub(crate) use grounding::file_name_from_path;
pub(crate) use grounding::goal_requests_directory_listing;
pub(crate) use grounding::goal_requests_file_read;
pub(crate) use grounding::goal_requests_filesystem_inspect;
pub(crate) use grounding::goal_requests_python_execution;
pub(crate) use grounding::goal_requests_web_lookup;
pub(crate) use grounding::grounded_python_code_allows_execution;
pub(crate) use grounding::grounded_shell_command_allows_execution;
pub(crate) use grounding::preferred_grounded_filesystem_tool;
pub(crate) use grounding::preferred_grounded_table_tool;
pub use loop_state::{LoopState, LoopStatus};
pub use next_step::{NextStepAction, NextStepDecision, NextStepDecisionSchemaError};
pub use observation::{StepObservation, ToolObservation};
pub use probe_check::{ToolProbeCheckReport, check_seed_tool_probe};
pub use recovery::should_resume_awaiting_user;
pub use regression::{
	InterpretedFlagExpectation, RegressionSuiteKind, RuntimeLoopRegressionCaseReport,
	RuntimeLoopRegressionExpectation, evaluate_runtime_loop_regression_case,
};
pub use request_intake::LoopRequest;
pub(crate) use request_intake::intake_request;
pub(crate) use route_classifier::classify_existing_route;
pub use state_update::{InterpretedObservation, interpret_observation};
pub use step_record::{StepAction, StepRecord};
pub use summarizer::FinalAnswerPayload;
pub(crate) use summarizer::summarize_observation;
pub(crate) use tool_loop::{
	attachments_for_tool, decide_tool_loop_next_step, ground_tool_arguments,
	next_working_directory_from_observation, tool_required_argument_keys,
};
pub use trace::{RuntimeLoopTraceCheckReport, check_runtime_loop_trace, runtime_loop_trace};
