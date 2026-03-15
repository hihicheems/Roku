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

use roku_common_types::{
	ToolContract, ToolInputContract, ToolInputFieldContract, ToolIsolationProfile,
	ToolOutputContract, ToolRetryPolicy, ToolRuntimeContract, ToolSelectionContract,
	ToolSideEffectPolicy,
};
use roku_plugin_host::{RuntimeConstraints, SandboxProfile, ToolSchema};

pub(crate) fn selection_contract(
	use_when: &[&str],
	avoid_when: &[&str],
	common_confusions: &[&str],
) -> ToolSelectionContract {
	ToolSelectionContract {
		use_when: use_when.iter().map(|value| (*value).to_string()).collect(),
		avoid_when: avoid_when
			.iter()
			.map(|value| (*value).to_string())
			.collect(),
		common_confusions: common_confusions
			.iter()
			.map(|value| (*value).to_string())
			.collect(),
	}
}

pub(crate) fn input_field(
	name: &str,
	required: bool,
	semantics: &str,
	invalid_when: &[&str],
) -> ToolInputFieldContract {
	ToolInputFieldContract {
		name: name.to_string(),
		required,
		semantics: semantics.to_string(),
		invalid_when: invalid_when
			.iter()
			.map(|value| (*value).to_string())
			.collect(),
	}
}

pub(crate) fn input_contract(
	fields: Vec<ToolInputFieldContract>,
	preconditions: &[&str],
) -> ToolInputContract {
	ToolInputContract {
		fields,
		preconditions: preconditions
			.iter()
			.map(|value| (*value).to_string())
			.collect(),
	}
}

pub(crate) fn output_contract(
	success_semantics: &str,
	empty_result_semantics: &str,
	error_semantics: &[&str],
	non_terminal_success: bool,
	terminal_success: bool,
) -> ToolOutputContract {
	ToolOutputContract {
		observation_schema: "tool_observation.v1".to_string(),
		success_semantics: success_semantics.to_string(),
		empty_result_semantics: empty_result_semantics.to_string(),
		error_semantics: error_semantics
			.iter()
			.map(|value| (*value).to_string())
			.collect(),
		non_terminal_success,
		terminal_success,
	}
}

pub(crate) fn runtime_contract(
	constraints: &RuntimeConstraints,
	side_effects: ToolSideEffectPolicy,
	retry_policy: ToolRetryPolicy,
) -> ToolRuntimeContract {
	ToolRuntimeContract {
		side_effects,
		retry_policy,
		timeout_ms: constraints.timeout_ms,
		isolation_profile: isolation_profile(constraints.sandbox_profile.clone()),
	}
}

pub(crate) fn contract_tool_schema(
	contract: Option<&ToolContract>,
	leading_required_fields: &[&str],
) -> ToolSchema {
	let mut required_fields = leading_required_fields
		.iter()
		.map(|value| (*value).to_string())
		.collect::<Vec<_>>();
	if let Some(contract) = contract {
		for field in contract.input.required_field_names() {
			if !required_fields.iter().any(|existing| existing == &field) {
				required_fields.push(field);
			}
		}
	}
	ToolSchema { required_fields }
}

pub(crate) fn contract_input_schema(
	contract: Option<&ToolContract>,
	fallback: &[String],
) -> Vec<String> {
	contract
		.map(|contract| contract.input.field_names())
		.filter(|fields| !fields.is_empty())
		.unwrap_or_else(|| fallback.to_vec())
}

fn isolation_profile(profile: SandboxProfile) -> ToolIsolationProfile {
	match profile {
		SandboxProfile::NoIsolation => ToolIsolationProfile::NoIsolation,
		SandboxProfile::ReadOnlyFs => ToolIsolationProfile::ReadOnlyFs,
		SandboxProfile::PythonResearch => ToolIsolationProfile::PythonResearch,
		SandboxProfile::ContainerRestricted => ToolIsolationProfile::ContainerRestricted,
	}
}
