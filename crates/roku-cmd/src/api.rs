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

//! HTTP gateway bootstrap for command-owned local deployments.
//!
//! The CLI crate only owns process startup and env parsing here. Request validation and runtime
//! execution semantics stay inside the API gateway and runtime service crates.

use std::env;
use std::sync::Arc;

use actix_web::{App, HttpServer, web};
use roku_api_gateway::{GatewayAppState, RuntimeServiceExecutor, configure_routes};
use roku_observability::{LogLevel, LogRecord, emit_global_log};

use crate::CommandError;
use crate::runtime::build_live_runtime_service_from_env;

/// Process-local server settings for the embedded API gateway.
///
/// These values are intentionally small and startup-scoped so the command surface can keep HTTP
/// bootstrap concerns separate from runtime config owned by other crates.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ApiGatewayServerConfig {
	bind_addr: String,
	json_limit_bytes: usize,
}

impl ApiGatewayServerConfig {
	fn from_env() -> Result<Self, CommandError> {
		let bind_addr = env::var("ROKU_API_BIND_ADDR")
			.ok()
			.filter(|value| !value.trim().is_empty())
			.unwrap_or_else(|| "127.0.0.1:8787".to_string());
		let json_limit_bytes = match env::var("ROKU_API_JSON_LIMIT_BYTES") {
			Ok(value) if !value.trim().is_empty() => value.parse::<usize>().map_err(|error| {
				CommandError::ApiGatewayBootstrap(format!(
					"invalid ROKU_API_JSON_LIMIT_BYTES: {error}"
				))
			})?,
			Ok(_) | Err(env::VarError::NotPresent) => 8 * 1024,
			Err(error) => {
				return Err(CommandError::ApiGatewayBootstrap(format!(
					"failed to read ROKU_API_JSON_LIMIT_BYTES: {error}"
				)));
			}
		};

		Ok(Self {
			bind_addr,
			json_limit_bytes,
		})
	}
}

/// Boots the HTTP gateway and blocks the current process until the server exits.
///
/// This command shares the same live runtime bootstrap path as Telegram and `live-once`, so all
/// three surfaces observe the same plugin inventory and fallback mode report.
pub(crate) fn run_api_gateway_from_env() -> Result<(), CommandError> {
	let config = ApiGatewayServerConfig::from_env()?;
	let service = Arc::new(build_live_runtime_service_from_env()?);
	let executor = Arc::new(RuntimeServiceExecutor::new(service));
	let state = web::Data::new(GatewayAppState::new(executor));
	let bind_addr = config.bind_addr.clone();
	let json_limit_bytes = config.json_limit_bytes;
	let _ = emit_global_log(
		LogRecord::new("roku-cmd", LogLevel::Info, "starting api gateway server")
			.with_field("bind_addr", bind_addr.clone())
			.with_field("json_limit_bytes", json_limit_bytes.to_string()),
	);

	actix_web::rt::System::new()
		.block_on(async move {
			HttpServer::new(move || {
				App::new()
					.app_data(state.clone())
					.app_data(web::JsonConfig::default().limit(json_limit_bytes))
					.configure(configure_routes)
			})
			.bind(&bind_addr)?
			.run()
			.await
		})
		.map_err(|error| CommandError::ApiGatewayBootstrap(error.to_string()))
}

#[cfg(test)]
mod tests {
	use super::ApiGatewayServerConfig;

	#[test]
	fn api_gateway_server_config_defaults_are_stable() {
		let config = ApiGatewayServerConfig {
			bind_addr: "127.0.0.1:8787".to_string(),
			json_limit_bytes: 8 * 1024,
		};

		assert_eq!(config.bind_addr, "127.0.0.1:8787");
		assert_eq!(config.json_limit_bytes, 8 * 1024);
	}
}
