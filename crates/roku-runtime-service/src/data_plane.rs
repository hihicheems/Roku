use roku_common_types::{
	ApprovalTicket, Artifact, ArtifactId, ExperimentMetric, ExperimentRun, NodeId, ResultEnvelope,
	RuntimeError, Task, TaskId, TaskNode, TaskState, ValidationEvidenceSet,
};

use crate::RuntimeService;

impl RuntimeService {
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
		node_id: &NodeId,
	) -> Result<Vec<ValidationEvidenceSet>, RuntimeError> {
		let results = self.collect_upstream_results(task, node_id)?;
		let mut evidence_sets = Vec::with_capacity(results.len());
		for result in results {
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
}
