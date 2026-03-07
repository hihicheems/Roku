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
			descriptor: ToolDescriptor {
				name: name.to_string(),
				version: "1.0.0".to_string(),
				input_schema: ToolSchema {
					required_fields: vec![
						"task_id".to_string(),
						"node_id".to_string(),
						"summary".to_string(),
						"budget_tokens".to_string(),
						"time_budget_ms".to_string(),
					],
				},
				output_schema: "result.v1".to_string(),
				required_capabilities,
				runtime_constraints: RuntimeConstraints {
					timeout_ms: 5_000,
					max_retries: 0,
					retry_backoff_ms: 0,
					sandbox_profile,
					deterministic_hooks: true,
				},
			},
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
		let Some(input) = request.input.as_object() else {
			return Err(ToolFailure::terminal("tool input must be a json object"));
		};

		let task_id = input
			.get("task_id")
			.and_then(Value::as_str)
			.unwrap_or_default();
		let node_id = input
			.get("node_id")
			.and_then(Value::as_str)
			.unwrap_or_default();
		let summary = input
			.get("summary")
			.and_then(Value::as_str)
			.unwrap_or_default();
		let budget_tokens = input
			.get("budget_tokens")
			.and_then(Value::as_u64)
			.unwrap_or_default();
		let time_budget_ms = input
			.get("time_budget_ms")
			.and_then(Value::as_u64)
			.unwrap_or_default();

		Ok(json!({
			"worker_id": self.worker_id,
			"message": self.message,
			"task_id": task_id,
			"node_id": node_id,
			"summary": summary,
			"budget_tokens": budget_tokens,
			"time_budget_ms": time_budget_ms,
			"attempt": request.attempt,
			"invocation_key": request.invocation_key,
		}))
	}
}
