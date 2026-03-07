//! External coding provider adapter.

use roku_common_types::{CodeChangeReport, CodingWorkContract};
use roku_mcp_bridge::{McpClient, McpRequest};

pub trait CodingProvider {
	fn execute(&self, contract: &CodingWorkContract) -> CodeChangeReport;
}

#[derive(Debug)]
pub struct McpCodingProvider<C> {
	client: C,
}

impl<C> McpCodingProvider<C> {
	pub fn new(client: C) -> Self {
		Self { client }
	}
}

impl<C: McpClient> CodingProvider for McpCodingProvider<C> {
	fn execute(&self, contract: &CodingWorkContract) -> CodeChangeReport {
		let response = self.client.call(&McpRequest {
			method: "coding.execute".to_string(),
			payload: contract.goal.clone(),
		});

		CodeChangeReport {
			modified_files: vec!["src/lib.rs".to_string()],
			patch_summary: response.payload,
			commands_executed: contract.acceptance_checks.clone(),
			test_results: vec!["pending".to_string()],
			artifacts: vec!["artifact://patch/mock".to_string()],
			residual_risks: Vec::new(),
		}
	}
}
