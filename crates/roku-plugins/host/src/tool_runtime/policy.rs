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

use roku_common_types::{CanonicalExecution, PolicyDecision, PolicyOutcome, PolicyReasonCode};

pub(crate) fn evaluate_execution_policy(
	tool_name: &str,
	execution: &CanonicalExecution,
	bridge_decision: Option<PolicyDecision>,
) -> PolicyDecision {
	if execution.tool_name != tool_name {
		return deny(PolicyReasonCode::DeniedByUncanonicalizableInput);
	}

	bridge_decision.unwrap_or_else(allow)
}

pub(crate) fn allow() -> PolicyDecision {
	PolicyDecision {
		outcome: PolicyOutcome::Allow,
		reason_code: PolicyReasonCode::AllowedByPolicy,
		approval_requirement: None,
	}
}

pub(crate) fn deny(reason_code: PolicyReasonCode) -> PolicyDecision {
	PolicyDecision {
		outcome: PolicyOutcome::Deny,
		reason_code,
		approval_requirement: None,
	}
}

pub(crate) fn decision_message(decision: &PolicyDecision) -> String {
	format!(
		"policy_outcome={} reason_code={}",
		policy_outcome_label(decision.outcome),
		policy_reason_label(decision.reason_code)
	)
}

fn policy_outcome_label(outcome: PolicyOutcome) -> &'static str {
	match outcome {
		PolicyOutcome::Allow => "allow",
		PolicyOutcome::Deny => "deny",
		PolicyOutcome::RequireApproval => "require_approval",
	}
}

fn policy_reason_label(reason_code: PolicyReasonCode) -> &'static str {
	match reason_code {
		PolicyReasonCode::AllowedByPolicy => "allowed_by_policy",
		PolicyReasonCode::DeniedByShellSyntax => "denied_by_shell_syntax",
		PolicyReasonCode::DeniedByCommandPolicy => "denied_by_command_policy",
		PolicyReasonCode::DeniedByOutOfScopeCwd => "denied_by_out_of_scope_cwd",
		PolicyReasonCode::DeniedByOutOfScopeTarget => "denied_by_out_of_scope_target",
		PolicyReasonCode::DeniedByUncanonicalizableInput => "denied_by_uncanonicalizable_input",
		PolicyReasonCode::ApprovalRequiredByWriteScope => "approval_required_by_write_scope",
		PolicyReasonCode::ApprovalRequiredByNetwork => "approval_required_by_network",
		PolicyReasonCode::ApprovalRequiredByUntrustedProgram => {
			"approval_required_by_untrusted_program"
		}
	}
}
