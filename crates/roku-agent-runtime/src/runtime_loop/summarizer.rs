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

use serde::{Deserialize, Serialize};

use crate::runtime_loop::ToolObservation;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinalSummary {
	pub final_message: String,
}

pub(crate) fn summarize_observation(goal: &str, observation: &ToolObservation) -> FinalSummary {
	let failure_message = summarize_failure(goal, observation);
	let final_message = match observation.tool_name.as_str() {
		"fs.read_text" | "fs.list_dir" | "fs.inspect" | "fs.exists" | "fs.find"
			if failure_message.is_some() =>
		{
			failure_message.unwrap_or_else(|| observation.message.clone())
		}
		"fs.read_text" => observation
			.data
			.get("content")
			.and_then(serde_json::Value::as_str)
			.and_then(|content| (!content.trim().is_empty()).then_some(content))
			.map(str::to_string)
			.unwrap_or_else(|| observation.message.clone()),
		"fs.list_dir" | "fs.inspect" | "fs.exists" | "fs.find" => observation.message.clone(),
		_ => {
			if !goal.is_ascii() {
				format!("已完成：{}", observation.message)
			} else {
				format!("Completed: {}", observation.message)
			}
		}
	};
	FinalSummary { final_message }
}

fn summarize_failure(goal: &str, observation: &ToolObservation) -> Option<String> {
	let error_type = observation.error_type.as_deref()?;
	let is_non_ascii = !goal.is_ascii();
	let path = observation
		.data
		.get("path")
		.and_then(serde_json::Value::as_str)
		.or_else(|| {
			observation
				.data
				.get("name")
				.and_then(serde_json::Value::as_str)
		})
		.unwrap_or("the requested path");
	match error_type {
		"workspace_violation" => Some(if is_non_ascii {
			format!("我不能访问 `{path}`，因为它超出了当前允许的工作区范围。")
		} else {
			format!("I can't access `{path}` because it is outside the allowed workspace roots.")
		}),
		"path_not_found" => Some(if is_non_ascii {
			format!("我在当前允许的工作区范围内没有找到 `{path}`。")
		} else {
			format!("I couldn't find `{path}` inside the allowed workspace roots.")
		}),
		"not_directory" => Some(if is_non_ascii {
			format!("`{path}` 不是目录，所以我不能列出它的内容。")
		} else {
			format!("`{path}` is not a directory, so I can't list its contents.")
		}),
		"not_file" => Some(if is_non_ascii {
			format!("`{path}` 不是文本文件，所以我不能按文件内容读取它。")
		} else {
			format!("`{path}` is not a text file, so I can't read it as file contents.")
		}),
		"permission_denied" => Some(if is_non_ascii {
			format!("我没有权限访问 `{path}`。")
		} else {
			format!("I don't have permission to access `{path}`.")
		}),
		"tool_timeout" => Some(if is_non_ascii {
			"这次文件系统操作超时了，请缩小范围后再试。".to_string()
		} else {
			"The filesystem operation timed out. Please try a narrower target.".to_string()
		}),
		_ => None,
	}
}
