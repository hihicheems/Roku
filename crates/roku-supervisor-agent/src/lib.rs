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
	ErrorClass, NodeId, RequestEnvelope, ResultEnvelope, ResultStatus, RuntimeError, Task,
	TaskEdgeCondition, TaskGraph, TaskNodeKind,
};
use roku_execution_graph_builder::TaskGraphScheduler;
use roku_planning_engine::{
	DefaultPlanningEngine, PlanningDecision, PlanningInput, PlanningMode, RiskLevel,
	StrategySelector,
};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone)]
pub struct SupervisorInput {
	pub normalized_goal: String,
	pub planning_input: PlanningInput,
	pub planning_mode_hint: Option<PlanningMode>,
}

#[derive(Debug, Clone)]
pub struct SupervisorDecision {
	pub input: SupervisorInput,
	pub planning_decision: PlanningDecision,
}

#[derive(Debug, Clone)]
pub struct SupervisorExecutionFeedback {
	pub completed_nodes: usize,
	pub attempts: u32,
	pub last_error: Option<ErrorClass>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionAssessment {
	pub completed: bool,
	pub reason: String,
	pub final_node_id: Option<NodeId>,
	pub final_message: Option<String>,
}

pub trait SupervisorAgent {
	fn plan(&self, request: &RequestEnvelope) -> SupervisorDecision;
	fn should_replan(&self, feedback: &SupervisorExecutionFeedback) -> bool;
	fn assess_completion(
		&self,
		task: &Task,
		results: &[ResultEnvelope],
	) -> Result<CompletionAssessment, RuntimeError>;
}

#[derive(Debug, Default)]
pub struct DefaultSupervisorAgent {
	planning_engine: DefaultPlanningEngine,
}

impl DefaultSupervisorAgent {
	pub fn planning_input_for_request(&self, request: &RequestEnvelope) -> PlanningInput {
		let normalized_goal = normalize_goal(&request.goal);
		let goal = normalized_goal.to_ascii_lowercase();
		let word_count = u64::try_from(goal.split_whitespace().count()).unwrap_or(u64::MAX);
		let complexity_keywords = [
			"and",
			"then",
			"compare",
			"analyze",
			"research",
			"plan",
			"build",
			"integrate",
			"deploy",
			"workflow",
		];
		let uncertainty_keywords = [
			"maybe",
			"explore",
			"option",
			"alternatives",
			"unknown",
			"unclear",
			"investigate",
			"hypothesis",
			"why",
		];
		let high_risk_keywords = [
			"delete",
			"production",
			"payment",
			"secret",
			"credential",
			"approve",
		];
		let medium_risk_keywords = ["write", "publish", "external", "notify", "mutation"];

		let complexity_hits = keyword_hits(&goal, &complexity_keywords);
		let uncertainty_hits = keyword_hits(&goal, &uncertainty_keywords);
		let complexity_score = score_from_hits(word_count, complexity_hits, 6, 10);
		let uncertainty_score = score_from_hits(word_count / 8, uncertainty_hits, 4, 10);
		let risk_level = if contains_any_keyword(&goal, &high_risk_keywords) {
			RiskLevel::High
		} else if contains_any_keyword(&goal, &medium_risk_keywords) {
			RiskLevel::Medium
		} else {
			RiskLevel::Low
		};
		let risk_budget = match risk_level {
			RiskLevel::Low => 0,
			RiskLevel::Medium => 2_000,
			RiskLevel::High => 4_000,
		};
		let budget_tokens = 4_000u64
			.saturating_add(word_count.saturating_mul(120))
			.saturating_add(u64::from(complexity_score).saturating_mul(250))
			.saturating_add(u64::from(uncertainty_score).saturating_mul(150))
			.saturating_add(risk_budget);

		PlanningInput {
			complexity_score,
			uncertainty_score,
			risk_level,
			budget_tokens,
		}
	}

	fn input_for_request(&self, request: &RequestEnvelope) -> SupervisorInput {
		SupervisorInput {
			normalized_goal: normalize_goal(&request.goal),
			planning_input: self.planning_input_for_request(request),
			planning_mode_hint: request.planning_mode_hint.map(planning_mode_from_hint),
		}
	}
}

impl SupervisorAgent for DefaultSupervisorAgent {
	fn plan(&self, request: &RequestEnvelope) -> SupervisorDecision {
		let input = self.input_for_request(request);
		let planning_decision = input
			.planning_mode_hint
			.map(|mode| {
				self.planning_engine
					.decision_for_mode(mode, &input.planning_input)
			})
			.unwrap_or_else(|| self.planning_engine.select(&input.planning_input));

		SupervisorDecision {
			input,
			planning_decision,
		}
	}

	fn should_replan(&self, feedback: &SupervisorExecutionFeedback) -> bool {
		matches!(
			feedback.last_error,
			Some(ErrorClass::Validation | ErrorClass::Dependency | ErrorClass::Timeout)
		) && feedback.attempts < 3
			&& feedback.completed_nodes > 0
	}

	fn assess_completion(
		&self,
		task: &Task,
		results: &[ResultEnvelope],
	) -> Result<CompletionAssessment, RuntimeError> {
		let Some(graph) = &task.graph else {
			return Ok(CompletionAssessment {
				completed: false,
				reason: "task graph is missing".to_string(),
				final_node_id: None,
				final_message: None,
			});
		};

		let scheduler = TaskGraphScheduler;
		let is_complete = scheduler
			.is_complete(graph, &task.completed_nodes)
			.map_err(|error| RuntimeError::new(error.to_string()))?;

		let selected_result = is_complete
			.then(|| select_final_result(graph, results))
			.flatten();
		let selected_message = selected_result.map(result_message);
		let selected_node_id = selected_result.map(|result| result.node_id.clone());

		Ok(CompletionAssessment {
			completed: is_complete,
			reason: if is_complete {
				"all task graph nodes completed".to_string()
			} else {
				"task graph still has incomplete nodes".to_string()
			},
			final_node_id: selected_node_id,
			final_message: selected_message,
		})
	}
}

fn select_final_result<'a>(
	graph: &TaskGraph,
	results: &'a [ResultEnvelope],
) -> Option<&'a ResultEnvelope> {
	let successful_results = results
		.iter()
		.filter(|result| matches!(result.status, ResultStatus::Ok))
		.collect::<Vec<_>>();
	if successful_results.is_empty() {
		return None;
	}

	let node_by_id = graph
		.nodes
		.iter()
		.map(|node| (node.node_id.0.as_str(), node))
		.collect::<HashMap<_, _>>();
	let node_position = graph
		.nodes
		.iter()
		.enumerate()
		.map(|(index, node)| (node.node_id.0.as_str(), index))
		.collect::<HashMap<_, _>>();
	let completion_path_sources = graph
		.edges
		.iter()
		.filter(|edge| edge_is_completion_path(edge.condition))
		.map(|edge| edge.from.0.as_str())
		.collect::<HashSet<_>>();

	for kind in [
		TaskNodeKind::Aggregation,
		TaskNodeKind::Validation,
		TaskNodeKind::Execution,
	] {
		if let Some(result) = pick_best_result(
			successful_results.iter().copied().filter(|result| {
				node_by_id
					.get(result.node_id.0.as_str())
					.is_some_and(|node| node.kind == kind)
					&& !completion_path_sources.contains(result.node_id.0.as_str())
			}),
			&node_position,
		) {
			return Some(result);
		}
	}

	for kind in [
		TaskNodeKind::Aggregation,
		TaskNodeKind::Validation,
		TaskNodeKind::Execution,
	] {
		if let Some(result) = pick_best_result(
			successful_results.iter().copied().filter(|result| {
				node_by_id
					.get(result.node_id.0.as_str())
					.is_some_and(|node| node.kind == kind)
			}),
			&node_position,
		) {
			return Some(result);
		}
	}

	pick_best_result(successful_results.into_iter(), &node_position)
}

fn pick_best_result<'a>(
	candidates: impl Iterator<Item = &'a ResultEnvelope>,
	node_position: &HashMap<&str, usize>,
) -> Option<&'a ResultEnvelope> {
	candidates.max_by(|left, right| {
		left.confidence
			.total_cmp(&right.confidence)
			.then_with(|| {
				let left_position = node_position
					.get(left.node_id.0.as_str())
					.copied()
					.unwrap_or(0);
				let right_position = node_position
					.get(right.node_id.0.as_str())
					.copied()
					.unwrap_or(0);
				left_position.cmp(&right_position)
			})
			.then_with(|| left.node_id.0.cmp(&right.node_id.0))
	})
}

fn edge_is_completion_path(condition: TaskEdgeCondition) -> bool {
	matches!(
		condition,
		TaskEdgeCondition::Always | TaskEdgeCondition::OnSuccess | TaskEdgeCondition::OnApproved
	)
}

fn result_message(result: &ResultEnvelope) -> String {
	extract_message(&result.payload).unwrap_or_else(|| result.payload.clone())
}

fn extract_message(payload: &str) -> Option<String> {
	let parsed = serde_json::from_str::<serde_json::Value>(payload).ok()?;
	parsed
		.get("message")
		.and_then(serde_json::Value::as_str)
		.map(str::to_string)
}

fn normalize_goal(goal: &str) -> String {
	let normalized = goal.lines().map(str::trim).collect::<Vec<_>>().join("\n");
	if normalized.trim().is_empty() {
		goal.trim().to_string()
	} else {
		normalized
	}
}

fn planning_mode_from_hint(hint: roku_common_types::PlanningModeHint) -> PlanningMode {
	match hint {
		roku_common_types::PlanningModeHint::ReAct => PlanningMode::ReAct,
		roku_common_types::PlanningModeHint::TaskDecomposition => PlanningMode::TaskDecomposition,
		roku_common_types::PlanningModeHint::TreeSearch => PlanningMode::TreeSearch,
		roku_common_types::PlanningModeHint::IterativeRefinement => {
			PlanningMode::IterativeRefinement
		}
	}
}

fn keyword_hits(goal: &str, keywords: &[&str]) -> u8 {
	let hits = keywords
		.iter()
		.filter(|keyword| goal.contains(**keyword))
		.count();
	u8::try_from(hits).unwrap_or(u8::MAX)
}

fn contains_any_keyword(goal: &str, keywords: &[&str]) -> bool {
	keywords.iter().any(|keyword| goal.contains(keyword))
}

fn score_from_hits(base: u64, hits: u8, divisor: u64, max_score: u8) -> u8 {
	let derived = 2u64
		.saturating_add(base / divisor)
		.saturating_add(u64::from(hits).saturating_mul(2));
	let capped = derived.min(u64::from(max_score));
	u8::try_from(capped).unwrap_or(max_score)
}

#[cfg(test)]
mod tests {
	use super::*;
	use roku_common_types::{
		NodeId, RequestEnvelope, RequestId, ResultEnvelope, ResultStatus, TaskEdge, TaskGraph,
		TaskId, TaskNode, TaskNodeKind, TaskState,
	};

	fn sample_request(goal: &str) -> RequestEnvelope {
		RequestEnvelope {
			request_id: RequestId("req-1".to_string()),
			session_id: "session-1".to_string(),
			goal: goal.to_string(),
			planning_mode_hint: None,
			conversation_history: Vec::new(),
		}
	}

	fn sample_node(node_id: &str) -> TaskNode {
		TaskNode {
			node_id: NodeId(node_id.to_string()),
			kind: TaskNodeKind::Execution,
			description: node_id.to_string(),
			capabilities: Vec::new(),
			join_policy: roku_common_types::JoinPolicy::AllParents,
			aggregation_mode: roku_common_types::AggregationMode::CollectAll,
			..TaskNode::default()
		}
	}

	fn sample_result(node_id: &str, message: &str, confidence: f32) -> ResultEnvelope {
		ResultEnvelope {
			task_id: TaskId("task-1".to_string()),
			node_id: NodeId(node_id.to_string()),
			producer: format!("producer:{node_id}"),
			schema_version: "result.v1".to_string(),
			status: ResultStatus::Ok,
			payload: serde_json::json!({
				"message": message,
			})
			.to_string(),
			evidence: Vec::new(),
			confidence,
		}
	}

	#[test]
	fn plan_respects_request_hint() {
		let supervisor = DefaultSupervisorAgent::default();
		let mut request = sample_request("investigate options");
		request.planning_mode_hint = Some(roku_common_types::PlanningModeHint::TreeSearch);

		let decision = supervisor.plan(&request);
		assert_eq!(decision.planning_decision.mode, PlanningMode::TreeSearch);
		assert_eq!(decision.input.normalized_goal, "investigate options");
	}

	#[test]
	fn planning_input_scales_for_complex_goal() {
		let supervisor = DefaultSupervisorAgent::default();
		let input = supervisor.planning_input_for_request(&sample_request(
			"build and integrate a workflow to research alternatives, compare options, and deploy a production-ready bot",
		));

		assert!(input.complexity_score >= 6);
		assert!(input.budget_tokens >= 6_000);
	}

	#[test]
	fn assess_completion_uses_task_graph_status() {
		let supervisor = DefaultSupervisorAgent::default();
		let task = Task {
			task_id: roku_common_types::TaskId("task-1".to_string()),
			request_id: RequestId("req-1".to_string()),
			session_id: "session-1".to_string(),
			goal: "goal".to_string(),
			state: TaskState::Executing,
			attempts: 0,
			planning_mode_hint: None,
			conversation_history: Vec::new(),
			completed_nodes: vec![NodeId("step-1".to_string())],
			next_node_index: 1,
			pending_approval_id: None,
			last_result: None,
			compensation_records: Vec::new(),
			graph: Some(TaskGraph {
				task_id: roku_common_types::TaskId("task-1".to_string()),
				nodes: vec![sample_node("step-1")],
				edges: Vec::new(),
			}),
		};

		let assessment = supervisor
			.assess_completion(&task, &[])
			.expect("completion assessment should succeed");
		assert!(assessment.completed);
		assert_eq!(assessment.final_node_id, None);
	}

	#[test]
	fn assess_completion_prefers_terminal_aggregation_result() {
		let supervisor = DefaultSupervisorAgent::default();
		let mut aggregation_node = sample_node("aggregation-gate");
		aggregation_node.kind = TaskNodeKind::Aggregation;
		let task = Task {
			task_id: TaskId("task-1".to_string()),
			request_id: RequestId("req-1".to_string()),
			session_id: "session-1".to_string(),
			goal: "goal".to_string(),
			state: TaskState::Aggregating,
			attempts: 0,
			planning_mode_hint: None,
			conversation_history: Vec::new(),
			completed_nodes: vec![
				NodeId("step-1".to_string()),
				NodeId("aggregation-gate".to_string()),
			],
			next_node_index: 2,
			pending_approval_id: None,
			last_result: None,
			compensation_records: Vec::new(),
			graph: Some(TaskGraph {
				task_id: TaskId("task-1".to_string()),
				nodes: vec![aggregation_node, sample_node("step-1")],
				edges: vec![TaskEdge {
					from: NodeId("step-1".to_string()),
					to: NodeId("aggregation-gate".to_string()),
					condition: roku_common_types::TaskEdgeCondition::OnSuccess,
				}],
			}),
		};
		let results = vec![
			sample_result("step-1", "execution summary", 0.7),
			sample_result("aggregation-gate", "aggregated summary", 0.6),
		];

		let assessment = supervisor
			.assess_completion(&task, &results)
			.expect("completion assessment should succeed");

		assert!(assessment.completed);
		assert_eq!(
			assessment.final_node_id,
			Some(NodeId("aggregation-gate".to_string()))
		);
		assert_eq!(
			assessment.final_message.as_deref(),
			Some("aggregated summary")
		);
	}

	#[test]
	fn assess_completion_falls_back_to_terminal_execution_result() {
		let supervisor = DefaultSupervisorAgent::default();
		let task = Task {
			task_id: TaskId("task-1".to_string()),
			request_id: RequestId("req-1".to_string()),
			session_id: "session-1".to_string(),
			goal: "goal".to_string(),
			state: TaskState::Executing,
			attempts: 0,
			planning_mode_hint: None,
			conversation_history: Vec::new(),
			completed_nodes: vec![NodeId("step-1".to_string())],
			next_node_index: 1,
			pending_approval_id: None,
			last_result: None,
			compensation_records: Vec::new(),
			graph: Some(TaskGraph {
				task_id: TaskId("task-1".to_string()),
				nodes: vec![sample_node("step-1")],
				edges: Vec::new(),
			}),
		};
		let results = vec![sample_result("step-1", "execution summary", 0.8)];

		let assessment = supervisor
			.assess_completion(&task, &results)
			.expect("completion assessment should succeed");

		assert!(assessment.completed);
		assert_eq!(assessment.final_node_id, Some(NodeId("step-1".to_string())));
		assert_eq!(
			assessment.final_message.as_deref(),
			Some("execution summary")
		);
	}

	#[test]
	fn replan_policy_is_limited_to_recoverable_failures() {
		let supervisor = DefaultSupervisorAgent::default();
		assert!(supervisor.should_replan(&SupervisorExecutionFeedback {
			completed_nodes: 1,
			attempts: 1,
			last_error: Some(ErrorClass::Validation),
		}));
		assert!(!supervisor.should_replan(&SupervisorExecutionFeedback {
			completed_nodes: 0,
			attempts: 1,
			last_error: Some(ErrorClass::Validation),
		}));
		assert!(!supervisor.should_replan(&SupervisorExecutionFeedback {
			completed_nodes: 1,
			attempts: 3,
			last_error: Some(ErrorClass::Validation),
		}));
	}
}
