//! Tool runtime with descriptor-based dispatch and execution policies.

use std::collections::HashMap;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SandboxProfile {
	#[default]
	NoIsolation,
	ReadOnlyFs,
	PythonResearch,
	ContainerRestricted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ToolSchema {
	pub required_fields: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeConstraints {
	pub timeout_ms: u64,
	pub max_retries: u8,
	pub retry_backoff_ms: u64,
	pub sandbox_profile: SandboxProfile,
	pub deterministic_hooks: bool,
}

impl RuntimeConstraints {
	pub fn max_attempts(&self) -> u8 {
		self.max_retries.saturating_add(1)
	}
}

impl Default for RuntimeConstraints {
	fn default() -> Self {
		Self {
			timeout_ms: 30_000,
			max_retries: 0,
			retry_backoff_ms: 0,
			sandbox_profile: SandboxProfile::NoIsolation,
			deterministic_hooks: true,
		}
	}
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolDescriptor {
	pub name: String,
	pub version: String,
	pub input_schema: ToolSchema,
	pub output_schema: String,
	pub required_capabilities: Vec<String>,
	pub runtime_constraints: RuntimeConstraints,
}

impl ToolDescriptor {
	fn validate(&self) -> Result<(), ToolRuntimeError> {
		if self.name.trim().is_empty() {
			return Err(ToolRuntimeError::InvalidDescriptor(
				"descriptor name cannot be empty".to_string(),
			));
		}
		if self.version.trim().is_empty() {
			return Err(ToolRuntimeError::InvalidDescriptor(
				"descriptor version cannot be empty".to_string(),
			));
		}
		if self.runtime_constraints.timeout_ms == 0 {
			return Err(ToolRuntimeError::InvalidDescriptor(
				"timeout_ms must be greater than zero".to_string(),
			));
		}
		Ok(())
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolInvocation {
	pub tool_name: String,
	pub input: Value,
	pub granted_capabilities: Vec<String>,
	pub invocation_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolInvocationRequest {
	pub invocation_key: String,
	pub attempt: u8,
	pub input: Value,
	pub sandbox_profile: SandboxProfile,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolExecutionResult {
	pub tool_name: String,
	pub output: Value,
	pub output_fingerprint: String,
	pub attempts: u8,
	pub elapsed_ms: u128,
	pub sandbox_profile: SandboxProfile,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolFailure {
	pub message: String,
	pub retriable: bool,
}

impl ToolFailure {
	pub fn retriable(message: impl Into<String>) -> Self {
		Self {
			message: message.into(),
			retriable: true,
		}
	}

	pub fn terminal(message: impl Into<String>) -> Self {
		Self {
			message: message.into(),
			retriable: false,
		}
	}
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ToolRuntimeError {
	#[error("tool not found: {0}")]
	ToolNotFound(String),
	#[error("tool already registered: {0}")]
	ToolAlreadyRegistered(String),
	#[error("invalid descriptor: {0}")]
	InvalidDescriptor(String),
	#[error("input schema violation for {tool}: missing required fields {missing_fields:?}")]
	InputSchemaViolation {
		tool: String,
		missing_fields: Vec<String>,
	},
	#[error("capability denied for {tool}: missing {missing_capabilities:?}")]
	CapabilityDenied {
		tool: String,
		missing_capabilities: Vec<String>,
	},
	#[error("tool timed out: {tool} exceeded {timeout_ms}ms (elapsed {elapsed_ms}ms)")]
	Timeout {
		tool: String,
		timeout_ms: u64,
		elapsed_ms: u128,
	},
	#[error("tool execution failed: {tool} after {attempts} attempts: {message}")]
	ExecutionFailed {
		tool: String,
		attempts: u8,
		message: String,
		retriable: bool,
	},
}

pub trait Tool: Send + Sync {
	fn descriptor(&self) -> ToolDescriptor;
	fn invoke(&self, request: ToolInvocationRequest) -> Result<Value, ToolFailure>;
}

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
	fn as_str(&self) -> &'static str {
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

struct RegisteredTool {
	descriptor: ToolDescriptor,
	tool: Arc<dyn Tool>,
}

#[derive(Default)]
pub struct ToolRuntime {
	tools: HashMap<String, RegisteredTool>,
	hooks: Vec<Arc<dyn ExecutionHook>>,
}

impl ToolRuntime {
	pub fn register_tool<T>(&mut self, tool: T) -> Result<(), ToolRuntimeError>
	where
		T: Tool + 'static,
	{
		let descriptor = tool.descriptor();
		descriptor.validate()?;

		if self.tools.contains_key(&descriptor.name) {
			return Err(ToolRuntimeError::ToolAlreadyRegistered(descriptor.name));
		}

		self.tools.insert(
			descriptor.name.clone(),
			RegisteredTool {
				descriptor,
				tool: Arc::new(tool),
			},
		);
		Ok(())
	}

	pub fn register_hook(&mut self, hook: Arc<dyn ExecutionHook>) {
		self.hooks.push(hook);
	}

	pub fn invoke(
		&self,
		invocation: ToolInvocation,
	) -> Result<ToolExecutionResult, ToolRuntimeError> {
		let registered = self
			.tools
			.get(&invocation.tool_name)
			.ok_or_else(|| ToolRuntimeError::ToolNotFound(invocation.tool_name.clone()))?;

		validate_input(
			&invocation.input,
			&registered.descriptor.input_schema,
			&invocation.tool_name,
		)?;

		let missing_capabilities = missing_capabilities(
			&registered.descriptor.required_capabilities,
			&invocation.granted_capabilities,
		);
		if !missing_capabilities.is_empty() {
			let invocation_key = invocation.invocation_key.clone().unwrap_or_else(|| {
				default_invocation_key(&invocation.tool_name, &invocation.input)
			});
			self.emit_event(
				&invocation_key,
				&invocation.tool_name,
				ExecutionEventKind::Rejected,
				0,
				&registered.descriptor.runtime_constraints.sandbox_profile,
				None,
				Some(format!(
					"missing capabilities: {}",
					missing_capabilities.join(",")
				)),
			);
			return Err(ToolRuntimeError::CapabilityDenied {
				tool: invocation.tool_name,
				missing_capabilities,
			});
		}

		let invocation_key = invocation
			.invocation_key
			.clone()
			.unwrap_or_else(|| default_invocation_key(&invocation.tool_name, &invocation.input));
		self.emit_event(
			&invocation_key,
			&invocation.tool_name,
			ExecutionEventKind::Dispatched,
			0,
			&registered.descriptor.runtime_constraints.sandbox_profile,
			None,
			None,
		);

		let max_attempts = registered.descriptor.runtime_constraints.max_attempts();
		for attempt in 1..=max_attempts {
			let sandbox_profile = &registered.descriptor.runtime_constraints.sandbox_profile;
			self.emit_event(
				&invocation_key,
				&invocation.tool_name,
				ExecutionEventKind::AttemptStarted,
				attempt,
				sandbox_profile,
				None,
				None,
			);

			let request = ToolInvocationRequest {
				invocation_key: invocation_key.clone(),
				attempt,
				input: invocation.input.clone(),
				sandbox_profile: sandbox_profile.clone(),
			};
			let started_at = Instant::now();
			let invocation_outcome = registered.tool.invoke(request);
			let elapsed_ms = started_at.elapsed().as_millis();

			if elapsed_ms > u128::from(registered.descriptor.runtime_constraints.timeout_ms) {
				self.emit_event(
					&invocation_key,
					&invocation.tool_name,
					ExecutionEventKind::TimedOut,
					attempt,
					sandbox_profile,
					None,
					Some(format!("elapsed={}ms", elapsed_ms)),
				);
				if attempt < max_attempts {
					self.emit_event(
						&invocation_key,
						&invocation.tool_name,
						ExecutionEventKind::Retrying,
						attempt,
						sandbox_profile,
						None,
						Some("retrying after timeout".to_string()),
					);
					apply_retry_backoff(registered.descriptor.runtime_constraints.retry_backoff_ms);
					continue;
				}

				return Err(ToolRuntimeError::Timeout {
					tool: invocation.tool_name,
					timeout_ms: registered.descriptor.runtime_constraints.timeout_ms,
					elapsed_ms,
				});
			}

			match invocation_outcome {
				Ok(output) => {
					let output_fingerprint = fingerprint_json(&output);
					let fingerprint = registered
						.descriptor
						.runtime_constraints
						.deterministic_hooks
						.then(|| output_fingerprint.clone());
					self.emit_event(
						&invocation_key,
						&invocation.tool_name,
						ExecutionEventKind::Succeeded,
						attempt,
						sandbox_profile,
						fingerprint,
						None,
					);

					return Ok(ToolExecutionResult {
						tool_name: invocation.tool_name,
						output,
						output_fingerprint,
						attempts: attempt,
						elapsed_ms,
						sandbox_profile: sandbox_profile.clone(),
					});
				}
				Err(failure) => {
					self.emit_event(
						&invocation_key,
						&invocation.tool_name,
						ExecutionEventKind::AttemptFailed,
						attempt,
						sandbox_profile,
						None,
						Some(failure.message.clone()),
					);

					if failure.retriable && attempt < max_attempts {
						self.emit_event(
							&invocation_key,
							&invocation.tool_name,
							ExecutionEventKind::Retrying,
							attempt,
							sandbox_profile,
							None,
							Some("retrying after retriable error".to_string()),
						);
						apply_retry_backoff(
							registered.descriptor.runtime_constraints.retry_backoff_ms,
						);
						continue;
					}

					return Err(ToolRuntimeError::ExecutionFailed {
						tool: invocation.tool_name,
						attempts: attempt,
						message: failure.message,
						retriable: failure.retriable,
					});
				}
			}
		}

		Err(ToolRuntimeError::ExecutionFailed {
			tool: invocation.tool_name,
			attempts: max_attempts,
			message: "execution exhausted without result".to_string(),
			retriable: false,
		})
	}

	fn emit_event(
		&self,
		invocation_key: &str,
		tool_name: &str,
		kind: ExecutionEventKind,
		attempt: u8,
		sandbox_profile: &SandboxProfile,
		fingerprint: Option<String>,
		message: Option<String>,
	) {
		let trace_id = format!("{invocation_key}:{attempt}:{}", kind.as_str());
		let event = ExecutionEvent {
			trace_id,
			invocation_key: invocation_key.to_string(),
			tool_name: tool_name.to_string(),
			kind,
			attempt,
			sandbox_profile: sandbox_profile.clone(),
			fingerprint,
			message,
		};
		for hook in &self.hooks {
			hook.on_event(&event);
		}
	}
}

fn apply_retry_backoff(backoff_ms: u64) {
	if backoff_ms > 0 {
		thread::sleep(Duration::from_millis(backoff_ms));
	}
}

fn validate_input(
	input: &Value,
	schema: &ToolSchema,
	tool_name: &str,
) -> Result<(), ToolRuntimeError> {
	let Some(map) = input.as_object() else {
		return Err(ToolRuntimeError::InputSchemaViolation {
			tool: tool_name.to_string(),
			missing_fields: schema.required_fields.clone(),
		});
	};

	let mut missing = Vec::new();
	for field in &schema.required_fields {
		if !map.contains_key(field) {
			missing.push(field.clone());
		}
	}

	if missing.is_empty() {
		Ok(())
	} else {
		Err(ToolRuntimeError::InputSchemaViolation {
			tool: tool_name.to_string(),
			missing_fields: missing,
		})
	}
}

fn missing_capabilities(required: &[String], granted: &[String]) -> Vec<String> {
	required
		.iter()
		.filter(|capability| !granted.contains(capability))
		.cloned()
		.collect()
}

fn default_invocation_key(tool_name: &str, input: &Value) -> String {
	format!("{tool_name}:{}", fingerprint_json(input))
}

fn fingerprint_json(value: &Value) -> String {
	let bytes = match serde_json::to_vec(value) {
		Ok(bytes) => bytes,
		Err(_) => b"serialization-error".to_vec(),
	};
	format!("{:016x}", fnv1a64(&bytes))
}

fn fnv1a64(bytes: &[u8]) -> u64 {
	const OFFSET_BASIS: u64 = 0xcbf29ce484222325;
	const PRIME: u64 = 0x100000001b3;

	let mut hash = OFFSET_BASIS;
	for byte in bytes {
		hash ^= u64::from(*byte);
		hash = hash.wrapping_mul(PRIME);
	}
	hash
}

#[cfg(test)]
mod tests {
	use std::sync::{Arc, Mutex};
	use std::thread;
	use std::time::Duration;

	use serde_json::json;

	use super::*;

	#[derive(Clone)]
	struct EchoJsonTool {
		descriptor: ToolDescriptor,
	}

	impl EchoJsonTool {
		fn new(
			required_capabilities: Vec<String>,
			runtime_constraints: RuntimeConstraints,
		) -> Self {
			Self {
				descriptor: ToolDescriptor {
					name: "echo-json".to_string(),
					version: "1.0.0".to_string(),
					input_schema: ToolSchema {
						required_fields: vec!["text".to_string()],
					},
					output_schema: "echo.v1".to_string(),
					required_capabilities,
					runtime_constraints,
				},
			}
		}
	}

	impl Tool for EchoJsonTool {
		fn descriptor(&self) -> ToolDescriptor {
			self.descriptor.clone()
		}

		fn invoke(&self, request: ToolInvocationRequest) -> Result<Value, ToolFailure> {
			Ok(request.input)
		}
	}

	struct FlakyTool {
		descriptor: ToolDescriptor,
		failures_left: Mutex<u8>,
	}

	impl FlakyTool {
		fn new(failures_left: u8) -> Self {
			Self {
				descriptor: ToolDescriptor {
					name: "flaky".to_string(),
					version: "1.0.0".to_string(),
					input_schema: ToolSchema::default(),
					output_schema: "flaky.v1".to_string(),
					required_capabilities: Vec::new(),
					runtime_constraints: RuntimeConstraints {
						timeout_ms: 1_000,
						max_retries: 3,
						retry_backoff_ms: 0,
						sandbox_profile: SandboxProfile::NoIsolation,
						deterministic_hooks: true,
					},
				},
				failures_left: Mutex::new(failures_left),
			}
		}
	}

	impl Tool for FlakyTool {
		fn descriptor(&self) -> ToolDescriptor {
			self.descriptor.clone()
		}

		fn invoke(&self, request: ToolInvocationRequest) -> Result<Value, ToolFailure> {
			let mut failures_left = self.failures_left.lock().expect("poisoned lock");
			if *failures_left > 0 {
				*failures_left -= 1;
				return Err(ToolFailure::retriable(format!(
					"temporary failure on attempt {}",
					request.attempt
				)));
			}
			Ok(json!({"status": "ok", "attempt": request.attempt}))
		}
	}

	struct SlowTool {
		descriptor: ToolDescriptor,
		sleep_ms: u64,
	}

	impl SlowTool {
		fn new(timeout_ms: u64, sleep_ms: u64) -> Self {
			Self {
				descriptor: ToolDescriptor {
					name: "slow".to_string(),
					version: "1.0.0".to_string(),
					input_schema: ToolSchema::default(),
					output_schema: "slow.v1".to_string(),
					required_capabilities: Vec::new(),
					runtime_constraints: RuntimeConstraints {
						timeout_ms,
						max_retries: 0,
						retry_backoff_ms: 0,
						sandbox_profile: SandboxProfile::ContainerRestricted,
						deterministic_hooks: true,
					},
				},
				sleep_ms,
			}
		}
	}

	impl Tool for SlowTool {
		fn descriptor(&self) -> ToolDescriptor {
			self.descriptor.clone()
		}

		fn invoke(&self, _request: ToolInvocationRequest) -> Result<Value, ToolFailure> {
			thread::sleep(Duration::from_millis(self.sleep_ms));
			Ok(json!({"status":"late"}))
		}
	}

	#[derive(Default)]
	struct RecordingHook {
		events: Mutex<Vec<ExecutionEvent>>,
	}

	impl RecordingHook {
		fn events(&self) -> Vec<ExecutionEvent> {
			self.events.lock().expect("poisoned lock").clone()
		}
	}

	impl ExecutionHook for RecordingHook {
		fn on_event(&self, event: &ExecutionEvent) {
			self.events
				.lock()
				.expect("poisoned lock")
				.push(event.clone());
		}
	}

	#[test]
	fn invoke_registered_tool_with_descriptor_constraints() {
		let mut runtime = ToolRuntime::default();
		let tool = EchoJsonTool::new(
			vec!["artifact:read:dataset/*".to_string()],
			RuntimeConstraints {
				timeout_ms: 1_000,
				max_retries: 0,
				retry_backoff_ms: 0,
				sandbox_profile: SandboxProfile::ReadOnlyFs,
				deterministic_hooks: true,
			},
		);
		runtime.register_tool(tool).expect("register tool");

		let result = runtime
			.invoke(ToolInvocation {
				tool_name: "echo-json".to_string(),
				input: json!({"text":"hello"}),
				granted_capabilities: vec!["artifact:read:dataset/*".to_string()],
				invocation_key: None,
			})
			.expect("invoke tool");

		assert_eq!(result.attempts, 1);
		assert_eq!(result.sandbox_profile, SandboxProfile::ReadOnlyFs);
		assert_eq!(result.output["text"], "hello");
		assert!(!result.output_fingerprint.is_empty());
	}

	#[test]
	fn reject_when_capability_is_missing() {
		let mut runtime = ToolRuntime::default();
		let hook = Arc::new(RecordingHook::default());
		runtime.register_hook(hook.clone());
		runtime
			.register_tool(EchoJsonTool::new(
				vec!["artifact:read:dataset/*".to_string()],
				RuntimeConstraints::default(),
			))
			.expect("register tool");

		let error = runtime
			.invoke(ToolInvocation {
				tool_name: "echo-json".to_string(),
				input: json!({"text":"hello"}),
				granted_capabilities: Vec::new(),
				invocation_key: Some("cap-denied".to_string()),
			})
			.expect_err("expected capability denied");

		match error {
			ToolRuntimeError::CapabilityDenied {
				tool,
				missing_capabilities,
			} => {
				assert_eq!(tool, "echo-json");
				assert_eq!(
					missing_capabilities,
					vec!["artifact:read:dataset/*".to_string()]
				);
			}
			other => panic!("unexpected error: {other:?}"),
		}

		let events = hook.events();
		assert_eq!(events.len(), 1);
		assert_eq!(events[0].kind, ExecutionEventKind::Rejected);
		assert_eq!(events[0].trace_id, "cap-denied:0:rejected");
	}

	#[test]
	fn retry_retriable_failure_then_succeed() {
		let mut runtime = ToolRuntime::default();
		let hook = Arc::new(RecordingHook::default());
		runtime.register_hook(hook.clone());
		runtime
			.register_tool(FlakyTool::new(2))
			.expect("register flaky tool");

		let result = runtime
			.invoke(ToolInvocation {
				tool_name: "flaky".to_string(),
				input: json!({}),
				granted_capabilities: Vec::new(),
				invocation_key: Some("flaky-invoke".to_string()),
			})
			.expect("invoke flaky tool");
		assert_eq!(result.attempts, 3);
		assert_eq!(result.output["status"], "ok");

		let events = hook.events();
		let retry_count = events
			.iter()
			.filter(|event| event.kind == ExecutionEventKind::Retrying)
			.count();
		assert_eq!(retry_count, 2);
	}

	#[test]
	fn timeout_is_reported_when_execution_exceeds_budget() {
		let mut runtime = ToolRuntime::default();
		runtime
			.register_tool(SlowTool::new(1, 15))
			.expect("register slow tool");

		let error = runtime
			.invoke(ToolInvocation {
				tool_name: "slow".to_string(),
				input: json!({}),
				granted_capabilities: Vec::new(),
				invocation_key: Some("slow-invoke".to_string()),
			})
			.expect_err("timeout expected");

		match error {
			ToolRuntimeError::Timeout {
				tool,
				timeout_ms,
				elapsed_ms,
			} => {
				assert_eq!(tool, "slow");
				assert_eq!(timeout_ms, 1);
				assert!(elapsed_ms >= 15);
			}
			other => panic!("unexpected error: {other:?}"),
		}
	}

	#[test]
	fn deterministic_hook_trace_ids_follow_stable_order() {
		let mut runtime = ToolRuntime::default();
		let hook = Arc::new(RecordingHook::default());
		runtime.register_hook(hook.clone());
		runtime
			.register_tool(EchoJsonTool::new(
				Vec::new(),
				RuntimeConstraints {
					timeout_ms: 1_000,
					max_retries: 0,
					retry_backoff_ms: 0,
					sandbox_profile: SandboxProfile::NoIsolation,
					deterministic_hooks: true,
				},
			))
			.expect("register tool");

		runtime
			.invoke(ToolInvocation {
				tool_name: "echo-json".to_string(),
				input: json!({"text":"order"}),
				granted_capabilities: Vec::new(),
				invocation_key: Some("inv-001".to_string()),
			})
			.expect("invoke tool");

		let events = hook.events();
		let trace_ids: Vec<String> = events.iter().map(|event| event.trace_id.clone()).collect();
		assert_eq!(
			trace_ids,
			vec![
				"inv-001:0:dispatched".to_string(),
				"inv-001:1:attempt_started".to_string(),
				"inv-001:1:succeeded".to_string()
			]
		);
		assert!(events[2].fingerprint.is_some());
	}
}
