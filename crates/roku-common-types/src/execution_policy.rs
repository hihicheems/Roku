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

//! Shared execution policy contracts.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyOutcome {
	#[default]
	Allow,
	Deny,
	RequireApproval,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyReasonCode {
	AllowedByPolicy,
	DeniedByShellSyntax,
	DeniedByCommandPolicy,
	DeniedByOutOfScopeCwd,
	DeniedByOutOfScopeTarget,
	DeniedByUncanonicalizableInput,
	ApprovalRequiredByWriteScope,
	ApprovalRequiredByNetwork,
	ApprovalRequiredByUntrustedProgram,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalRequirementScope {
	#[default]
	Invocation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRequirement {
	pub scope: ApprovalRequirementScope,
	pub reason_code: PolicyReasonCode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyDecision {
	pub outcome: PolicyOutcome,
	pub reason_code: PolicyReasonCode,
	#[serde(default)]
	pub approval_requirement: Option<ApprovalRequirement>,
}
