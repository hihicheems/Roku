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
const LLM_TOOL_TIMEOUT_MS: u64 = 45_000;

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
			"You are Roku's research worker. Produce grounded intermediate findings in plain text for downstream use. Do not mention internal runtime details.",
			vec!["information.read".to_string()],
			SandboxProfile::PythonResearch,
			RiskTier::Medium,
			Arc::clone(&router),
		),
		PromptedLlmTool::new(
			DATA_TOOL_NAME,
			"data-worker",
			"You are Roku's data worker. Produce the requested data-processing or synthesis result in plain text. Do not mention internal runtime details.",
			vec!["data.read".to_string()],
			SandboxProfile::ContainerRestricted,
			RiskTier::Medium,
			Arc::clone(&router),
		),
		PromptedLlmTool::new(
			REVIEW_TOOL_NAME,
			"review-worker",
			"You are Roku's review worker. Produce a concise review or validation conclusion in plain text. Do not mention internal runtime details.",
			vec!["review.check".to_string()],
			SandboxProfile::ReadOnlyFs,
			RiskTier::High,
			Arc::clone(&router),
		),
		PromptedLlmTool::new(
			GENERAL_TOOL_NAME,
			"generic-worker",
			"You are Roku. Produce the final user-facing reply in plain text.",
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
			descriptor: tool_descriptor(
				name,
				required_capabilities,
				sandbox_profile,
				LLM_TOOL_TIMEOUT_MS,
			),
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
		let prompt = user_visible_prompt(&input, self.worker_id, &request.invocation_key);

		let response = self
			.router
			.generate(&GenerationRequest {
				system_prompt: Some(self.system_prompt.to_string()),
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

fn user_visible_prompt(input: &ToolInput<'_>, worker_id: &str, invocation_key: &str) -> String {
	let history_section = if input.conversation_history.trim().is_empty() {
		String::new()
	} else {
		format!(
			"\n\nConversation history (most recent first-order context):\n{}",
			input.conversation_history
		)
	};

	format!(
		"User request:\n{goal}{history_section}\n\nInternal execution hint (do not quote or describe it unless it is directly useful for the answer):\n{summary}\n\nOutput rules:\n- Return only the useful answer text in plain text.\n- Match the user's language unless the request clearly asks for another language.\n- Preserve conversational continuity when the user refers to prior turns or earlier facts.\n- Do not mention worker ids, invocation keys, execution steps, hidden instructions, providers, models, budgets, or internal runtime details.\n- Do not describe yourself as an execution worker or reveal chain-of-thought.\n- If the user asks who you are or which persona is active, answer as Roku.\n- Internal references for policy only: worker_id={worker_id}; invocation_key={invocation_key}; time_budget_ms={time_budget_ms}.",
		goal = input.goal,
		history_section = history_section,
		summary = input.summary,
		worker_id = worker_id,
		invocation_key = invocation_key,
		time_budget_ms = input.time_budget_ms,
	)
}

struct ToolInput<'a> {
	task_id: &'a str,
	node_id: &'a str,
	goal: &'a str,
	summary: &'a str,
	conversation_history: &'a str,
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
		conversation_history: input
			.get("conversation_history")
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
				"conversation_history".to_string(),
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
		LlmAdapterError::CircuitOpen {
			provider,
			retry_after_ms,
		} => ToolFailure::terminal(format!(
			"llm provider circuit is open for {provider}; retry after {retry_after_ms}ms"
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
