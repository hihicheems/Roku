use roku_common_types::{CodeChangeReport, CodingWorkContract};
use roku_mcp_bridge::{McpClient, McpRequest};

use crate::contract::{ProviderExecutionPayload, normalize_payload, validate_contract};
use crate::error::CodingProviderError;

pub trait CodingProvider {
	fn execute(
		&self,
		contract: &CodingWorkContract,
	) -> Result<CodeChangeReport, CodingProviderError>;
}

#[derive(Debug)]
pub struct McpCodingProvider<C> {
	client: C,
	server_id: String,
}

impl<C> McpCodingProvider<C> {
	pub fn new(client: C, server_id: impl Into<String>) -> Self {
		Self {
			client,
			server_id: server_id.into(),
		}
	}
}

impl<C: McpClient> CodingProvider for McpCodingProvider<C> {
	fn execute(
		&self,
		contract: &CodingWorkContract,
	) -> Result<CodeChangeReport, CodingProviderError> {
		validate_contract(contract)?;

		let tools = self.client.discover_tools(&self.server_id)?;
		if !tools
			.iter()
			.any(|descriptor| descriptor.tool_name == "coding.execute")
		{
			return Err(CodingProviderError::InvalidResponse(
				"coding.execute tool is not published by provider".to_string(),
			));
		}

		let request = McpRequest {
			server_id: self.server_id.clone(),
			method: "tools/call/coding.execute".to_string(),
			payload: serde_json::to_string(contract).map_err(|error| {
				CodingProviderError::InvalidContract(format!(
					"failed to serialize coding contract: {error}"
				))
			})?,
		};
		let response = self.client.call(&request)?;
		if !response.success {
			return Err(CodingProviderError::ProviderRejected(response.payload));
		}

		let payload: ProviderExecutionPayload =
			serde_json::from_str(&response.payload).map_err(|error| {
				CodingProviderError::InvalidResponse(format!(
					"provider payload is not valid execution payload: {error}"
				))
			})?;

		Ok(normalize_payload(payload))
	}
}

#[cfg(test)]
mod tests {
	use roku_mcp_bridge::{InMemoryMcpBridge, McpResponse, McpToolDescriptor};

	use super::*;

	fn sample_contract() -> CodingWorkContract {
		CodingWorkContract {
			repo_ref: "artifact://repos/roku-main".to_string(),
			goal: "Implement feature X".to_string(),
			allowed_paths: vec!["src/**".to_string(), "tests/**".to_string()],
			acceptance_checks: vec!["cargo test".to_string()],
			output_schema: "code_change_report.v1".to_string(),
			token_budget: 20_000,
			time_budget_ms: 600_000,
		}
	}

	fn coding_descriptor() -> McpToolDescriptor {
		McpToolDescriptor {
			server_id: "coding-provider".to_string(),
			tool_name: "coding.execute".to_string(),
			description: "Execute coding work contract".to_string(),
			required_capabilities: vec!["external:coding_provider".to_string()],
			input_schema: "coding_work_contract.v1".to_string(),
			output_schema: "code_change_report.v1".to_string(),
		}
	}

	#[test]
	fn reject_invalid_contract_without_allowed_paths() {
		let bridge = InMemoryMcpBridge::default();
		let provider = McpCodingProvider::new(bridge, "coding-provider");
		let mut contract = sample_contract();
		contract.allowed_paths.clear();

		let error = provider
			.execute(&contract)
			.expect_err("invalid contract should be rejected");
		assert!(matches!(error, CodingProviderError::InvalidContract(_)));
	}

	#[test]
	fn execute_coding_contract_through_mcp_bridge() {
		let mut bridge = InMemoryMcpBridge::default();
		bridge.register_tool(coding_descriptor());
		bridge.register_response(
			"coding-provider",
			"tools/call/coding.execute",
			McpResponse {
				success: true,
				payload: serde_json::to_string(&ProviderExecutionPayload {
					modified_files: vec!["src/lib.rs".to_string(), "tests/lib.rs".to_string()],
					patch_summary: "Implement feature X".to_string(),
					commands_executed: vec!["cargo test".to_string()],
					test_results: vec!["cargo test: passed".to_string()],
					artifacts: vec!["artifact://patches/123".to_string()],
					residual_risks: vec!["benchmark not executed".to_string()],
				})
				.expect("payload should serialize"),
				audit_ref: Some("audit-1".to_string()),
			},
		);
		let provider = McpCodingProvider::new(bridge, "coding-provider");

		let report = provider
			.execute(&sample_contract())
			.expect("coding provider should return report");

		assert_eq!(report.modified_files.len(), 2);
		assert_eq!(report.patch_summary, "Implement feature X");
		assert_eq!(report.commands_executed, vec!["cargo test".to_string()]);
		assert_eq!(report.artifacts, vec!["artifact://patches/123".to_string()]);
	}

	#[test]
	fn reject_unstructured_provider_payload() {
		let mut bridge = InMemoryMcpBridge::default();
		bridge.register_tool(coding_descriptor());
		bridge.register_response(
			"coding-provider",
			"tools/call/coding.execute",
			McpResponse {
				success: true,
				payload: "plain text is not allowed".to_string(),
				audit_ref: None,
			},
		);
		let provider = McpCodingProvider::new(bridge, "coding-provider");

		let error = provider
			.execute(&sample_contract())
			.expect_err("provider payload should be structured");
		assert!(matches!(error, CodingProviderError::InvalidResponse(_)));
	}
}
