use std::sync::Arc;

use crate::CommandError;
use crate::runtime::build_live_runtime_service_from_env;

pub fn run_telegram_bot_from_env() -> Result<(), CommandError> {
	let service = Arc::new(build_live_runtime_service_from_env()?);
	let runner = roku_connectors_telegram::TelegramPollingRunner::from_env()?;
	runner
		.run(move |request| service.execute(request))
		.map_err(CommandError::TelegramTransport)
}
