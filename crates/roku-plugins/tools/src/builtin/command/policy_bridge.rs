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

use std::path::Path;

use roku_common_types::{
	ApprovalRequirement, ApprovalRequirementScope, CanonicalExecution, ExecutionActionClass,
	InvocationMode, PolicyDecision, PolicyOutcome, PolicyReasonCode,
};

use super::prepare::validate_allowed_command;

pub(super) fn evaluate_command_policy(execution: &CanonicalExecution) -> PolicyDecision {
	if execution.invocation_mode != InvocationMode::DirectExec || execution.shell_context.is_some()
	{
		return deny(PolicyReasonCode::DeniedByShellSyntax);
	}

	if !path_in_any_root(
		&execution.cwd,
		&execution.resource_scope.effective_read_roots,
	) {
		return require_approval(PolicyReasonCode::ApprovalRequiredByOutOfScopePath);
	}

	if execution
		.resource_scope
		.resolved_targets
		.iter()
		.any(|target| {
			!path_in_any_root(target, &execution.resource_scope.effective_read_roots)
				&& !path_in_any_root(target, &execution.resource_scope.effective_write_roots)
		}) {
		return require_approval(PolicyReasonCode::ApprovalRequiredByOutOfScopePath);
	}

	match execution.action_class {
		ExecutionActionClass::Write | ExecutionActionClass::Mixed => {
			return require_approval(PolicyReasonCode::ApprovalRequiredByWriteScope);
		}
		ExecutionActionClass::Network => {
			return require_approval(PolicyReasonCode::ApprovalRequiredByNetwork);
		}
		ExecutionActionClass::Read | ExecutionActionClass::Exec => {}
	}

	let arguments = execution.argv.get(1..).unwrap_or(&[]);
	if validate_allowed_command(&execution.program, arguments).is_err() {
		return require_approval(PolicyReasonCode::ApprovalRequiredByUntrustedProgram);
	}

	allow()
}

fn allow() -> PolicyDecision {
	PolicyDecision {
		outcome: PolicyOutcome::Allow,
		reason_code: PolicyReasonCode::AllowedByPolicy,
		approval_requirement: None,
	}
}

fn deny(reason_code: PolicyReasonCode) -> PolicyDecision {
	PolicyDecision {
		outcome: PolicyOutcome::Deny,
		reason_code,
		approval_requirement: None,
	}
}

fn require_approval(reason_code: PolicyReasonCode) -> PolicyDecision {
	PolicyDecision {
		outcome: PolicyOutcome::RequireApproval,
		reason_code,
		approval_requirement: Some(ApprovalRequirement {
			scope: ApprovalRequirementScope::Invocation,
			reason_code,
		}),
	}
}

fn path_in_any_root(path: &str, roots: &[String]) -> bool {
	if roots.is_empty() {
		return true;
	}

	roots
		.iter()
		.any(|root| Path::new(path).starts_with(Path::new(root)))
}
