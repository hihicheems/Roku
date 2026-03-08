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

use std::collections::HashMap;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::{
	ExecutionEvent, ExecutionEventKind, ExecutionHook, SandboxProfile, ToolDescriptor, ToolFailure,
	ToolRuntimeError, ToolSchema,
};

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

pub trait Tool: Send + Sync {
	fn descriptor(&self) -> ToolDescriptor;
	fn invoke(&self, request: ToolInvocationRequest) -> Result<Value, ToolFailure>;
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
