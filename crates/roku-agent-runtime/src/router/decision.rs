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

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

/// Coarse intent hint emitted by the route classifier.
///
/// ## Why this exists
/// The route layer still needs a compact description of what kind of request the loop is about
/// to seed tool visibility and prompt shaping. `IntentFamily` is that hint surface.
///
/// ## Invariants
/// - This enum is a loop hint, not an execution plan.
/// - `MultiStep` and `Unknown` requests must still be handled by the generic runtime loop.
///
/// ## Non-Goals
/// - This enum does not bind the runtime to a fixed tool sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentFamily {
	Chat,
	FilesystemRead,
	TableRead,
	WebLookup,
	CodeExec,
	TextTransform,
	MultiStep,
	Unknown,
}

/// Coarse risk hint attached to a `RouteDecision`.
///
/// ## Why this exists
/// Route classification needs to preserve a lightweight risk signal for downstream policy checks
/// and logging without growing into a planner.
///
/// ## Non-Goals
/// - `RouteRisk` does not decide execution eligibility by itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteRisk {
	Low,
	Medium,
	High,
}

/// Route-layer hint object that seeds the generic ReAct loop.
///
/// ## Why this exists
/// `RouteDecision` captures the classifier's best-effort summary of the current request so the
/// runtime can initialize one loop with intent, candidate tool hints, and missing-input signals.
///
/// ## Fields
/// - `intent_family`: Coarse family hint for the current request.
/// - `confidence`: Confidence score for the route hint.
/// - `requires_multi_step`: Whether the classifier believes the request likely needs multiple
///   loop turns.
/// - `risk`: Coarse risk hint preserved for policy/logging.
/// - `candidate_tools`: Optional initial tool shortlist hints.
/// - `candidate_plugins`: Optional plugin hints associated with the route.
/// - `missing_arguments`: Missing-input hints that may justify `ask_user`.
/// - `reason`: Human-readable explanation of the route hint.
///
/// ## Invariants
/// - This struct only describes the current request state.
/// - `candidate_tools` is a hint list, not a fixed execution contract.
/// - `reason` documents the hint; it does not replace runtime observations.
///
/// ## Non-Goals
/// - `RouteDecision` is not a multi-step planner.
/// - `RouteDecision` does not own loop termination or completion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RouteDecision {
	pub intent_family: IntentFamily,
	pub confidence: f32,
	pub requires_multi_step: bool,
	pub risk: RouteRisk,
	pub candidate_tools: Vec<String>,
	pub candidate_plugins: Vec<String>,
	pub missing_arguments: Vec<String>,
	pub reason: String,
}

impl RouteDecision {
	pub fn new(
		intent_family: IntentFamily,
		confidence: f32,
		requires_multi_step: bool,
		risk: RouteRisk,
		candidate_tools: Vec<String>,
		candidate_plugins: Vec<String>,
		missing_arguments: Vec<String>,
		reason: impl Into<String>,
	) -> Self {
		Self {
			intent_family,
			confidence,
			requires_multi_step,
			risk,
			candidate_tools,
			candidate_plugins,
			missing_arguments,
			reason: reason.into(),
		}
	}

	pub fn confidence_score(&self) -> f32 {
		self.confidence
	}

	pub fn from_json_value(value: &Value) -> Result<Self, RouteDecisionSchemaError> {
		let object = value
			.as_object()
			.ok_or(RouteDecisionSchemaError::RootNotObject)?;
		let required_keys = [
			"intent_family",
			"confidence",
			"requires_multi_step",
			"risk",
			"candidate_tools",
			"candidate_plugins",
			"missing_arguments",
			"reason",
		];
		let missing = required_keys
			.iter()
			.filter(|key| !object.contains_key(**key))
			.map(|key| (*key).to_string())
			.collect::<Vec<_>>();
		if !missing.is_empty() {
			return Err(RouteDecisionSchemaError::MissingRequiredKeys { keys: missing });
		}

		Ok(Self {
			intent_family: parse_intent_family(object.get("intent_family").ok_or(
				RouteDecisionSchemaError::MissingRequiredKeys {
					keys: vec!["intent_family".to_string()],
				},
			)?)?,
			confidence: parse_confidence(object.get("confidence").ok_or(
				RouteDecisionSchemaError::MissingRequiredKeys {
					keys: vec!["confidence".to_string()],
				},
			)?)?,
			requires_multi_step: parse_bool(
				object.get("requires_multi_step").ok_or(
					RouteDecisionSchemaError::MissingRequiredKeys {
						keys: vec!["requires_multi_step".to_string()],
					},
				)?,
				"requires_multi_step",
			)?,
			risk: parse_route_risk(object.get("risk").ok_or(
				RouteDecisionSchemaError::MissingRequiredKeys {
					keys: vec!["risk".to_string()],
				},
			)?)?,
			candidate_tools: parse_string_vec(
				object.get("candidate_tools").ok_or(
					RouteDecisionSchemaError::MissingRequiredKeys {
						keys: vec!["candidate_tools".to_string()],
					},
				)?,
				"candidate_tools",
			)?,
			candidate_plugins: parse_string_vec(
				object.get("candidate_plugins").ok_or(
					RouteDecisionSchemaError::MissingRequiredKeys {
						keys: vec!["candidate_plugins".to_string()],
					},
				)?,
				"candidate_plugins",
			)?,
			missing_arguments: parse_string_vec(
				object.get("missing_arguments").ok_or(
					RouteDecisionSchemaError::MissingRequiredKeys {
						keys: vec!["missing_arguments".to_string()],
					},
				)?,
				"missing_arguments",
			)?,
			reason: parse_string(
				object
					.get("reason")
					.ok_or(RouteDecisionSchemaError::MissingRequiredKeys {
						keys: vec!["reason".to_string()],
					})?,
				"reason",
			)?,
		})
	}
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum RouteDecisionSchemaError {
	#[error("route decision root must be a json object")]
	RootNotObject,
	#[error("route decision is missing required keys: {keys:?}")]
	MissingRequiredKeys { keys: Vec<String> },
	#[error("field `{field}` has invalid type; expected {expected}")]
	InvalidType {
		field: &'static str,
		expected: &'static str,
	},
	#[error("field `{field}` contains unknown enum value `{value}`")]
	UnknownEnumValue { field: &'static str, value: String },
}

fn parse_intent_family(value: &Value) -> Result<IntentFamily, RouteDecisionSchemaError> {
	let raw = parse_string(value, "intent_family")?;
	match raw.as_str() {
		"chat" => Ok(IntentFamily::Chat),
		"filesystem_read" => Ok(IntentFamily::FilesystemRead),
		"table_read" => Ok(IntentFamily::TableRead),
		"web_lookup" => Ok(IntentFamily::WebLookup),
		"code_exec" => Ok(IntentFamily::CodeExec),
		"text_transform" => Ok(IntentFamily::TextTransform),
		"multi_step" => Ok(IntentFamily::MultiStep),
		"unknown" => Ok(IntentFamily::Unknown),
		_ => Err(RouteDecisionSchemaError::UnknownEnumValue {
			field: "intent_family",
			value: raw,
		}),
	}
}

fn parse_route_risk(value: &Value) -> Result<RouteRisk, RouteDecisionSchemaError> {
	let raw = parse_string(value, "risk")?;
	match raw.as_str() {
		"low" => Ok(RouteRisk::Low),
		"medium" => Ok(RouteRisk::Medium),
		"high" => Ok(RouteRisk::High),
		_ => Err(RouteDecisionSchemaError::UnknownEnumValue {
			field: "risk",
			value: raw,
		}),
	}
}

fn parse_confidence(value: &Value) -> Result<f32, RouteDecisionSchemaError> {
	match value {
		Value::String(raw) => {
			raw.parse::<f32>()
				.map_err(|_| RouteDecisionSchemaError::InvalidType {
					field: "confidence",
					expected: "parseable number",
				})
		}
		Value::Number(number) => {
			number
				.as_f64()
				.map(|value| value as f32)
				.ok_or(RouteDecisionSchemaError::InvalidType {
					field: "confidence",
					expected: "number",
				})
		}
		_ => Err(RouteDecisionSchemaError::InvalidType {
			field: "confidence",
			expected: "string or number",
		}),
	}
}

fn parse_bool(value: &Value, field: &'static str) -> Result<bool, RouteDecisionSchemaError> {
	value
		.as_bool()
		.ok_or(RouteDecisionSchemaError::InvalidType {
			field,
			expected: "boolean",
		})
}

fn parse_string_vec(
	value: &Value,
	field: &'static str,
) -> Result<Vec<String>, RouteDecisionSchemaError> {
	let array = value
		.as_array()
		.ok_or(RouteDecisionSchemaError::InvalidType {
			field,
			expected: "array<string>",
		})?;
	array.iter().map(|item| parse_string(item, field)).collect()
}

fn parse_string(value: &Value, field: &'static str) -> Result<String, RouteDecisionSchemaError> {
	value
		.as_str()
		.map(str::to_string)
		.ok_or(RouteDecisionSchemaError::InvalidType {
			field,
			expected: "string",
		})
}
