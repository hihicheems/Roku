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

use roku_common_types::{PlanOutline, PlanStep, RequestEnvelope};
use roku_llm_adapter::{GenerationRequest, LlmRouter, RiskTier};
use roku_planning_engine::{PlanningDecision, PlanningMode};

use crate::planner::{AdaptiveTaskPlanner, TaskPlanner};

pub struct LlmTaskPlanner {
	router: LlmRouter,
	fallback: AdaptiveTaskPlanner,
}

impl LlmTaskPlanner {
	pub fn new(router: LlmRouter) -> Self {
		Self {
			router,
			fallback: AdaptiveTaskPlanner,
		}
	}

	fn generate_outline(
		&self,
		request: &RequestEnvelope,
		decision: &PlanningDecision,
	) -> Option<PlanOutline> {
		let prompt = planning_prompt(request, decision);
		let response = self
			.router
			.generate(&GenerationRequest {
				system_prompt: Some(
					"You are Roku's planning engine. Return only valid JSON that matches the requested schema."
						.to_string(),
				),
				prompt,
				expected_output_tokens: 900,
				risk_tier: planning_risk_tier(decision.mode),
				preferred_provider: None,
				budget_tokens_remaining: 8_000,
				budget_cost_remaining_usd: 1.0,
			})
			.ok()?;
		let raw_outline =
			serde_json::from_str::<PlanOutline>(&extract_json_payload(&response.output)).ok()?;
		normalize_outline(raw_outline, &request.goal)
	}
}

impl TaskPlanner for LlmTaskPlanner {
	fn build_outline(&self, request: &RequestEnvelope, decision: &PlanningDecision) -> PlanOutline {
		if matches!(decision.mode, PlanningMode::ReAct) {
			return self.fallback.build_outline(request, decision);
		}

		self.generate_outline(request, decision)
			.unwrap_or_else(|| self.fallback.build_outline(request, decision))
	}
}

fn planning_prompt(request: &RequestEnvelope, decision: &PlanningDecision) -> String {
	let history_block = render_conversation_history(&request.conversation_history);

	format!(
		r#"Return only JSON. Build a plan outline for the user goal.

JSON schema:
{{
  "goal": "string",
  "steps": [
    {{
      "step_id": "string",
      "summary": "string",
      "required_capabilities": ["string"],
      "requires_approval": false,
      "depends_on": ["string"]
    }}
  ]
}}

Rules:
- Keep between 2 and 6 steps.
- Preserve the user's goal exactly in the output goal field.
- Use concise, descriptive snake-case step_id values.
- Each summary must describe an actionable step.
- depends_on may only reference earlier step_ids.
- required_capabilities must be explicit and non-empty.
- Use requires_approval only when the step has review/finalization or risky execution semantics.
- planning_mode={mode:?}
- max_iterations={max_iterations}
- max_branches={max_branches}

User goal:
{goal}

Conversation history:
{history_block}"#,
		mode = decision.mode,
		max_iterations = decision.max_iterations,
		max_branches = decision.max_branches,
		goal = request.goal,
		history_block = history_block,
	)
}

fn planning_risk_tier(mode: PlanningMode) -> RiskTier {
	match mode {
		PlanningMode::ReAct => RiskTier::Low,
		PlanningMode::TaskDecomposition => RiskTier::Medium,
		PlanningMode::TreeSearch => RiskTier::High,
		PlanningMode::IterativeRefinement => RiskTier::High,
	}
}

fn extract_json_payload(payload: &str) -> String {
	let trimmed = payload.trim();
	if let Some(stripped) = trimmed.strip_prefix("```") {
		let without_language = stripped
			.strip_prefix("json")
			.map(str::trim_start)
			.unwrap_or(stripped);
		return without_language
			.strip_suffix("```")
			.map(str::trim)
			.unwrap_or(without_language)
			.to_string();
	}

	trimmed.to_string()
}

fn normalize_outline(outline: PlanOutline, goal: &str) -> Option<PlanOutline> {
	if outline.steps.is_empty() {
		return None;
	}

	let mut normalized_steps = Vec::with_capacity(outline.steps.len());
	for (index, step) in outline.steps.into_iter().enumerate() {
		let step_id = if step.step_id.trim().is_empty() {
			format!("step-{}", index + 1)
		} else {
			step.step_id
		};
		let mut depends_on = step
			.depends_on
			.into_iter()
			.filter(|dependency| {
				normalized_steps
					.iter()
					.any(|step: &PlanStep| step.step_id == *dependency)
			})
			.collect::<Vec<_>>();
		if depends_on.iter().any(|dependency| dependency == &step_id) {
			depends_on.retain(|dependency| dependency != &step_id);
		}

		let required_capabilities = if step.required_capabilities.is_empty() {
			vec!["tool.invoke".to_string()]
		} else {
			step.required_capabilities
		};
		let action = if let Some(action) = step
			.summary
			.lines()
			.find_map(|line| line.strip_prefix("Step: "))
		{
			action.trim().to_string()
		} else {
			step.summary.trim().to_string()
		};
		if action.is_empty() {
			return None;
		}

		normalized_steps.push(PlanStep {
			step_id,
			summary: format!("Goal: {goal}\nStep: {action}"),
			required_capabilities,
			requires_approval: step.requires_approval,
			depends_on,
		});
	}

	Some(PlanOutline {
		goal: goal.to_string(),
		steps: normalized_steps,
	})
}

fn render_conversation_history(history: &[roku_common_types::ConversationTurn]) -> String {
	if history.is_empty() {
		return "none".to_string();
	}

	history
		.iter()
		.map(|turn| format!("{:?}: {}", turn.role, turn.content))
		.collect::<Vec<_>>()
		.join("\n")
}

#[cfg(test)]
mod tests {
	use roku_common_types::RequestId;
	use roku_llm_adapter::{
		LlmProvider, ModelProfile, ProviderCallError, ProviderResponse, RoutingPolicy,
	};

	use super::*;

	struct PlanningProvider {
		output: &'static str,
	}

	impl LlmProvider for PlanningProvider {
		fn provider_name(&self) -> &'static str {
			"planner-provider"
		}

		fn complete(
			&self,
			_model: &ModelProfile,
			_request: &GenerationRequest,
		) -> Result<ProviderResponse, ProviderCallError> {
			Ok(ProviderResponse {
				output: self.output.to_string(),
				prompt_tokens: 60,
				output_tokens: 120,
				latency_ms: 80,
			})
		}
	}

	fn request() -> RequestEnvelope {
		RequestEnvelope {
			request_id: RequestId("req-1".to_string()),
			session_id: "s1".to_string(),
			goal: "ship a telegram-connected research assistant".to_string(),
			planning_mode_hint: None,
			conversation_history: Vec::new(),
		}
	}

	fn decision() -> PlanningDecision {
		PlanningDecision {
			mode: PlanningMode::TaskDecomposition,
			max_iterations: 4,
			max_branches: 2,
			hooks: Vec::new(),
		}
	}

	fn planner_with_output(output: &'static str) -> LlmTaskPlanner {
		let mut router = LlmRouter::new(RoutingPolicy {
			max_request_cost_usd: 1.0,
			max_latency_ms: 5_000,
		});
		router.register_provider(PlanningProvider { output });
		router.register_model(ModelProfile {
			model_id: "planner-model".to_string(),
			provider: "planner-provider".to_string(),
			max_context_tokens: 16_000,
			cost_per_1k_tokens_usd: 0.0,
			max_risk_tier: RiskTier::Critical,
			route_priority: 100,
		});
		LlmTaskPlanner::new(router)
	}

	#[test]
	fn llm_planner_uses_structured_outline_when_json_is_valid() {
		let planner = planner_with_output(
			r#"{
				"goal":"ignored by normalization",
				"steps":[
					{
						"step_id":"collect-context",
						"summary":"Collect runtime context",
						"required_capabilities":["information.read"],
						"requires_approval":false,
						"depends_on":[]
					},
					{
						"step_id":"execute",
						"summary":"Step: Execute the primary synthesis path",
						"required_capabilities":["tool.invoke"],
						"requires_approval":false,
						"depends_on":["collect-context"]
					}
				]
			}"#,
		);

		let outline = planner.build_outline(&request(), &decision());
		assert_eq!(outline.goal, request().goal);
		assert_eq!(outline.steps.len(), 2);
		assert_eq!(outline.steps[0].step_id, "collect-context");
		assert!(
			outline.steps[0]
				.summary
				.contains("Goal: ship a telegram-connected research assistant")
		);
		assert_eq!(
			outline.steps[1].depends_on,
			vec!["collect-context".to_string()]
		);
	}

	#[test]
	fn llm_planner_falls_back_when_output_is_invalid() {
		let planner = planner_with_output("not-json");
		let outline = planner.build_outline(&request(), &decision());

		assert!(
			outline
				.steps
				.iter()
				.any(|step| step.step_id == "decompose-goal")
		);
	}

	#[test]
	fn llm_planner_uses_deterministic_fast_path_for_react_mode() {
		let planner = planner_with_output("this should never be used");
		let outline = planner.build_outline(
			&request(),
			&PlanningDecision {
				mode: PlanningMode::ReAct,
				max_iterations: 4,
				max_branches: 1,
				hooks: Vec::new(),
			},
		);

		assert_eq!(outline.steps.len(), 2);
		assert_eq!(outline.steps[0].step_id, "observe-context");
		assert_eq!(outline.steps[1].step_id, "act-primary");
	}
}
