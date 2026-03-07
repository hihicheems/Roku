use roku_agent_runtime::GenericAgentRuntime;
use roku_api_gateway::{Gateway, RawRequest};
use roku_common_types::{ResponseEnvelope, RuntimeError};
use roku_llm_adapter::{OpenRouterConfig, build_openrouter_router};
pub use roku_runtime_service::RunMode;
use roku_runtime_service::RuntimeService;

use crate::CommandError;

pub fn run_once(goal: &str) -> Result<ResponseEnvelope, RuntimeError> {
	run_with_mode(goal, RunMode::Normal)
}

pub fn run_with_mode(goal: &str, mode: RunMode) -> Result<ResponseEnvelope, RuntimeError> {
	let gateway = Gateway;
	let service = RuntimeService::default();
	let request = gateway.normalize(
		RawRequest {
			session_id: "session-1".to_string(),
			goal: goal.to_string(),
		},
		1,
	);

	service.execute_with_mode(request, mode)
}

pub fn run_live_once_from_env(goal: &str) -> Result<ResponseEnvelope, CommandError> {
	let gateway = Gateway;
	let service = build_live_runtime_service_from_env()?;
	let request = gateway.normalize(
		RawRequest {
			session_id: "session-1".to_string(),
			goal: goal.to_string(),
		},
		1,
	);
	service.execute(request).map_err(CommandError::Runtime)
}

pub(crate) fn build_live_runtime_service_from_env() -> Result<RuntimeService, CommandError> {
	let config = OpenRouterConfig::from_env()?;
	let router = build_openrouter_router(config)?;
	let runtime = GenericAgentRuntime::with_llm_router(router);
	Ok(RuntimeService::in_memory_with_agent_runtime(runtime))
}
