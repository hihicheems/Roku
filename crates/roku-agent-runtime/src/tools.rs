use std::sync::Arc;

use roku_llm_adapter::{GenerationRequest, LlmAdapterError, LlmRouter, RiskTier};
use roku_tool_runtime::{
	RuntimeConstraints, SandboxProfile, Tool, ToolDescriptor, ToolFailure, ToolInvocationRequest,
	ToolRuntime, ToolSchema,
};
use serde_json::{Value, json};

pub(crate) const RESEARCH_TOOL_NAME: &str = "research.synthesize";
pub(crate) const DATA_TOOL_NAME: &str = "data.execute";
pub(crate) const REVIEW_TOOL_NAME: &str = "review.assess";
pub(crate) const GENERAL_TOOL_NAME: &str = "general.execute";

pub(crate) fn build_builtin_tool_runtime() -> ToolRuntime {
	let mut runtime = ToolRuntime::default();
	for tool in [
		WorkerReportTool::new(
			RESEARCH_TOOL_NAME,
			"research-worker",
			"research synthesis generated",
			vec!["information.read".to_string()],
			SandboxProfile::PythonResearch,
		),
		WorkerReportTool::new(
			DATA_TOOL_NAME,
			"data-worker",
			"data pipeline step executed",
			vec!["data.read".to_string()],
			SandboxProfile::ContainerRestricted,
		),
		WorkerReportTool::new(
			REVIEW_TOOL_NAME,
			"review-worker",
			"review checks completed",
			vec!["review.check".to_string()],
			SandboxProfile::ReadOnlyFs,
		),
		WorkerReportTool::new(
			GENERAL_TOOL_NAME,
			"generic-worker",
			"generic execution completed",
			Vec::new(),
			SandboxProfile::NoIsolation,
		),
	] {
		runtime
			.register_tool(tool)
			.expect("default runtime tools must register successfully");
	}
	runtime
}

pub(crate) fn build_llm_tool_runtime(router: Arc<LlmRouter>) -> ToolRuntime {
	let mut runtime = ToolRuntime::default();
	for tool in [
		PromptedLlmTool::new(
			RESEARCH_TOOL_NAME,
			"research-worker",
			"You are the research worker inside Roku Agent Runtime. Produce a grounded intermediate result in plain text.",
			vec!["information.read".to_string()],
			SandboxProfile::PythonResearch,
			RiskTier::Medium,
			Arc::clone(&router),
		),
		PromptedLlmTool::new(
			DATA_TOOL_NAME,
			"data-worker",
			"You are the data worker inside Roku Agent Runtime. Produce the data-processing or synthesis result for the described task step in plain text.",
			vec!["data.read".to_string()],
			SandboxProfile::ContainerRestricted,
			RiskTier::Medium,
			Arc::clone(&router),
		),
		PromptedLlmTool::new(
			REVIEW_TOOL_NAME,
			"review-worker",
			"You are the review worker inside Roku Agent Runtime. Produce a concise review or validation conclusion for the described task step in plain text.",
			vec!["review.check".to_string()],
			SandboxProfile::ReadOnlyFs,
			RiskTier::High,
			Arc::clone(&router),
		),
		PromptedLlmTool::new(
			GENERAL_TOOL_NAME,
			"generic-worker",
			"You are the execution worker inside Roku Agent Runtime. Answer the user's goal directly and concisely based on the provided task goal and execution step. Return only the useful answer text.",
			Vec::new(),
			SandboxProfile::NoIsolation,
			RiskTier::Medium,
			router,
		),
	] {
		runtime
			.register_tool(tool)
			.expect("llm runtime tools must register successfully");
	}
	runtime
}

#[derive(Clone)]
struct WorkerReportTool {
	descriptor: ToolDescriptor,
	worker_id: &'static str,
	message: &'static str,
}

impl WorkerReportTool {
	fn new(
		name: &str,
		worker_id: &'static str,
		message: &'static str,
		required_capabilities: Vec<String>,
		sandbox_profile: SandboxProfile,
	) -> Self {
		Self {
			descriptor: tool_descriptor(name, required_capabilities, sandbox_profile, 5_000),
			worker_id,
			message,
		}
	}
}

impl Tool for WorkerReportTool {
	fn descriptor(&self) -> ToolDescriptor {
		self.descriptor.clone()
	}

	fn invoke(&self, request: ToolInvocationRequest) -> Result<Value, ToolFailure> {
		let input = request_input(&request)?;
		Ok(json!({
			"worker_id": self.worker_id,
			"message": self.message,
			"task_id": input.task_id,
			"node_id": input.node_id,
			"goal": input.goal,
			"summary": input.summary,
			"budget_tokens": input.budget_tokens,
			"time_budget_ms": input.time_budget_ms,
			"attempt": request.attempt,
			"invocation_key": request.invocation_key,
		}))
	}
}

#[derive(Clone)]
struct PromptedLlmTool {
	descriptor: ToolDescriptor,
	worker_id: &'static str,
	system_prompt: &'static str,
	risk_tier: RiskTier,
	router: Arc<LlmRouter>,
}

impl PromptedLlmTool {
	fn new(
		name: &str,
		worker_id: &'static str,
		system_prompt: &'static str,
		required_capabilities: Vec<String>,
		sandbox_profile: SandboxProfile,
		risk_tier: RiskTier,
		router: Arc<LlmRouter>,
	) -> Self {
		Self {
			descriptor: tool_descriptor(name, required_capabilities, sandbox_profile, 20_000),
			worker_id,
			system_prompt,
			risk_tier,
			router,
		}
	}
}

impl Tool for PromptedLlmTool {
	fn descriptor(&self) -> ToolDescriptor {
		self.descriptor.clone()
	}

	fn invoke(&self, request: ToolInvocationRequest) -> Result<Value, ToolFailure> {
		let input = request_input(&request)?;
		let prompt = format!(
			"{system_prompt}\n\nTask goal:\n{goal}\n\nExecution step:\n{summary}\n\nConstraints:\n- Worker id: {worker_id}\n- Invocation key: {invocation_key}\n- Time budget ms: {time_budget_ms}\n- Reply in plain text with no markdown fences.",
			system_prompt = self.system_prompt,
			goal = input.goal,
			summary = input.summary,
			worker_id = self.worker_id,
			invocation_key = request.invocation_key,
			time_budget_ms = input.time_budget_ms,
		);

		let response = self
			.router
			.generate(&GenerationRequest {
				prompt,
				expected_output_tokens: input.budget_tokens.min(512),
				risk_tier: self.risk_tier,
				preferred_provider: None,
				budget_tokens_remaining: input.budget_tokens,
				budget_cost_remaining_usd: 1.0,
			})
			.map_err(llm_failure)?;

		Ok(json!({
			"worker_id": self.worker_id,
			"message": response.output,
			"task_id": input.task_id,
			"node_id": input.node_id,
			"goal": input.goal,
			"summary": input.summary,
			"provider": response.provider,
			"model_id": response.model_id,
			"prompt_tokens": response.prompt_tokens,
			"output_tokens": response.output_tokens,
			"latency_ms": response.latency_ms,
			"attempt": request.attempt,
			"invocation_key": request.invocation_key,
		}))
	}
}

struct ToolInput<'a> {
	task_id: &'a str,
	node_id: &'a str,
	goal: &'a str,
	summary: &'a str,
	budget_tokens: u64,
	time_budget_ms: u64,
}

fn request_input(request: &ToolInvocationRequest) -> Result<ToolInput<'_>, ToolFailure> {
	let Some(input) = request.input.as_object() else {
		return Err(ToolFailure::terminal("tool input must be a json object"));
	};

	Ok(ToolInput {
		task_id: input
			.get("task_id")
			.and_then(Value::as_str)
			.unwrap_or_default(),
		node_id: input
			.get("node_id")
			.and_then(Value::as_str)
			.unwrap_or_default(),
		goal: input
			.get("goal")
			.and_then(Value::as_str)
			.unwrap_or_default(),
		summary: input
			.get("summary")
			.and_then(Value::as_str)
			.unwrap_or_default(),
		budget_tokens: input
			.get("budget_tokens")
			.and_then(Value::as_u64)
			.unwrap_or_default(),
		time_budget_ms: input
			.get("time_budget_ms")
			.and_then(Value::as_u64)
			.unwrap_or_default(),
	})
}

fn tool_descriptor(
	name: &str,
	required_capabilities: Vec<String>,
	sandbox_profile: SandboxProfile,
	timeout_ms: u64,
) -> ToolDescriptor {
	ToolDescriptor {
		name: name.to_string(),
		version: "1.0.0".to_string(),
		input_schema: ToolSchema {
			required_fields: vec![
				"task_id".to_string(),
				"node_id".to_string(),
				"goal".to_string(),
				"summary".to_string(),
				"budget_tokens".to_string(),
				"time_budget_ms".to_string(),
			],
		},
		output_schema: "result.v1".to_string(),
		required_capabilities,
		runtime_constraints: RuntimeConstraints {
			timeout_ms,
			max_retries: 0,
			retry_backoff_ms: 0,
			sandbox_profile,
			deterministic_hooks: true,
		},
	}
}

fn llm_failure(error: LlmAdapterError) -> ToolFailure {
	match error {
		LlmAdapterError::BudgetExceeded(message) => ToolFailure::terminal(message),
		LlmAdapterError::LatencyExceeded {
			latency_ms,
			max_latency_ms,
		} => ToolFailure::terminal(format!(
			"llm latency exceeded policy: latency={latency_ms}ms max={max_latency_ms}ms"
		)),
		LlmAdapterError::NoEligibleModel => {
			ToolFailure::terminal("no eligible llm model for request")
		}
		LlmAdapterError::ProviderNotRegistered(provider) => {
			ToolFailure::terminal(format!("llm provider is not registered: {provider}"))
		}
		LlmAdapterError::ProviderCallFailed {
			provider,
			model_id,
			message,
		} => ToolFailure::terminal(format!(
			"llm provider call failed for {provider}/{model_id}: {message}"
		)),
	}
}
