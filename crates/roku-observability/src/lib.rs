//! Observability primitives and lightweight exporters.

use std::fs::OpenOptions;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceContext {
	pub trace_id: String,
	pub span_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditCorrelation {
	pub trace_id: String,
	pub span_id: String,
	pub task_id: Option<String>,
	pub request_id: Option<String>,
}

impl From<TraceContext> for AuditCorrelation {
	fn from(value: TraceContext) -> Self {
		Self {
			trace_id: value.trace_id,
			span_id: value.span_id,
			task_id: None,
			request_id: None,
		}
	}
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditAttribute {
	pub key: String,
	pub value: String,
}

#[derive(Debug)]
pub struct Metrics {
	pub requests_total: AtomicU64,
	pub failures_total: AtomicU64,
	pub validation_failures_total: AtomicU64,
	pub planning_runs_total: AtomicU64,
	pub planning_react_total: AtomicU64,
	pub planning_task_decomposition_total: AtomicU64,
	pub planning_tree_search_total: AtomicU64,
	pub planning_iterative_refinement_total: AtomicU64,
	pub approvals_created_total: AtomicU64,
	pub approvals_resolved_total: AtomicU64,
	pub dead_letters_total: AtomicU64,
	pub artifacts_total: AtomicU64,
	pub experiments_started_total: AtomicU64,
	pub experiments_succeeded_total: AtomicU64,
	pub experiments_failed_total: AtomicU64,
}

impl Default for Metrics {
	fn default() -> Self {
		Self {
			requests_total: AtomicU64::new(0),
			failures_total: AtomicU64::new(0),
			validation_failures_total: AtomicU64::new(0),
			planning_runs_total: AtomicU64::new(0),
			planning_react_total: AtomicU64::new(0),
			planning_task_decomposition_total: AtomicU64::new(0),
			planning_tree_search_total: AtomicU64::new(0),
			planning_iterative_refinement_total: AtomicU64::new(0),
			approvals_created_total: AtomicU64::new(0),
			approvals_resolved_total: AtomicU64::new(0),
			dead_letters_total: AtomicU64::new(0),
			artifacts_total: AtomicU64::new(0),
			experiments_started_total: AtomicU64::new(0),
			experiments_succeeded_total: AtomicU64::new(0),
			experiments_failed_total: AtomicU64::new(0),
		}
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetricsSnapshot {
	pub requests_total: u64,
	pub failures_total: u64,
	pub validation_failures_total: u64,
	pub planning_runs_total: u64,
	pub planning_react_total: u64,
	pub planning_task_decomposition_total: u64,
	pub planning_tree_search_total: u64,
	pub planning_iterative_refinement_total: u64,
	pub approvals_created_total: u64,
	pub approvals_resolved_total: u64,
	pub dead_letters_total: u64,
	pub artifacts_total: u64,
	pub experiments_started_total: u64,
	pub experiments_succeeded_total: u64,
	pub experiments_failed_total: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlanningModeLabel {
	ReAct,
	TaskDecomposition,
	TreeSearch,
	IterativeRefinement,
	Unknown,
}

fn normalize_planning_mode(mode_label: &str) -> PlanningModeLabel {
	let normalized = mode_label
		.chars()
		.filter(|character| character.is_ascii_alphanumeric())
		.collect::<String>()
		.to_ascii_lowercase();

	match normalized.as_str() {
		"react" => PlanningModeLabel::ReAct,
		"taskdecomposition" => PlanningModeLabel::TaskDecomposition,
		"treesearch" => PlanningModeLabel::TreeSearch,
		"iterativerefinement" => PlanningModeLabel::IterativeRefinement,
		_ => PlanningModeLabel::Unknown,
	}
}

impl Metrics {
	pub fn inc_requests(&self) {
		self.requests_total.fetch_add(1, Ordering::Relaxed);
	}

	pub fn inc_failures(&self) {
		self.failures_total.fetch_add(1, Ordering::Relaxed);
	}

	pub fn inc_validation_failures(&self) {
		self.validation_failures_total
			.fetch_add(1, Ordering::Relaxed);
	}

	pub fn inc_planning_run(&self) {
		self.planning_runs_total.fetch_add(1, Ordering::Relaxed);
	}

	pub fn inc_planning_strategy(&self, mode_label: &str) {
		match normalize_planning_mode(mode_label) {
			PlanningModeLabel::ReAct => {
				self.planning_react_total.fetch_add(1, Ordering::Relaxed);
			}
			PlanningModeLabel::TaskDecomposition => {
				self.planning_task_decomposition_total
					.fetch_add(1, Ordering::Relaxed);
			}
			PlanningModeLabel::TreeSearch => {
				self.planning_tree_search_total
					.fetch_add(1, Ordering::Relaxed);
			}
			PlanningModeLabel::IterativeRefinement => {
				self.planning_iterative_refinement_total
					.fetch_add(1, Ordering::Relaxed);
			}
			PlanningModeLabel::Unknown => {}
		}
	}

	pub fn inc_approvals_created(&self) {
		self.approvals_created_total.fetch_add(1, Ordering::Relaxed);
	}

	pub fn inc_approvals_resolved(&self) {
		self.approvals_resolved_total
			.fetch_add(1, Ordering::Relaxed);
	}

	pub fn inc_dead_letters(&self) {
		self.dead_letters_total.fetch_add(1, Ordering::Relaxed);
	}

	pub fn inc_artifacts(&self) {
		self.artifacts_total.fetch_add(1, Ordering::Relaxed);
	}

	pub fn inc_experiments_started(&self) {
		self.experiments_started_total
			.fetch_add(1, Ordering::Relaxed);
	}

	pub fn inc_experiments_succeeded(&self) {
		self.experiments_succeeded_total
			.fetch_add(1, Ordering::Relaxed);
	}

	pub fn inc_experiments_failed(&self) {
		self.experiments_failed_total
			.fetch_add(1, Ordering::Relaxed);
	}

	pub fn snapshot(&self) -> MetricsSnapshot {
		MetricsSnapshot {
			requests_total: self.requests_total.load(Ordering::Relaxed),
			failures_total: self.failures_total.load(Ordering::Relaxed),
			validation_failures_total: self.validation_failures_total.load(Ordering::Relaxed),
			planning_runs_total: self.planning_runs_total.load(Ordering::Relaxed),
			planning_react_total: self.planning_react_total.load(Ordering::Relaxed),
			planning_task_decomposition_total: self
				.planning_task_decomposition_total
				.load(Ordering::Relaxed),
			planning_tree_search_total: self.planning_tree_search_total.load(Ordering::Relaxed),
			planning_iterative_refinement_total: self
				.planning_iterative_refinement_total
				.load(Ordering::Relaxed),
			approvals_created_total: self.approvals_created_total.load(Ordering::Relaxed),
			approvals_resolved_total: self.approvals_resolved_total.load(Ordering::Relaxed),
			dead_letters_total: self.dead_letters_total.load(Ordering::Relaxed),
			artifacts_total: self.artifacts_total.load(Ordering::Relaxed),
			experiments_started_total: self.experiments_started_total.load(Ordering::Relaxed),
			experiments_succeeded_total: self.experiments_succeeded_total.load(Ordering::Relaxed),
			experiments_failed_total: self.experiments_failed_total.load(Ordering::Relaxed),
		}
	}
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditRecord {
	pub actor: String,
	pub action: String,
	pub resource: String,
	pub outcome: String,
	#[serde(default)]
	pub correlation: Option<AuditCorrelation>,
	#[serde(default)]
	pub attributes: Vec<AuditAttribute>,
}

impl AuditRecord {
	pub fn new(
		actor: impl Into<String>,
		action: impl Into<String>,
		resource: impl Into<String>,
		outcome: impl Into<String>,
	) -> Self {
		Self {
			actor: actor.into(),
			action: action.into(),
			resource: resource.into(),
			outcome: outcome.into(),
			correlation: None,
			attributes: Vec::new(),
		}
	}

	pub fn with_correlation(mut self, correlation: AuditCorrelation) -> Self {
		self.correlation = Some(correlation);
		self
	}

	pub fn with_attribute(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
		self.attributes.push(AuditAttribute {
			key: key.into(),
			value: value.into(),
		});
		self
	}
}

pub trait AuditSink: Send + Sync {
	fn record(&self, record: AuditRecord) -> std::io::Result<()>;
}

#[derive(Debug, Default)]
pub struct InMemoryAuditSink {
	records: Mutex<Vec<AuditRecord>>,
}

impl InMemoryAuditSink {
	pub fn records(&self) -> Vec<AuditRecord> {
		self.records
			.lock()
			.expect("audit sink mutex should not be poisoned")
			.clone()
	}
}

impl AuditSink for InMemoryAuditSink {
	fn record(&self, record: AuditRecord) -> std::io::Result<()> {
		self.records
			.lock()
			.expect("audit sink mutex should not be poisoned")
			.push(record);
		Ok(())
	}
}

#[derive(Debug, Clone)]
pub struct JsonlAuditExporter {
	path: PathBuf,
}

impl JsonlAuditExporter {
	pub fn new(path: impl Into<PathBuf>) -> Self {
		Self { path: path.into() }
	}
}

impl AuditSink for JsonlAuditExporter {
	fn record(&self, record: AuditRecord) -> std::io::Result<()> {
		if let Some(parent) = self.path.parent() {
			std::fs::create_dir_all(parent)?;
		}

		let file = OpenOptions::new()
			.create(true)
			.append(true)
			.open(&self.path)?;
		let mut writer = BufWriter::new(file);
		let line = serde_json::to_string(&record)
			.map_err(|error| std::io::Error::other(error.to_string()))?;
		writer.write_all(line.as_bytes())?;
		writer.write_all(b"\n")?;
		writer.flush()?;
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::time::{SystemTime, UNIX_EPOCH};

	#[test]
	fn snapshot_tracks_counters() {
		let metrics = Metrics::default();
		metrics.inc_requests();
		metrics.inc_failures();
		metrics.inc_validation_failures();
		metrics.inc_planning_run();
		metrics.inc_planning_strategy("ReAct");
		metrics.inc_planning_strategy("TaskDecomposition");
		metrics.inc_planning_strategy("TreeSearch");
		metrics.inc_planning_strategy("IterativeRefinement");
		metrics.inc_approvals_created();
		metrics.inc_approvals_resolved();
		metrics.inc_dead_letters();
		metrics.inc_artifacts();
		metrics.inc_experiments_started();
		metrics.inc_experiments_succeeded();
		metrics.inc_experiments_failed();

		let snapshot = metrics.snapshot();
		assert_eq!(snapshot.requests_total, 1);
		assert_eq!(snapshot.failures_total, 1);
		assert_eq!(snapshot.validation_failures_total, 1);
		assert_eq!(snapshot.planning_runs_total, 1);
		assert_eq!(snapshot.planning_react_total, 1);
		assert_eq!(snapshot.planning_task_decomposition_total, 1);
		assert_eq!(snapshot.planning_tree_search_total, 1);
		assert_eq!(snapshot.planning_iterative_refinement_total, 1);
		assert_eq!(snapshot.approvals_created_total, 1);
		assert_eq!(snapshot.approvals_resolved_total, 1);
		assert_eq!(snapshot.dead_letters_total, 1);
		assert_eq!(snapshot.artifacts_total, 1);
		assert_eq!(snapshot.experiments_started_total, 1);
		assert_eq!(snapshot.experiments_succeeded_total, 1);
		assert_eq!(snapshot.experiments_failed_total, 1);
	}

	#[test]
	fn in_memory_audit_sink_stores_records() {
		let sink = InMemoryAuditSink::default();
		sink.record(
			AuditRecord::new("agent", "invoke", "tool.echo", "ok").with_correlation(
				AuditCorrelation {
					trace_id: "trace-1".to_string(),
					span_id: "span-1".to_string(),
					task_id: Some("task-1".to_string()),
					request_id: Some("request-1".to_string()),
				},
			),
		)
		.expect("recording should succeed");

		let records = sink.records();
		assert_eq!(records.len(), 1);
		assert_eq!(
			records[0]
				.correlation
				.as_ref()
				.expect("correlation should exist")
				.trace_id,
			"trace-1"
		);
	}

	#[test]
	fn jsonl_exporter_writes_lines() {
		let nanos = SystemTime::now()
			.duration_since(UNIX_EPOCH)
			.expect("clock should be after epoch")
			.as_nanos();
		let path = std::env::temp_dir().join(format!("roku-audit-{nanos}.jsonl"));
		let exporter = JsonlAuditExporter::new(path.clone());
		exporter
			.record(
				AuditRecord::new("agent", "validate", "result", "ok")
					.with_attribute("validator", "semantic"),
			)
			.expect("export should succeed");

		let content = std::fs::read_to_string(&path).expect("file should be readable");
		assert!(content.contains("\"actor\":\"agent\""));
		assert!(content.contains("\"validator\""));
		let _ = std::fs::remove_file(path);
	}
}
