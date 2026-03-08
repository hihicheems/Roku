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
