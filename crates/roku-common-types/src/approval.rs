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

//! Shared approval contracts.

use serde::{Deserialize, Serialize};

use crate::{
	ApprovalId, CanonicalDigest, CanonicalExecution, NodeId, PolicyDecision, RequestId, TaskId,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApprovalStatus {
	Pending,
	Approved,
	Rejected,
	Cancelled,
}

pub type ApprovalTicketStatus = ApprovalStatus;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalDecision {
	pub actor: String,
	pub approved: bool,
	pub comment: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovedExecutionRef {
	pub digest: CanonicalDigest,
	#[serde(default)]
	pub frozen_payload_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingExecutionApproval {
	pub approval_id: ApprovalId,
	pub digest: CanonicalDigest,
	pub canonical_execution: CanonicalExecution,
	pub policy_decision: PolicyDecision,
	#[serde(default)]
	pub execution_ref: Option<ApprovedExecutionRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalTicket {
	pub approval_id: ApprovalId,
	pub task_id: TaskId,
	pub request_id: RequestId,
	pub node_id: NodeId,
	pub summary: String,
	pub status: ApprovalStatus,
	pub decided_by: Option<String>,
	pub comment: Option<String>,
	#[serde(default)]
	pub pending_execution: Option<PendingExecutionApproval>,
}
