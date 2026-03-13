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

use crate::router::{DirectRoutePlan, RouteDecision};

/// Terminal escalation actions that still bypass direct loop execution.
///
/// ## Why this exists
/// Some requests cannot immediately continue through the generic loop because they are missing
/// mandatory user input or the runtime has hit a hard block. `EscalationAction` encodes those
/// exceptional cases.
///
/// ## Invariants
/// - Escalations are exceptional paths, not the default handling mode for new requests.
/// - `EnterLimitedPlanning` is reserved for deprecated compatibility paths such as explicit
///   planning-mode hints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscalationAction {
	AskForMoreInfo,
	FallbackAnswer,
	EnterLimitedPlanning,
}

/// Reasons attached to a terminal route escalation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscalationReason {
	MissingArguments,
	RequiresMultiStep,
	NoEnabledRouteTarget,
	RouteModelUnavailable,
	RouteClassifierFailure,
	RouteParseGuardFailure,
	LowConfidence,
}

/// Route-layer terminal plan used only when the request cannot continue into the generic loop.
#[derive(Debug, Clone, PartialEq)]
pub struct RouteEscalationPlan {
	pub decision: RouteDecision,
	pub reason: EscalationReason,
	pub action: EscalationAction,
}

/// Final route outcome produced by the classifier.
///
/// `Direct` means "initialize one runtime loop with this hint." `Escalate` means the runtime hit
/// an explicit hard stop or compatibility-only path.
#[derive(Debug, Clone, PartialEq)]
pub enum RouteDecisionResult {
	Direct(DirectRoutePlan),
	Escalate(RouteEscalationPlan),
}
