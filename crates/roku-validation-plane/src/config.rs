#[derive(Debug, Clone)]
pub struct ValidationConfig {
	pub require_evidence: bool,
	pub min_confidence: f32,
	pub require_non_empty_producer: bool,
	pub enable_cross_checks: bool,
}

impl Default for ValidationConfig {
	fn default() -> Self {
		Self {
			require_evidence: true,
			min_confidence: 0.1,
			require_non_empty_producer: true,
			enable_cross_checks: true,
		}
	}
}
