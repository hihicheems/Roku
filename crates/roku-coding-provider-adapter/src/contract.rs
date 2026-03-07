use serde::{Deserialize, Serialize};

use roku_common_types::{CodeChangeReport, CodingWorkContract};

use crate::CodingProviderError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderExecutionPayload {
	pub modified_files: Vec<String>,
	pub patch_summary: String,
	pub commands_executed: Vec<String>,
	pub test_results: Vec<String>,
	pub artifacts: Vec<String>,
	pub residual_risks: Vec<String>,
}

pub(crate) fn validate_contract(contract: &CodingWorkContract) -> Result<(), CodingProviderError> {
	if contract.repo_ref.trim().is_empty() {
		return Err(CodingProviderError::InvalidContract(
			"repo_ref cannot be empty".to_string(),
		));
	}
	if contract.goal.trim().is_empty() {
		return Err(CodingProviderError::InvalidContract(
			"goal cannot be empty".to_string(),
		));
	}
	if contract.allowed_paths.is_empty() {
		return Err(CodingProviderError::InvalidContract(
			"allowed_paths cannot be empty".to_string(),
		));
	}
	if contract.acceptance_checks.is_empty() {
		return Err(CodingProviderError::InvalidContract(
			"acceptance_checks cannot be empty".to_string(),
		));
	}
	if contract.output_schema.trim().is_empty() {
		return Err(CodingProviderError::InvalidContract(
			"output_schema cannot be empty".to_string(),
		));
	}
	if contract.token_budget == 0 {
		return Err(CodingProviderError::InvalidContract(
			"token_budget must be greater than zero".to_string(),
		));
	}
	if contract.time_budget_ms == 0 {
		return Err(CodingProviderError::InvalidContract(
			"time_budget_ms must be greater than zero".to_string(),
		));
	}
	Ok(())
}

pub(crate) fn normalize_payload(payload: ProviderExecutionPayload) -> CodeChangeReport {
	CodeChangeReport {
		modified_files: payload.modified_files,
		patch_summary: payload.patch_summary,
		commands_executed: payload.commands_executed,
		test_results: payload.test_results,
		artifacts: payload.artifacts,
		residual_risks: payload.residual_risks,
	}
}
