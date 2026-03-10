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

//! Observability primitives and lightweight exporters.

mod logging;

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

pub use logging::{
	AsyncRotatingFileLogSink, FanoutLogSink, FileLogConfig, LogField, LogLevel, LogRecord, LogSink,
	StderrLogSink, emit_global_log, install_global_log_sink,
};

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
	pub direct_route_hits_total: AtomicU64,
	pub direct_route_fallback_total: AtomicU64,
	pub route_classifier_failures_total: AtomicU64,
	pub route_parse_guard_failures_total: AtomicU64,
	pub route_escalations_total: AtomicU64,
	pub route_limited_planning_total: AtomicU64,
	pub llm_requests_total: AtomicU64,
	pub llm_successes_total: AtomicU64,
	pub llm_failures_total: AtomicU64,
	pub llm_routing_failures_total: AtomicU64,
	pub llm_prompt_tokens_total: AtomicU64,
	pub llm_output_tokens_total: AtomicU64,
	pub llm_latency_ms_total: AtomicU64,
	pub llm_estimated_cost_microusd_total: AtomicU64,
	llm_provider_metrics: Mutex<HashMap<String, LlmProviderMetrics>>,
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
			direct_route_hits_total: AtomicU64::new(0),
			direct_route_fallback_total: AtomicU64::new(0),
			route_classifier_failures_total: AtomicU64::new(0),
			route_parse_guard_failures_total: AtomicU64::new(0),
			route_escalations_total: AtomicU64::new(0),
			route_limited_planning_total: AtomicU64::new(0),
			llm_requests_total: AtomicU64::new(0),
			llm_successes_total: AtomicU64::new(0),
			llm_failures_total: AtomicU64::new(0),
			llm_routing_failures_total: AtomicU64::new(0),
			llm_prompt_tokens_total: AtomicU64::new(0),
			llm_output_tokens_total: AtomicU64::new(0),
			llm_latency_ms_total: AtomicU64::new(0),
			llm_estimated_cost_microusd_total: AtomicU64::new(0),
			llm_provider_metrics: Mutex::new(HashMap::new()),
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
	pub direct_route_hits_total: u64,
	pub direct_route_fallback_total: u64,
	pub route_classifier_failures_total: u64,
	pub route_parse_guard_failures_total: u64,
	pub route_escalations_total: u64,
	pub route_limited_planning_total: u64,
	pub llm_requests_total: u64,
	pub llm_successes_total: u64,
	pub llm_failures_total: u64,
	pub llm_routing_failures_total: u64,
	pub llm_prompt_tokens_total: u64,
	pub llm_output_tokens_total: u64,
	pub llm_latency_ms_total: u64,
	pub llm_estimated_cost_microusd_total: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmProviderMetricsSnapshot {
	pub provider: String,
	pub model_id: String,
	pub requests_total: u64,
	pub successes_total: u64,
	pub failures_total: u64,
	pub prompt_tokens_total: u64,
	pub output_tokens_total: u64,
	pub latency_ms_total: u64,
	pub estimated_cost_microusd_total: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmInvocationOutcome {
	Success,
	Failure,
}

#[derive(Debug, Default)]
struct LlmProviderMetrics {
	requests_total: u64,
	successes_total: u64,
	failures_total: u64,
	prompt_tokens_total: u64,
	output_tokens_total: u64,
	latency_ms_total: u64,
	estimated_cost_microusd_total: u64,
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

	pub fn inc_direct_route_hits(&self) {
		self.direct_route_hits_total.fetch_add(1, Ordering::Relaxed);
	}

	pub fn inc_direct_route_fallbacks(&self) {
		self.direct_route_fallback_total
			.fetch_add(1, Ordering::Relaxed);
	}

	pub fn inc_route_classifier_failures(&self) {
		self.route_classifier_failures_total
			.fetch_add(1, Ordering::Relaxed);
	}

	pub fn inc_route_parse_guard_failures(&self) {
		self.route_parse_guard_failures_total
			.fetch_add(1, Ordering::Relaxed);
	}

	pub fn inc_route_escalations(&self) {
		self.route_escalations_total.fetch_add(1, Ordering::Relaxed);
	}

	pub fn inc_route_limited_planning(&self) {
		self.route_limited_planning_total
			.fetch_add(1, Ordering::Relaxed);
	}

	pub fn record_llm_routing_failure(&self) {
		self.llm_requests_total.fetch_add(1, Ordering::Relaxed);
		self.llm_failures_total.fetch_add(1, Ordering::Relaxed);
		self.llm_routing_failures_total
			.fetch_add(1, Ordering::Relaxed);
	}

	pub fn record_llm_invocation(
		&self,
		provider: &str,
		model_id: &str,
		outcome: LlmInvocationOutcome,
		prompt_tokens: u64,
		output_tokens: u64,
		latency_ms: u64,
		estimated_cost_usd: f64,
	) {
		self.llm_requests_total.fetch_add(1, Ordering::Relaxed);
		match outcome {
			LlmInvocationOutcome::Success => {
				self.llm_successes_total.fetch_add(1, Ordering::Relaxed);
			}
			LlmInvocationOutcome::Failure => {
				self.llm_failures_total.fetch_add(1, Ordering::Relaxed);
			}
		}
		self.llm_prompt_tokens_total
			.fetch_add(prompt_tokens, Ordering::Relaxed);
		self.llm_output_tokens_total
			.fetch_add(output_tokens, Ordering::Relaxed);
		self.llm_latency_ms_total
			.fetch_add(latency_ms, Ordering::Relaxed);
		let estimated_cost_microusd = usd_to_microusd(estimated_cost_usd);
		self.llm_estimated_cost_microusd_total
			.fetch_add(estimated_cost_microusd, Ordering::Relaxed);

		let mut provider_metrics = self
			.llm_provider_metrics
			.lock()
			.expect("llm provider metrics mutex should not be poisoned");
		let entry = provider_metrics
			.entry(format!("{provider}/{model_id}"))
			.or_default();
		entry.requests_total = entry.requests_total.saturating_add(1);
		match outcome {
			LlmInvocationOutcome::Success => {
				entry.successes_total = entry.successes_total.saturating_add(1);
			}
			LlmInvocationOutcome::Failure => {
				entry.failures_total = entry.failures_total.saturating_add(1);
			}
		}
		entry.prompt_tokens_total = entry.prompt_tokens_total.saturating_add(prompt_tokens);
		entry.output_tokens_total = entry.output_tokens_total.saturating_add(output_tokens);
		entry.latency_ms_total = entry.latency_ms_total.saturating_add(latency_ms);
		entry.estimated_cost_microusd_total = entry
			.estimated_cost_microusd_total
			.saturating_add(estimated_cost_microusd);
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
			direct_route_hits_total: self.direct_route_hits_total.load(Ordering::Relaxed),
			direct_route_fallback_total: self.direct_route_fallback_total.load(Ordering::Relaxed),
			route_classifier_failures_total: self
				.route_classifier_failures_total
				.load(Ordering::Relaxed),
			route_parse_guard_failures_total: self
				.route_parse_guard_failures_total
				.load(Ordering::Relaxed),
			route_escalations_total: self.route_escalations_total.load(Ordering::Relaxed),
			route_limited_planning_total: self.route_limited_planning_total.load(Ordering::Relaxed),
			llm_requests_total: self.llm_requests_total.load(Ordering::Relaxed),
			llm_successes_total: self.llm_successes_total.load(Ordering::Relaxed),
			llm_failures_total: self.llm_failures_total.load(Ordering::Relaxed),
			llm_routing_failures_total: self.llm_routing_failures_total.load(Ordering::Relaxed),
			llm_prompt_tokens_total: self.llm_prompt_tokens_total.load(Ordering::Relaxed),
			llm_output_tokens_total: self.llm_output_tokens_total.load(Ordering::Relaxed),
			llm_latency_ms_total: self.llm_latency_ms_total.load(Ordering::Relaxed),
			llm_estimated_cost_microusd_total: self
				.llm_estimated_cost_microusd_total
				.load(Ordering::Relaxed),
		}
	}

	pub fn llm_provider_metrics(&self) -> Vec<LlmProviderMetricsSnapshot> {
		let provider_metrics = self
			.llm_provider_metrics
			.lock()
			.expect("llm provider metrics mutex should not be poisoned");
		let mut snapshots = provider_metrics
			.iter()
			.map(|(key, value)| {
				let mut parts = key.splitn(2, '/');
				let provider = parts.next().unwrap_or_default().to_string();
				let model_id = parts.next().unwrap_or_default().to_string();
				LlmProviderMetricsSnapshot {
					provider,
					model_id,
					requests_total: value.requests_total,
					successes_total: value.successes_total,
					failures_total: value.failures_total,
					prompt_tokens_total: value.prompt_tokens_total,
					output_tokens_total: value.output_tokens_total,
					latency_ms_total: value.latency_ms_total,
					estimated_cost_microusd_total: value.estimated_cost_microusd_total,
				}
			})
			.collect::<Vec<_>>();
		snapshots.sort_by(|left, right| {
			left.provider
				.cmp(&right.provider)
				.then_with(|| left.model_id.cmp(&right.model_id))
		});
		snapshots
	}
}

fn usd_to_microusd(amount_usd: f64) -> u64 {
	if !amount_usd.is_finite() || amount_usd <= 0.0 {
		return 0;
	}

	let micros = (amount_usd * 1_000_000.0).round();
	if micros >= u64::MAX as f64 {
		return u64::MAX;
	}

	micros as u64
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
		metrics.record_llm_invocation(
			"openrouter",
			"openrouter/free",
			LlmInvocationOutcome::Success,
			120,
			30,
			450,
			0.0025,
		);
		metrics.record_llm_routing_failure();

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
		assert_eq!(snapshot.llm_requests_total, 2);
		assert_eq!(snapshot.llm_successes_total, 1);
		assert_eq!(snapshot.llm_failures_total, 1);
		assert_eq!(snapshot.llm_routing_failures_total, 1);
		assert_eq!(snapshot.llm_prompt_tokens_total, 120);
		assert_eq!(snapshot.llm_output_tokens_total, 30);
		assert_eq!(snapshot.llm_latency_ms_total, 450);
		assert_eq!(snapshot.llm_estimated_cost_microusd_total, 2_500);

		let provider_metrics = metrics.llm_provider_metrics();
		assert_eq!(provider_metrics.len(), 1);
		assert_eq!(provider_metrics[0].provider, "openrouter");
		assert_eq!(provider_metrics[0].model_id, "openrouter/free");
		assert_eq!(provider_metrics[0].successes_total, 1);
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
