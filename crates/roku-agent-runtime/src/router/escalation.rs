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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscalationAction {
	AskForMoreInfo,
	FallbackAnswer,
	EnterLimitedPlanning,
}

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

#[derive(Debug, Clone, PartialEq)]
pub struct RouteEscalationPlan {
	pub decision: RouteDecision,
	pub reason: EscalationReason,
	pub action: EscalationAction,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RouteDecisionResult {
	Direct(DirectRoutePlan),
	Escalate(RouteEscalationPlan),
}
