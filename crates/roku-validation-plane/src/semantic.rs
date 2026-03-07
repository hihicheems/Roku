use roku_common_types::{ResultEnvelope, ResultStatus};

pub(crate) fn run_semantic_checks(result: &ResultEnvelope, failures: &mut Vec<String>) {
	if result.schema_version.starts_with("backtest_report.v1")
		&& matches!(result.status, ResultStatus::Ok)
	{
		match serde_json::from_str::<serde_json::Value>(&result.payload) {
			Ok(value) => {
				for field in ["annual_return", "sharpe", "max_drawdown"] {
					if value.get(field).is_none() {
						failures.push(format!("backtest payload missing field: {field}"));
					}
				}
			}
			Err(error) => failures.push(format!("backtest payload is not valid json: {error}")),
		}
	}
}
