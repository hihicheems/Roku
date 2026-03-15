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

use std::env;

use serde::Deserialize;
use thiserror::Error;

/// Effective runtime tunables owned by `roku-agent-runtime`.
///
/// Values in this struct are already merged from typed defaults, TOML patches,
/// and environment overrides. Call [`Self::validate_and_clamp`] before using
/// externally sourced values so hard limits remain the final guardrail.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AgentRuntimeConfig {
	/// Effective values for `[runtime.agent.loop]`.
	///
	/// The raw identifier keeps the Rust field name aligned with the TOML
	/// namespace so config names remain easy to trace during maintenance.
	pub r#loop: LoopRuntimeConfig,
	/// Effective values for `[runtime.agent.router]`.
	pub router: RouteClassifierRuntimeConfig,
	/// Effective values for `[runtime.agent.prompts]`.
	pub prompts: PromptCompactionRuntimeConfig,
	/// Effective values for `[runtime.agent.next_step]`.
	pub next_step: NextStepRuntimeConfig,
}

/// Partial runtime overrides for [`AgentRuntimeConfig`].
///
/// This mirrors the `[runtime.agent.*]` section in `config/runtime.toml`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentRuntimeConfigPatch {
	#[serde(default)]
	pub r#loop: Option<LoopRuntimeConfigPatch>,
	#[serde(default)]
	pub router: Option<RouteClassifierRuntimeConfigPatch>,
	#[serde(default)]
	pub prompts: Option<PromptCompactionRuntimeConfigPatch>,
	#[serde(default)]
	pub next_step: Option<NextStepRuntimeConfigPatch>,
}

/// Effective loop lifecycle budgets for a single ReAct run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopRuntimeConfig {
	/// Initial maximum number of loop steps allowed before the runtime aborts.
	pub initial_step_budget: u32,
	/// Initial maximum number of recovery turns allowed after tool failures.
	pub initial_recovery_budget: u32,
}

/// Partial overrides for [`LoopRuntimeConfig`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoopRuntimeConfigPatch {
	pub initial_step_budget: Option<u32>,
	pub initial_recovery_budget: Option<u32>,
}

/// Effective route-classifier generation budgets and inventory compaction knobs.
#[derive(Debug, Clone, PartialEq)]
pub struct RouteClassifierRuntimeConfig {
	/// Target size of the route-classifier JSON response.
	pub expected_output_tokens: u64,
	/// Token budget reserved for one route-classifier model call.
	pub budget_tokens_remaining: u64,
	/// Cost budget reserved for one route-classifier model call.
	pub budget_cost_remaining_usd: f64,
	/// Maximum number of candidate tool entries projected into one route prompt.
	pub candidate_inventory_limit: usize,
}

/// Partial overrides for [`RouteClassifierRuntimeConfig`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteClassifierRuntimeConfigPatch {
	pub expected_output_tokens: Option<u64>,
	pub budget_tokens_remaining: Option<u64>,
	pub budget_cost_remaining_usd: Option<f64>,
	pub candidate_inventory_limit: Option<usize>,
}

/// Effective prompt-compaction limits for model-facing tool hints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptCompactionRuntimeConfig {
	/// Maximum characters kept from one visible-tool hint in `ContextProjection`.
	pub visible_tool_hint_max_chars: usize,
	/// Maximum characters kept from one route-candidate selection hint.
	pub candidate_description_max_chars: usize,
}

/// Partial overrides for [`PromptCompactionRuntimeConfig`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromptCompactionRuntimeConfigPatch {
	pub visible_tool_hint_max_chars: Option<usize>,
	pub candidate_description_max_chars: Option<usize>,
}

/// Effective next-step model generation budgets for the generic tool loop.
#[derive(Debug, Clone, PartialEq)]
pub struct NextStepRuntimeConfig {
	/// Target size of one generic loop next-step JSON response.
	pub expected_output_tokens: u64,
	/// Token budget reserved for one generic loop next-step model call.
	pub budget_tokens_remaining: u64,
	/// Cost budget reserved for one generic loop next-step model call.
	pub budget_cost_remaining_usd: f64,
}

/// Partial overrides for [`NextStepRuntimeConfig`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NextStepRuntimeConfigPatch {
	pub expected_output_tokens: Option<u64>,
	pub budget_tokens_remaining: Option<u64>,
	pub budget_cost_remaining_usd: Option<f64>,
}

#[derive(Debug, Error, Clone, PartialEq)]
pub enum AgentRuntimeConfigError {
	#[error("runtime.agent.loop.initial_step_budget must be greater than zero")]
	InvalidInitialStepBudget,
	#[error("runtime.agent.loop.initial_recovery_budget must be greater than zero")]
	InvalidInitialRecoveryBudget,
	#[error("runtime.agent.router.expected_output_tokens must be greater than zero")]
	InvalidRouteExpectedOutputTokens,
	#[error("runtime.agent.router.budget_tokens_remaining must be greater than zero")]
	InvalidRouteBudgetTokensRemaining,
	#[error("runtime.agent.router.budget_cost_remaining_usd must be greater than zero")]
	InvalidRouteBudgetCostRemainingUsd,
	#[error("runtime.agent.router.candidate_inventory_limit must be greater than zero")]
	InvalidCandidateInventoryLimit,
	#[error("runtime.agent.prompts.visible_tool_hint_max_chars must be greater than zero")]
	InvalidVisibleToolHintMaxChars,
	#[error("runtime.agent.prompts.candidate_description_max_chars must be greater than zero")]
	InvalidCandidateDescriptionMaxChars,
	#[error("runtime.agent.next_step.expected_output_tokens must be greater than zero")]
	InvalidNextStepExpectedOutputTokens,
	#[error("runtime.agent.next_step.budget_tokens_remaining must be greater than zero")]
	InvalidNextStepBudgetTokensRemaining,
	#[error("runtime.agent.next_step.budget_cost_remaining_usd must be greater than zero")]
	InvalidNextStepBudgetCostRemainingUsd,
}

/// Final ceiling for `runtime.agent.loop.initial_step_budget`.
///
/// This is a safety guardrail, not the recommended operating value.
pub const HARD_MAX_INITIAL_STEP_BUDGET: u32 = 64;
/// Final ceiling for `runtime.agent.loop.initial_recovery_budget`.
///
/// This is a safety guardrail, not the recommended operating value.
pub const HARD_MAX_INITIAL_RECOVERY_BUDGET: u32 = 32;
/// Final ceiling for `runtime.agent.router.expected_output_tokens`.
pub const HARD_MAX_ROUTE_EXPECTED_OUTPUT_TOKENS: u64 = 2_048;
/// Final ceiling for `runtime.agent.router.budget_tokens_remaining`.
pub const HARD_MAX_ROUTE_BUDGET_TOKENS_REMAINING: u64 = 32_000;
/// Final ceiling for `runtime.agent.router.budget_cost_remaining_usd`.
pub const HARD_MAX_ROUTE_BUDGET_COST_REMAINING_USD: f64 = 10.0;
/// Final ceiling for `runtime.agent.router.candidate_inventory_limit`.
pub const HARD_MAX_CANDIDATE_INVENTORY_LIMIT: usize = 64;
/// Final ceiling for `runtime.agent.prompts.visible_tool_hint_max_chars`.
pub const HARD_MAX_VISIBLE_TOOL_HINT_MAX_CHARS: usize = 1_024;
/// Final ceiling for `runtime.agent.prompts.candidate_description_max_chars`.
pub const HARD_MAX_CANDIDATE_DESCRIPTION_MAX_CHARS: usize = 1_024;
/// Final ceiling for `runtime.agent.next_step.expected_output_tokens`.
pub const HARD_MAX_NEXT_STEP_EXPECTED_OUTPUT_TOKENS: u64 = 2_048;
/// Final ceiling for `runtime.agent.next_step.budget_tokens_remaining`.
pub const HARD_MAX_NEXT_STEP_BUDGET_TOKENS_REMAINING: u64 = 32_000;
/// Final ceiling for `runtime.agent.next_step.budget_cost_remaining_usd`.
pub const HARD_MAX_NEXT_STEP_BUDGET_COST_REMAINING_USD: f64 = 10.0;

impl Default for LoopRuntimeConfig {
	fn default() -> Self {
		Self {
			initial_step_budget: 10,
			initial_recovery_budget: 2,
		}
	}
}

impl Default for RouteClassifierRuntimeConfig {
	fn default() -> Self {
		Self {
			expected_output_tokens: 220,
			budget_tokens_remaining: 10_000,
			budget_cost_remaining_usd: 0.1,
			candidate_inventory_limit: 10,
		}
	}
}

impl Default for PromptCompactionRuntimeConfig {
	fn default() -> Self {
		Self {
			visible_tool_hint_max_chars: 180,
			candidate_description_max_chars: 180,
		}
	}
}

impl Default for NextStepRuntimeConfig {
	fn default() -> Self {
		Self {
			expected_output_tokens: 1_200,
			budget_tokens_remaining: 10_000,
			budget_cost_remaining_usd: 0.05,
		}
	}
}

impl AgentRuntimeConfig {
	pub fn apply_patch(&mut self, patch: AgentRuntimeConfigPatch) {
		if let Some(loop_config) = patch.r#loop {
			self.r#loop.apply_patch(loop_config);
		}
		if let Some(router) = patch.router {
			self.router.apply_patch(router);
		}
		if let Some(prompts) = patch.prompts {
			self.prompts.apply_patch(prompts);
		}
		if let Some(next_step) = patch.next_step {
			self.next_step.apply_patch(next_step);
		}
	}

	pub fn apply_env_overrides(&mut self) -> Result<(), AgentRuntimeConfigError> {
		self.r#loop.apply_env_overrides()?;
		self.router.apply_env_overrides()?;
		self.prompts.apply_env_overrides()?;
		self.next_step.apply_env_overrides()?;
		Ok(())
	}

	pub fn validate_and_clamp(&mut self) -> Result<(), AgentRuntimeConfigError> {
		self.r#loop.validate_and_clamp()?;
		self.router.validate_and_clamp()?;
		self.prompts.validate_and_clamp()?;
		self.next_step.validate_and_clamp()?;
		Ok(())
	}
}

impl LoopRuntimeConfig {
	pub fn apply_patch(&mut self, patch: LoopRuntimeConfigPatch) {
		if let Some(value) = patch.initial_step_budget {
			self.initial_step_budget = value;
		}
		if let Some(value) = patch.initial_recovery_budget {
			self.initial_recovery_budget = value;
		}
	}

	pub fn apply_env_overrides(&mut self) -> Result<(), AgentRuntimeConfigError> {
		if let Some(value) = env_override_u32("ROKU_RUNTIME__AGENT__LOOP__INITIAL_STEP_BUDGET") {
			self.initial_step_budget = value?;
		}
		if let Some(value) = env_override_u32("ROKU_RUNTIME__AGENT__LOOP__INITIAL_RECOVERY_BUDGET")
		{
			self.initial_recovery_budget = value?;
		}
		Ok(())
	}

	pub fn validate_and_clamp(&mut self) -> Result<(), AgentRuntimeConfigError> {
		if self.initial_step_budget == 0 {
			return Err(AgentRuntimeConfigError::InvalidInitialStepBudget);
		}
		if self.initial_recovery_budget == 0 {
			return Err(AgentRuntimeConfigError::InvalidInitialRecoveryBudget);
		}
		self.initial_step_budget = self.initial_step_budget.min(HARD_MAX_INITIAL_STEP_BUDGET);
		self.initial_recovery_budget = self
			.initial_recovery_budget
			.min(HARD_MAX_INITIAL_RECOVERY_BUDGET);
		Ok(())
	}
}

impl RouteClassifierRuntimeConfig {
	pub fn apply_patch(&mut self, patch: RouteClassifierRuntimeConfigPatch) {
		if let Some(value) = patch.expected_output_tokens {
			self.expected_output_tokens = value;
		}
		if let Some(value) = patch.budget_tokens_remaining {
			self.budget_tokens_remaining = value;
		}
		if let Some(value) = patch.budget_cost_remaining_usd {
			self.budget_cost_remaining_usd = value;
		}
		if let Some(value) = patch.candidate_inventory_limit {
			self.candidate_inventory_limit = value;
		}
	}

	pub fn apply_env_overrides(&mut self) -> Result<(), AgentRuntimeConfigError> {
		if let Some(value) = env_override_u64("ROKU_RUNTIME__AGENT__ROUTER__EXPECTED_OUTPUT_TOKENS")
		{
			self.expected_output_tokens = value?;
		}
		if let Some(value) =
			env_override_u64("ROKU_RUNTIME__AGENT__ROUTER__BUDGET_TOKENS_REMAINING")
		{
			self.budget_tokens_remaining = value?;
		}
		if let Some(value) =
			env_override_f64("ROKU_RUNTIME__AGENT__ROUTER__BUDGET_COST_REMAINING_USD")
		{
			self.budget_cost_remaining_usd = value?;
		}
		if let Some(value) =
			env_override_usize("ROKU_RUNTIME__AGENT__ROUTER__CANDIDATE_INVENTORY_LIMIT")
		{
			self.candidate_inventory_limit = value?;
		}
		Ok(())
	}

	pub fn validate_and_clamp(&mut self) -> Result<(), AgentRuntimeConfigError> {
		if self.expected_output_tokens == 0 {
			return Err(AgentRuntimeConfigError::InvalidRouteExpectedOutputTokens);
		}
		if self.budget_tokens_remaining == 0 {
			return Err(AgentRuntimeConfigError::InvalidRouteBudgetTokensRemaining);
		}
		if self.budget_cost_remaining_usd <= 0.0 {
			return Err(AgentRuntimeConfigError::InvalidRouteBudgetCostRemainingUsd);
		}
		if self.candidate_inventory_limit == 0 {
			return Err(AgentRuntimeConfigError::InvalidCandidateInventoryLimit);
		}
		self.expected_output_tokens = self
			.expected_output_tokens
			.min(HARD_MAX_ROUTE_EXPECTED_OUTPUT_TOKENS);
		self.budget_tokens_remaining = self
			.budget_tokens_remaining
			.min(HARD_MAX_ROUTE_BUDGET_TOKENS_REMAINING);
		self.budget_cost_remaining_usd = self
			.budget_cost_remaining_usd
			.min(HARD_MAX_ROUTE_BUDGET_COST_REMAINING_USD);
		self.candidate_inventory_limit = self
			.candidate_inventory_limit
			.min(HARD_MAX_CANDIDATE_INVENTORY_LIMIT);
		Ok(())
	}
}

impl PromptCompactionRuntimeConfig {
	pub fn apply_patch(&mut self, patch: PromptCompactionRuntimeConfigPatch) {
		if let Some(value) = patch.visible_tool_hint_max_chars {
			self.visible_tool_hint_max_chars = value;
		}
		if let Some(value) = patch.candidate_description_max_chars {
			self.candidate_description_max_chars = value;
		}
	}

	pub fn apply_env_overrides(&mut self) -> Result<(), AgentRuntimeConfigError> {
		if let Some(value) =
			env_override_usize("ROKU_RUNTIME__AGENT__PROMPTS__VISIBLE_TOOL_HINT_MAX_CHARS")
		{
			self.visible_tool_hint_max_chars = value?;
		}
		if let Some(value) =
			env_override_usize("ROKU_RUNTIME__AGENT__PROMPTS__CANDIDATE_DESCRIPTION_MAX_CHARS")
		{
			self.candidate_description_max_chars = value?;
		}
		Ok(())
	}

	pub fn validate_and_clamp(&mut self) -> Result<(), AgentRuntimeConfigError> {
		if self.visible_tool_hint_max_chars == 0 {
			return Err(AgentRuntimeConfigError::InvalidVisibleToolHintMaxChars);
		}
		if self.candidate_description_max_chars == 0 {
			return Err(AgentRuntimeConfigError::InvalidCandidateDescriptionMaxChars);
		}
		self.visible_tool_hint_max_chars = self
			.visible_tool_hint_max_chars
			.min(HARD_MAX_VISIBLE_TOOL_HINT_MAX_CHARS);
		self.candidate_description_max_chars = self
			.candidate_description_max_chars
			.min(HARD_MAX_CANDIDATE_DESCRIPTION_MAX_CHARS);
		Ok(())
	}
}

impl NextStepRuntimeConfig {
	pub fn apply_patch(&mut self, patch: NextStepRuntimeConfigPatch) {
		if let Some(value) = patch.expected_output_tokens {
			self.expected_output_tokens = value;
		}
		if let Some(value) = patch.budget_tokens_remaining {
			self.budget_tokens_remaining = value;
		}
		if let Some(value) = patch.budget_cost_remaining_usd {
			self.budget_cost_remaining_usd = value;
		}
	}

	pub fn apply_env_overrides(&mut self) -> Result<(), AgentRuntimeConfigError> {
		if let Some(value) =
			env_override_u64("ROKU_RUNTIME__AGENT__NEXT_STEP__EXPECTED_OUTPUT_TOKENS")
		{
			self.expected_output_tokens = value?;
		}
		if let Some(value) =
			env_override_u64("ROKU_RUNTIME__AGENT__NEXT_STEP__BUDGET_TOKENS_REMAINING")
		{
			self.budget_tokens_remaining = value?;
		}
		if let Some(value) =
			env_override_f64("ROKU_RUNTIME__AGENT__NEXT_STEP__BUDGET_COST_REMAINING_USD")
		{
			self.budget_cost_remaining_usd = value?;
		}
		Ok(())
	}

	pub fn validate_and_clamp(&mut self) -> Result<(), AgentRuntimeConfigError> {
		if self.expected_output_tokens == 0 {
			return Err(AgentRuntimeConfigError::InvalidNextStepExpectedOutputTokens);
		}
		if self.budget_tokens_remaining == 0 {
			return Err(AgentRuntimeConfigError::InvalidNextStepBudgetTokensRemaining);
		}
		if self.budget_cost_remaining_usd <= 0.0 {
			return Err(AgentRuntimeConfigError::InvalidNextStepBudgetCostRemainingUsd);
		}
		self.expected_output_tokens = self
			.expected_output_tokens
			.min(HARD_MAX_NEXT_STEP_EXPECTED_OUTPUT_TOKENS);
		self.budget_tokens_remaining = self
			.budget_tokens_remaining
			.min(HARD_MAX_NEXT_STEP_BUDGET_TOKENS_REMAINING);
		self.budget_cost_remaining_usd = self
			.budget_cost_remaining_usd
			.min(HARD_MAX_NEXT_STEP_BUDGET_COST_REMAINING_USD);
		Ok(())
	}
}

fn env_override_string(key: &str) -> Option<String> {
	env::var(key)
		.ok()
		.map(|value| value.trim().to_string())
		.filter(|value| !value.is_empty())
}

fn env_override_u32(key: &'static str) -> Option<Result<u32, AgentRuntimeConfigError>> {
	env_override_string(key).map(|value| value.parse::<u32>().map_err(|_| invalid_env_key(key)))
}

fn env_override_u64(key: &'static str) -> Option<Result<u64, AgentRuntimeConfigError>> {
	env_override_string(key).map(|value| value.parse::<u64>().map_err(|_| invalid_env_key(key)))
}

fn env_override_usize(key: &'static str) -> Option<Result<usize, AgentRuntimeConfigError>> {
	env_override_string(key).map(|value| value.parse::<usize>().map_err(|_| invalid_env_key(key)))
}

fn env_override_f64(key: &'static str) -> Option<Result<f64, AgentRuntimeConfigError>> {
	env_override_string(key).map(|value| value.parse::<f64>().map_err(|_| invalid_env_key(key)))
}

fn invalid_env_key(key: &'static str) -> AgentRuntimeConfigError {
	match key {
		"ROKU_RUNTIME__AGENT__LOOP__INITIAL_STEP_BUDGET" => {
			AgentRuntimeConfigError::InvalidInitialStepBudget
		}
		"ROKU_RUNTIME__AGENT__LOOP__INITIAL_RECOVERY_BUDGET" => {
			AgentRuntimeConfigError::InvalidInitialRecoveryBudget
		}
		"ROKU_RUNTIME__AGENT__ROUTER__EXPECTED_OUTPUT_TOKENS" => {
			AgentRuntimeConfigError::InvalidRouteExpectedOutputTokens
		}
		"ROKU_RUNTIME__AGENT__ROUTER__BUDGET_TOKENS_REMAINING" => {
			AgentRuntimeConfigError::InvalidRouteBudgetTokensRemaining
		}
		"ROKU_RUNTIME__AGENT__ROUTER__BUDGET_COST_REMAINING_USD" => {
			AgentRuntimeConfigError::InvalidRouteBudgetCostRemainingUsd
		}
		"ROKU_RUNTIME__AGENT__ROUTER__CANDIDATE_INVENTORY_LIMIT" => {
			AgentRuntimeConfigError::InvalidCandidateInventoryLimit
		}
		"ROKU_RUNTIME__AGENT__PROMPTS__VISIBLE_TOOL_HINT_MAX_CHARS" => {
			AgentRuntimeConfigError::InvalidVisibleToolHintMaxChars
		}
		"ROKU_RUNTIME__AGENT__PROMPTS__CANDIDATE_DESCRIPTION_MAX_CHARS" => {
			AgentRuntimeConfigError::InvalidCandidateDescriptionMaxChars
		}
		"ROKU_RUNTIME__AGENT__NEXT_STEP__EXPECTED_OUTPUT_TOKENS" => {
			AgentRuntimeConfigError::InvalidNextStepExpectedOutputTokens
		}
		"ROKU_RUNTIME__AGENT__NEXT_STEP__BUDGET_TOKENS_REMAINING" => {
			AgentRuntimeConfigError::InvalidNextStepBudgetTokensRemaining
		}
		"ROKU_RUNTIME__AGENT__NEXT_STEP__BUDGET_COST_REMAINING_USD" => {
			AgentRuntimeConfigError::InvalidNextStepBudgetCostRemainingUsd
		}
		_ => AgentRuntimeConfigError::InvalidVisibleToolHintMaxChars,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn defaults_are_stable() {
		let config = AgentRuntimeConfig::default();
		assert_eq!(config.r#loop.initial_step_budget, 10);
		assert_eq!(config.router.budget_tokens_remaining, 10_000);
		assert_eq!(config.prompts.visible_tool_hint_max_chars, 180);
		assert_eq!(config.next_step.expected_output_tokens, 1_200);
	}

	#[test]
	fn validate_and_clamp_caps_values_at_hard_limits() {
		let mut config = AgentRuntimeConfig::default();
		config.apply_patch(AgentRuntimeConfigPatch {
			r#loop: Some(LoopRuntimeConfigPatch {
				initial_step_budget: Some(HARD_MAX_INITIAL_STEP_BUDGET * 4),
				initial_recovery_budget: Some(HARD_MAX_INITIAL_RECOVERY_BUDGET * 4),
			}),
			router: Some(RouteClassifierRuntimeConfigPatch {
				expected_output_tokens: Some(HARD_MAX_ROUTE_EXPECTED_OUTPUT_TOKENS * 4),
				budget_tokens_remaining: Some(HARD_MAX_ROUTE_BUDGET_TOKENS_REMAINING * 4),
				budget_cost_remaining_usd: Some(HARD_MAX_ROUTE_BUDGET_COST_REMAINING_USD * 4.0),
				candidate_inventory_limit: Some(HARD_MAX_CANDIDATE_INVENTORY_LIMIT * 4),
			}),
			prompts: Some(PromptCompactionRuntimeConfigPatch {
				visible_tool_hint_max_chars: Some(HARD_MAX_VISIBLE_TOOL_HINT_MAX_CHARS * 4),
				candidate_description_max_chars: Some(HARD_MAX_CANDIDATE_DESCRIPTION_MAX_CHARS * 4),
			}),
			next_step: Some(NextStepRuntimeConfigPatch {
				expected_output_tokens: Some(HARD_MAX_NEXT_STEP_EXPECTED_OUTPUT_TOKENS * 4),
				budget_tokens_remaining: Some(HARD_MAX_NEXT_STEP_BUDGET_TOKENS_REMAINING * 4),
				budget_cost_remaining_usd: Some(HARD_MAX_NEXT_STEP_BUDGET_COST_REMAINING_USD * 4.0),
			}),
		});

		config.validate_and_clamp().expect("config should clamp");

		assert_eq!(
			config.r#loop.initial_step_budget,
			HARD_MAX_INITIAL_STEP_BUDGET
		);
		assert_eq!(
			config.router.candidate_inventory_limit,
			HARD_MAX_CANDIDATE_INVENTORY_LIMIT
		);
		assert_eq!(
			config.prompts.visible_tool_hint_max_chars,
			HARD_MAX_VISIBLE_TOOL_HINT_MAX_CHARS
		);
		assert_eq!(
			config.next_step.budget_tokens_remaining,
			HARD_MAX_NEXT_STEP_BUDGET_TOKENS_REMAINING
		);
	}

	#[test]
	fn reject_zero_values() {
		let mut config = AgentRuntimeConfig::default();
		config.r#loop.initial_step_budget = 0;
		assert_eq!(
			config.validate_and_clamp(),
			Err(AgentRuntimeConfigError::InvalidInitialStepBudget)
		);
	}
}
