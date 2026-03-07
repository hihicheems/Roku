//! Gateway request normalization.

use roku_common_types::{RequestEnvelope, RequestId};

#[derive(Debug, Clone)]
pub struct RawRequest {
	pub session_id: String,
	pub goal: String,
}

#[derive(Debug, Default)]
pub struct Gateway;

impl Gateway {
	pub fn normalize(&self, raw: RawRequest, seq: u64) -> RequestEnvelope {
		RequestEnvelope {
			request_id: RequestId(format!("req-{seq}")),
			session_id: raw.session_id,
			goal: raw.goal,
		}
	}
}
