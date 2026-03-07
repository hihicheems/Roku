use std::sync::Arc;

use roku_common_types::{ApprovalDecision, ApprovalId, RequestEnvelope, ResponseEnvelope};

use crate::CommandError;
use crate::runtime::build_live_runtime_service_from_env;

pub fn run_telegram_bot_from_env() -> Result<(), CommandError> {
	let service = Arc::new(build_live_runtime_service_from_env()?);
	let runner = roku_connectors_telegram::TelegramPollingRunner::from_env()?;
	runner
		.run(RuntimeServiceTelegramHandler { service })
		.map_err(CommandError::TelegramTransport)
}

struct RuntimeServiceTelegramHandler {
	service: Arc<roku_runtime_service::RuntimeService>,
}

impl roku_connectors_telegram::TelegramInteractionHandler for RuntimeServiceTelegramHandler {
	fn handle_request(
		&self,
		request: RequestEnvelope,
	) -> Result<ResponseEnvelope, roku_common_types::RuntimeError> {
		self.service.execute(request)
	}

	fn handle_approval_decision(
		&self,
		approval_id: ApprovalId,
		decision: ApprovalDecision,
	) -> Result<ResponseEnvelope, roku_common_types::RuntimeError> {
		self.service.decide_approval(&approval_id, decision)
	}
}
