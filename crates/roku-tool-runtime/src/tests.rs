use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};

use crate::{
	ExecutionEvent, ExecutionEventKind, ExecutionHook, RuntimeConstraints, SandboxProfile, Tool,
	ToolDescriptor, ToolFailure, ToolInvocation, ToolInvocationRequest, ToolRuntime,
	ToolRuntimeError, ToolSchema,
};

#[derive(Clone)]
struct EchoJsonTool {
	descriptor: ToolDescriptor,
}

impl EchoJsonTool {
	fn new(required_capabilities: Vec<String>, runtime_constraints: RuntimeConstraints) -> Self {
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
