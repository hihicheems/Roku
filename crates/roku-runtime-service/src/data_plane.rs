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
	AggregationMode, ApprovalTicket, Artifact, ArtifactId, ExperimentMetric, ExperimentRun,
	JoinPolicy, NodeId, NodeResultSet, ResultEnvelope, RuntimeError, Task, TaskEvent, TaskId,
	TaskNode, TaskReplayReport, TaskState, ValidationEvidenceSet,
};
use roku_orchestrator::is_valid_transition;

use crate::RuntimeService;

impl RuntimeService {
	pub fn get_task(&self, task_id: &TaskId) -> Result<Option<Task>, RuntimeError> {
		let state = self.lock_state()?;
		state
			.task_repo
			.load_task(task_id)
			.map_err(|error| RuntimeError::new(error.to_string()))
	}

	pub fn list_task_events(&self, task_id: &TaskId) -> Result<Vec<TaskEvent>, RuntimeError> {
		let state = self.lock_state()?;
		state
			.event_repo
			.list_events(task_id)
			.map_err(|error| RuntimeError::new(error.to_string()))
	}

	pub fn get_task_replay_report(
		&self,
		task_id: &TaskId,
	) -> Result<Option<TaskReplayReport>, RuntimeError> {
		let Some(task) = self.get_task(task_id)? else {
			return Ok(None);
		};
		let events = self.list_task_events(task_id)?;
		Ok(Some(build_replay_report(task, events)))
	}

	pub fn list_artifacts(&self, task_id: &TaskId) -> Result<Vec<Artifact>, RuntimeError> {
		let state = self.lock_state()?;
		state
			.artifact_store
			.list_by_task(task_id)
			.map_err(|error| RuntimeError::new(error.to_string()))
	}

	pub fn get_experiment_run(
		&self,
		task_id: &TaskId,
	) -> Result<Option<ExperimentRun>, RuntimeError> {
		let state = self.lock_state()?;
		state
			.experiment_registry
			.load_by_task(task_id)
			.map_err(|error| RuntimeError::new(error.to_string()))
	}

	pub fn get_artifact_content(
		&self,
		task_id: &TaskId,
		artifact_id: &ArtifactId,
	) -> Result<Option<String>, RuntimeError> {
		let state = self.lock_state()?;
		let Some(artifact) = state
			.artifact_store
			.load_artifact(artifact_id)
			.map_err(|error| RuntimeError::new(error.to_string()))?
		else {
			return Ok(None);
		};

		if artifact.task_id != *task_id {
			return Err(RuntimeError::new("artifact does not belong to task"));
		}

		state
			.artifact_store
			.load_content_by_uri(&artifact.uri)
			.map_err(|error| RuntimeError::new(error.to_string()))
	}

	pub(super) fn record_transition(
		&self,
		task: &mut Task,
		next: TaskState,
		reason: &str,
	) -> Result<(), RuntimeError> {
		let event = self.orchestrator.transition(task, next, reason, None)?;
		let mut state = self.lock_state()?;
		state
			.event_repo
			.append_event(event)
			.map_err(|error| RuntimeError::new(error.to_string()))
	}

	pub(super) fn save_task(&self, task: Task) -> Result<(), RuntimeError> {
		let mut state = self.lock_state()?;
		state
			.task_repo
			.save_task(task)
			.map_err(|error| RuntimeError::new(error.to_string()))
	}

	pub(super) fn save_approval_ticket(&self, ticket: ApprovalTicket) -> Result<(), RuntimeError> {
		let mut state = self.lock_state()?;
		state
			.approval_repo
			.save_ticket(ticket)
			.map_err(|error| RuntimeError::new(error.to_string()))
	}

	pub(super) fn save_result(&self, result: ResultEnvelope) -> Result<(), RuntimeError> {
		let mut state = self.lock_state()?;
		state
			.result_repo
			.save_result(result)
			.map_err(|error| RuntimeError::new(error.to_string()))
	}

	pub(super) fn persist_result_artifact(
		&self,
		result: &ResultEnvelope,
	) -> Result<Artifact, RuntimeError> {
		let mut state = self.lock_state()?;
		state
			.artifact_store
			.persist_result_artifact(result)
			.map_err(|error| RuntimeError::new(error.to_string()))
	}

	pub(super) fn attach_artifact_to_experiment(
		&self,
		task_id: &TaskId,
		artifact_id: ArtifactId,
	) -> Result<(), RuntimeError> {
		let mut state = self.lock_state()?;
		state
			.experiment_registry
			.attach_artifact(task_id, artifact_id)
			.map(|_| ())
			.map_err(|error| RuntimeError::new(error.to_string()))
	}

	pub(super) fn list_results(
		&self,
		task_id: &TaskId,
	) -> Result<Vec<ResultEnvelope>, RuntimeError> {
		let state = self.lock_state()?;
		state
			.result_repo
			.list_results(task_id)
			.map_err(|error| RuntimeError::new(error.to_string()))
	}

	pub(super) fn start_experiment_run(
		&self,
		task: &Task,
		goal: &str,
		strategy: &str,
	) -> Result<(), RuntimeError> {
		let mut state = self.lock_state()?;
		state
			.experiment_registry
			.start_run(&task.task_id, &task.request_id, goal, strategy)
			.map_err(|error| RuntimeError::new(error.to_string()))?;
		self.metrics.inc_experiments_started();
		Ok(())
	}

	pub(super) fn complete_experiment_run(
		&self,
		task: &Task,
		result_count: usize,
	) -> Result<(), RuntimeError> {
		let mut state = self.lock_state()?;
		state
			.experiment_registry
			.complete_run(
				&task.task_id,
				"task succeeded",
				vec![
					ExperimentMetric {
						name: "completed_nodes".to_string(),
						value: task.completed_nodes.len() as f64,
					},
					ExperimentMetric {
						name: "validated_results".to_string(),
						value: result_count as f64,
					},
				],
			)
			.map_err(|error| RuntimeError::new(error.to_string()))?;
		self.metrics.inc_experiments_succeeded();
		Ok(())
	}

	pub(super) fn fail_experiment_run(
		&self,
		task: &Task,
		reason: &str,
	) -> Result<(), RuntimeError> {
		let mut state = self.lock_state()?;
		state
			.experiment_registry
			.fail_run(&task.task_id, reason)
			.map_err(|error| RuntimeError::new(error.to_string()))?;
		self.metrics.inc_experiments_failed();
		Ok(())
	}

	#[cfg(test)]
	pub(super) fn collect_upstream_results(
		&self,
		task: &Task,
		node_id: &NodeId,
	) -> Result<Vec<ResultEnvelope>, RuntimeError> {
		let graph = task
			.graph
			.as_ref()
			.ok_or_else(|| RuntimeError::new("task graph is missing"))?;
		let mut pending = graph
			.edges
			.iter()
			.filter(|edge| edge.to == *node_id)
			.map(|edge| edge.from.clone())
			.collect::<Vec<_>>();
		let mut visited = std::collections::HashSet::new();
		let mut results = Vec::new();
		let state = self.lock_state()?;

		while let Some(current) = pending.pop() {
			if !visited.insert(current.0.clone()) {
				continue;
			}

			if let Some(result) = state
				.result_repo
				.load_result(&task.task_id, &current)
				.map_err(|error| RuntimeError::new(error.to_string()))?
			{
				results.push(result);
				continue;
			}

			pending.extend(
				graph
					.edges
					.iter()
					.filter(|edge| edge.to == current)
					.map(|edge| edge.from.clone()),
			);
		}

		Ok(results)
	}

	pub(super) fn collect_node_result_set(
		&self,
		task: &Task,
		node: &TaskNode,
	) -> Result<NodeResultSet, RuntimeError> {
		let graph = task
			.graph
			.as_ref()
			.ok_or_else(|| RuntimeError::new("task graph is missing"))?;
		let branch_sources = graph
			.edges
			.iter()
			.filter(|edge| edge.to == node.node_id)
			.map(|edge| edge.from.clone())
			.collect::<Vec<_>>();
		let mut source_node_ids = Vec::new();
		let mut missing_source_nodes = Vec::new();
		let mut results = Vec::new();

		for branch_root in &branch_sources {
			let branch_results = self.resolve_branch_results(task, branch_root)?;
			if branch_results.is_empty() {
				missing_source_nodes.push(branch_root.clone());
				continue;
			}

			source_node_ids.push(branch_root.clone());
			results.extend(branch_results);
		}

		let results = apply_aggregation_mode(results, node.aggregation_mode);
		if !join_policy_satisfied(
			node.join_policy,
			branch_sources.len(),
			source_node_ids.len(),
		) {
			return Err(RuntimeError::new(format!(
				"join policy {:?} is not satisfied for node {}",
				node.join_policy, node.node_id.0
			)));
		}

		Ok(NodeResultSet {
			node_id: node.node_id.clone(),
			join_policy: node.join_policy,
			aggregation_mode: node.aggregation_mode,
			source_node_ids,
			missing_source_nodes,
			results,
		})
	}

	pub(super) fn load_artifacts_for_result(
		&self,
		result: &ResultEnvelope,
	) -> Result<Vec<Artifact>, RuntimeError> {
		let state = self.lock_state()?;
		let mut artifacts = Vec::new();

		for evidence in result
			.evidence
			.iter()
			.filter(|item| item.kind == "artifact_ref")
		{
			if let Some(artifact) = state
				.artifact_store
				.load_by_uri(&evidence.value)
				.map_err(|error| RuntimeError::new(error.to_string()))?
			{
				artifacts.push(artifact);
			}
		}

		Ok(artifacts)
	}

	pub(super) fn collect_validation_evidence(
		&self,
		task: &Task,
		node: &TaskNode,
	) -> Result<Vec<ValidationEvidenceSet>, RuntimeError> {
		let result_set = self.collect_node_result_set(task, node)?;
		let mut evidence_sets = Vec::with_capacity(result_set.results.len());
		for result in result_set.results {
			let artifacts = self.load_artifacts_for_result(&result)?;
			evidence_sets.push(ValidationEvidenceSet { result, artifacts });
		}
		Ok(evidence_sets)
	}

	pub(super) fn mark_node_completed(&self, task: &mut Task, node: &TaskNode) {
		self.mark_node_completed_by_id(task, &node.node_id);
	}

	pub(super) fn mark_node_completed_by_id(&self, task: &mut Task, node_id: &NodeId) {
		if task
			.completed_nodes
			.iter()
			.all(|completed| completed != node_id)
		{
			task.completed_nodes.push(node_id.clone());
		}
		task.next_node_index = task.completed_nodes.len();
	}

	pub(super) fn lock_state(
		&self,
	) -> Result<std::sync::MutexGuard<'_, crate::RuntimeState>, RuntimeError> {
		self.state
			.lock()
			.map_err(|_| RuntimeError::new("runtime state lock poisoned"))
	}

	fn resolve_branch_results(
		&self,
		task: &Task,
		node_id: &NodeId,
	) -> Result<Vec<ResultEnvelope>, RuntimeError> {
		let graph = task
			.graph
			.as_ref()
			.ok_or_else(|| RuntimeError::new("task graph is missing"))?;
		let state = self.lock_state()?;

		if let Some(result) = state
			.result_repo
			.load_result(&task.task_id, node_id)
			.map_err(|error| RuntimeError::new(error.to_string()))?
		{
			return Ok(vec![result]);
		}

		drop(state);

		let mut results = Vec::new();
		for parent in graph
			.edges
			.iter()
			.filter(|edge| edge.to == *node_id)
			.map(|edge| edge.from.clone())
		{
			results.extend(self.resolve_branch_results(task, &parent)?);
		}
		Ok(results)
	}
}

fn join_policy_satisfied(policy: JoinPolicy, branch_count: usize, resolved_count: usize) -> bool {
	match policy {
		JoinPolicy::AllParents => resolved_count == branch_count,
		JoinPolicy::AnyParent => resolved_count >= 1,
		JoinPolicy::Quorum(required) => resolved_count >= usize::from(required),
	}
}

fn apply_aggregation_mode(
	mut results: Vec<ResultEnvelope>,
	mode: AggregationMode,
) -> Vec<ResultEnvelope> {
	match mode {
		AggregationMode::CollectAll => results,
		AggregationMode::HighestConfidence => {
			if let Some(best) = results
				.drain(..)
				.max_by(|left, right| left.confidence.total_cmp(&right.confidence))
			{
				vec![best]
			} else {
				Vec::new()
			}
		}
	}
}

fn build_replay_report(task: Task, events: Vec<TaskEvent>) -> TaskReplayReport {
	let replayed_state = events.last().map(|event| event.to).unwrap_or(task.state);
	let transitions_valid = events
		.iter()
		.all(|event| is_valid_transition(event.from, event.to));
	let chain_consistent = events
		.windows(2)
		.all(|window| window[0].to == window[1].from);

	TaskReplayReport {
		task_id: task.task_id,
		persisted_state: task.state,
		replayed_state,
		event_count: events.len(),
		transitions_valid,
		chain_consistent,
		snapshot_matches_replay: task.state == replayed_state,
		recoverable: is_recoverable_state(task.state),
		events,
	}
}

fn is_recoverable_state(state: TaskState) -> bool {
	matches!(
		state,
		TaskState::Planning
			| TaskState::GraphBuilding
			| TaskState::Delegating
			| TaskState::Executing
			| TaskState::WaitingApproval
			| TaskState::Validating
			| TaskState::Aggregating
			| TaskState::Failed
	)
}
