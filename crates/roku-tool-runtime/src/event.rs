use crate::SandboxProfile;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionEventKind {
	Dispatched,
	AttemptStarted,
	AttemptFailed,
	Retrying,
	Succeeded,
	TimedOut,
	Rejected,
}

impl ExecutionEventKind {
	pub(crate) fn as_str(&self) -> &'static str {
		match self {
			Self::Dispatched => "dispatched",
			Self::AttemptStarted => "attempt_started",
			Self::AttemptFailed => "attempt_failed",
			Self::Retrying => "retrying",
			Self::Succeeded => "succeeded",
			Self::TimedOut => "timed_out",
			Self::Rejected => "rejected",
		}
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionEvent {
	pub trace_id: String,
	pub invocation_key: String,
	pub tool_name: String,
	pub kind: ExecutionEventKind,
	pub attempt: u8,
	pub sandbox_profile: SandboxProfile,
	pub fingerprint: Option<String>,
	pub message: Option<String>,
}

pub trait ExecutionHook: Send + Sync {
	fn on_event(&self, event: &ExecutionEvent);
}
