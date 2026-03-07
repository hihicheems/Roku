//! Minimal observability primitives.

use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Clone)]
pub struct TraceContext {
	pub trace_id: String,
	pub span_id: String,
}

#[derive(Debug)]
pub struct Metrics {
	pub requests_total: AtomicU64,
	pub failures_total: AtomicU64,
}

impl Default for Metrics {
	fn default() -> Self {
		Self {
			requests_total: AtomicU64::new(0),
			failures_total: AtomicU64::new(0),
		}
	}
}

impl Metrics {
	pub fn inc_requests(&self) {
		self.requests_total.fetch_add(1, Ordering::Relaxed);
	}

	pub fn inc_failures(&self) {
		self.failures_total.fetch_add(1, Ordering::Relaxed);
	}
}

#[derive(Debug, Clone)]
pub struct AuditRecord {
	pub actor: String,
	pub action: String,
	pub resource: String,
	pub outcome: String,
}
