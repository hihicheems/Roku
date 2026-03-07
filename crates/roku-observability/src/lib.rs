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

#[derive(Debug)]
pub struct Metrics {
	pub requests_total: AtomicU64,
	pub failures_total: AtomicU64,
	pub validation_failures_total: AtomicU64,
}

impl Default for Metrics {
	fn default() -> Self {
		Self {
			requests_total: AtomicU64::new(0),
			failures_total: AtomicU64::new(0),
			validation_failures_total: AtomicU64::new(0),
		}
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetricsSnapshot {
	pub requests_total: u64,
	pub failures_total: u64,
	pub validation_failures_total: u64,
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

	pub fn snapshot(&self) -> MetricsSnapshot {
		MetricsSnapshot {
			requests_total: self.requests_total.load(Ordering::Relaxed),
			failures_total: self.failures_total.load(Ordering::Relaxed),
			validation_failures_total: self.validation_failures_total.load(Ordering::Relaxed),
		}
	}
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditRecord {
	pub actor: String,
	pub action: String,
	pub resource: String,
	pub outcome: String,
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

		let snapshot = metrics.snapshot();
		assert_eq!(snapshot.requests_total, 1);
		assert_eq!(snapshot.failures_total, 1);
		assert_eq!(snapshot.validation_failures_total, 1);
	}

	#[test]
	fn in_memory_audit_sink_stores_records() {
		let sink = InMemoryAuditSink::default();
		sink.record(AuditRecord {
			actor: "agent".to_string(),
			action: "invoke".to_string(),
			resource: "tool.echo".to_string(),
			outcome: "ok".to_string(),
		})
		.expect("recording should succeed");

		assert_eq!(sink.records().len(), 1);
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
			.record(AuditRecord {
				actor: "agent".to_string(),
				action: "validate".to_string(),
				resource: "result".to_string(),
				outcome: "ok".to_string(),
			})
			.expect("export should succeed");

		let content = std::fs::read_to_string(&path).expect("file should be readable");
		assert!(content.contains("\"actor\":\"agent\""));
		let _ = std::fs::remove_file(path);
	}
}
