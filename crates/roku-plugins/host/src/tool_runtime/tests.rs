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

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use roku_common_types::{
	ApprovalRequirement, ApprovalRequirementScope, CanonicalDigest, CanonicalExecution,
	ExecutionActionClass, ExecutionEnvPolicy, ExecutionEnvPolicyMode, ExecutionResourceScope,
	InvocationMode, PolicyDecision, PolicyOutcome, PolicyReasonCode,
};
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
				contract: None,
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
					allowed_read_roots: Vec::new(),
					allowed_write_roots: Vec::new(),
				},
				contract: None,
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
					allowed_read_roots: Vec::new(),
					allowed_write_roots: Vec::new(),
				},
				contract: None,
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

struct PolicyAwareTool {
	descriptor: ToolDescriptor,
	decision: Option<PolicyDecision>,
	invocations: Arc<Mutex<u8>>,
}

impl PolicyAwareTool {
	fn new(name: &str, decision: Option<PolicyDecision>) -> (Self, Arc<Mutex<u8>>) {
		let invocations = Arc::new(Mutex::new(0));
		(
			Self {
				descriptor: ToolDescriptor {
					name: name.to_string(),
					version: "1.0.0".to_string(),
					input_schema: ToolSchema::default(),
					output_schema: "policy-aware.v1".to_string(),
					required_capabilities: Vec::new(),
					runtime_constraints: RuntimeConstraints {
						timeout_ms: 1_000,
						max_retries: 0,
						retry_backoff_ms: 0,
						sandbox_profile: SandboxProfile::NoIsolation,
						deterministic_hooks: true,
						allowed_read_roots: Vec::new(),
						allowed_write_roots: Vec::new(),
					},
					contract: None,
				},
				decision,
				invocations: invocations.clone(),
			},
			invocations,
		)
	}
}

impl Tool for PolicyAwareTool {
	fn descriptor(&self) -> ToolDescriptor {
		self.descriptor.clone()
	}

	fn invoke(&self, _request: ToolInvocationRequest) -> Result<Value, ToolFailure> {
		let mut invocations = self.invocations.lock().expect("poisoned lock");
		*invocations += 1;
		Ok(json!({"status": "ok"}))
	}

	fn policy_decision(&self, _execution: &CanonicalExecution) -> Option<PolicyDecision> {
		self.decision.clone()
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

fn sample_canonical_execution(tool_name: &str, program: &str) -> CanonicalExecution {
	CanonicalExecution {
		tool_name: tool_name.to_string(),
		program: program.to_string(),
		argv: vec![program.to_string()],
		invocation_mode: InvocationMode::DirectExec,
		shell_context: None,
		cwd: "/workspace".to_string(),
		env_policy: ExecutionEnvPolicy {
			mode: ExecutionEnvPolicyMode::Clean,
			allowed_keys: Vec::new(),
		},
		resource_scope: ExecutionResourceScope {
			working_directory: "/workspace".to_string(),
			resolved_targets: vec!["/workspace".to_string()],
			effective_read_roots: vec!["/workspace".to_string()],
			effective_write_roots: vec!["/workspace/out".to_string()],
		},
		action_class: ExecutionActionClass::Read,
		digest: CanonicalDigest(format!("digest:{tool_name}:{program}")),
	}
}

fn allow_decision() -> PolicyDecision {
	PolicyDecision {
		outcome: PolicyOutcome::Allow,
		reason_code: PolicyReasonCode::AllowedByPolicy,
		approval_requirement: None,
	}
}

fn deny_decision(reason_code: PolicyReasonCode) -> PolicyDecision {
	PolicyDecision {
		outcome: PolicyOutcome::Deny,
		reason_code,
		approval_requirement: None,
	}
}

fn require_approval_decision(reason_code: PolicyReasonCode) -> PolicyDecision {
	PolicyDecision {
		outcome: PolicyOutcome::RequireApproval,
		reason_code,
		approval_requirement: Some(ApprovalRequirement {
			scope: ApprovalRequirementScope::Invocation,
			reason_code,
		}),
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
			allowed_read_roots: Vec::new(),
			allowed_write_roots: Vec::new(),
		},
	);
	runtime.register_tool(tool).expect("register tool");

	let result = runtime
		.invoke(ToolInvocation {
			tool_name: "echo-json".to_string(),
			input: json!({"text":"hello"}),
			canonical_execution: None,
			granted_capabilities: vec!["artifact:read:dataset/*".to_string()],
			invocation_key: None,
			attachments: Vec::new(),
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
			canonical_execution: None,
			granted_capabilities: Vec::new(),
			invocation_key: Some("cap-denied".to_string()),
			attachments: Vec::new(),
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
			canonical_execution: None,
			granted_capabilities: Vec::new(),
			invocation_key: Some("flaky-invoke".to_string()),
			attachments: Vec::new(),
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
			canonical_execution: None,
			granted_capabilities: Vec::new(),
			invocation_key: Some("slow-invoke".to_string()),
			attachments: Vec::new(),
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
				allowed_read_roots: Vec::new(),
				allowed_write_roots: Vec::new(),
			},
		))
		.expect("register tool");

	runtime
		.invoke(ToolInvocation {
			tool_name: "echo-json".to_string(),
			input: json!({"text":"order"}),
			canonical_execution: None,
			granted_capabilities: Vec::new(),
			invocation_key: Some("inv-001".to_string()),
			attachments: Vec::new(),
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

#[test]
fn command_run_allow_policy_invokes_tool() {
	let mut runtime = ToolRuntime::default();
	let (tool, invocations) = PolicyAwareTool::new("command.run", Some(allow_decision()));
	runtime.register_tool(tool).expect("register tool");

	let result = runtime
		.invoke(ToolInvocation {
			tool_name: "command.run".to_string(),
			input: json!({}),
			canonical_execution: Some(sample_canonical_execution("command.run", "pwd")),
			granted_capabilities: Vec::new(),
			invocation_key: Some("command-allow".to_string()),
			attachments: Vec::new(),
		})
		.expect("invoke command.run");

	assert_eq!(result.output["status"], "ok");
	assert_eq!(*invocations.lock().expect("poisoned lock"), 1);
}

#[test]
fn command_run_deny_policy_rejects_before_invoke() {
	let mut runtime = ToolRuntime::default();
	let hook = Arc::new(RecordingHook::default());
	runtime.register_hook(hook.clone());
	let (tool, invocations) = PolicyAwareTool::new(
		"command.run",
		Some(deny_decision(PolicyReasonCode::DeniedByCommandPolicy)),
	);
	runtime.register_tool(tool).expect("register tool");

	let error = runtime
		.invoke(ToolInvocation {
			tool_name: "command.run".to_string(),
			input: json!({}),
			canonical_execution: Some(sample_canonical_execution("command.run", "pwd")),
			granted_capabilities: Vec::new(),
			invocation_key: Some("command-deny".to_string()),
			attachments: Vec::new(),
		})
		.expect_err("command.run should be rejected");

	match &error {
		ToolRuntimeError::ExecutionFailed {
			tool,
			attempts,
			retriable,
			policy_decision,
			..
		} => {
			assert_eq!(tool, "command.run");
			assert_eq!(*attempts, 0);
			assert!(!retriable);
			assert_eq!(
				*policy_decision,
				Some(deny_decision(PolicyReasonCode::DeniedByCommandPolicy))
			);
		}
		other => panic!("unexpected error: {other:?}"),
	}

	assert_eq!(*invocations.lock().expect("poisoned lock"), 0);
	let events = hook.events();
	assert_eq!(events.len(), 1);
	assert_eq!(events[0].kind, ExecutionEventKind::Rejected);
	assert!(events[0].message.is_some());
	assert_eq!(
		error.policy_decision(),
		Some(&deny_decision(PolicyReasonCode::DeniedByCommandPolicy))
	);
}

#[test]
fn command_run_require_approval_rejects_before_invoke() {
	let mut runtime = ToolRuntime::default();
	let hook = Arc::new(RecordingHook::default());
	runtime.register_hook(hook.clone());
	let (tool, invocations) = PolicyAwareTool::new(
		"command.run",
		Some(require_approval_decision(
			PolicyReasonCode::ApprovalRequiredByWriteScope,
		)),
	);
	runtime.register_tool(tool).expect("register tool");

	let error = runtime
		.invoke(ToolInvocation {
			tool_name: "command.run".to_string(),
			input: json!({}),
			canonical_execution: Some(sample_canonical_execution("command.run", "pwd")),
			granted_capabilities: Vec::new(),
			invocation_key: Some("command-approval".to_string()),
			attachments: Vec::new(),
		})
		.expect_err("command.run should require approval");

	match &error {
		ToolRuntimeError::ExecutionFailed {
			tool,
			attempts,
			retriable,
			policy_decision,
			..
		} => {
			assert_eq!(tool, "command.run");
			assert_eq!(*attempts, 0);
			assert!(!retriable);
			assert_eq!(
				*policy_decision,
				Some(require_approval_decision(
					PolicyReasonCode::ApprovalRequiredByWriteScope
				))
			);
		}
		other => panic!("unexpected error: {other:?}"),
	}

	assert_eq!(*invocations.lock().expect("poisoned lock"), 0);
	let events = hook.events();
	assert_eq!(events.len(), 1);
	assert_eq!(events[0].kind, ExecutionEventKind::Rejected);
	assert!(events[0].message.is_some());
	assert_eq!(
		error.policy_decision(),
		Some(&require_approval_decision(
			PolicyReasonCode::ApprovalRequiredByWriteScope
		))
	);
}

#[test]
fn non_command_tool_with_canonical_execution_does_not_trigger_command_policy() {
	let mut runtime = ToolRuntime::default();
	let (tool, invocations) = PolicyAwareTool::new("echo-json", None);
	runtime.register_tool(tool).expect("register tool");

	let result = runtime
		.invoke(ToolInvocation {
			tool_name: "echo-json".to_string(),
			input: json!({}),
			canonical_execution: Some(sample_canonical_execution("echo-json", "rm")),
			granted_capabilities: Vec::new(),
			invocation_key: Some("echo-policy-bypass".to_string()),
			attachments: Vec::new(),
		})
		.expect("non-command tool should still execute");

	assert_eq!(result.output["status"], "ok");
	assert_eq!(*invocations.lock().expect("poisoned lock"), 1);
}

#[test]
fn command_run_without_canonical_execution_skips_policy_gate() {
	let mut runtime = ToolRuntime::default();
	let (tool, invocations) = PolicyAwareTool::new(
		"command.run",
		Some(deny_decision(PolicyReasonCode::DeniedByCommandPolicy)),
	);
	runtime.register_tool(tool).expect("register tool");

	let result = runtime
		.invoke(ToolInvocation {
			tool_name: "command.run".to_string(),
			input: json!({}),
			canonical_execution: None,
			granted_capabilities: Vec::new(),
			invocation_key: Some("command-no-canonical".to_string()),
			attachments: Vec::new(),
		})
		.expect("missing canonical execution should keep current behavior");

	assert_eq!(result.output["status"], "ok");
	assert_eq!(*invocations.lock().expect("poisoned lock"), 1);
}
