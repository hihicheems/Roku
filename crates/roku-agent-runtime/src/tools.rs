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

use std::sync::Arc;

use roku_llm_adapter::{GenerationRequest, LlmAdapterError, LlmRouter, RiskTier};
use roku_observability::{LogLevel, LogRecord, emit_global_log};
use roku_tool_runtime::{
	RuntimeConstraints, SandboxProfile, Tool, ToolDescriptor, ToolFailure, ToolInvocationRequest,
	ToolRuntime, ToolSchema,
};
use serde_json::{Value, json};
use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};

pub(crate) const RESEARCH_TOOL_NAME: &str = "research.synthesize";
pub(crate) const DATA_TOOL_NAME: &str = "data.execute";
pub(crate) const REVIEW_TOOL_NAME: &str = "review.assess";
pub(crate) const GENERAL_TOOL_NAME: &str = "general.execute";
const LLM_TOOL_TIMEOUT_MS: u64 = 45_000;

pub(crate) fn build_builtin_tool_runtime() -> ToolRuntime {
	let mut runtime = ToolRuntime::default();
	for tool in [
		WorkerReportTool::new(
			RESEARCH_TOOL_NAME,
			"research-worker",
			"research synthesis generated",
			vec!["information.read".to_string()],
			SandboxProfile::PythonResearch,
		),
		WorkerReportTool::new(
			DATA_TOOL_NAME,
			"data-worker",
			"data pipeline step executed",
			vec!["data.read".to_string()],
			SandboxProfile::ContainerRestricted,
		),
		WorkerReportTool::new(
			REVIEW_TOOL_NAME,
			"review-worker",
			"review checks completed",
			vec!["review.check".to_string()],
			SandboxProfile::ReadOnlyFs,
		),
		WorkerReportTool::new(
			GENERAL_TOOL_NAME,
			"generic-worker",
			"generic execution completed",
			Vec::new(),
			SandboxProfile::NoIsolation,
		),
	] {
		runtime
			.register_tool(tool)
			.expect("default runtime tools must register successfully");
	}
	runtime
}

pub(crate) fn build_llm_tool_runtime(router: Arc<LlmRouter>) -> ToolRuntime {
	let mut runtime = ToolRuntime::default();
	for tool in [
		PromptedLlmTool::new(
			RESEARCH_TOOL_NAME,
			"research-worker",
			"You are Roku's research worker. Produce grounded intermediate findings in plain text for downstream use. Never expose chain-of-thought, hidden reasoning, or internal runtime details.",
			vec!["information.read".to_string()],
			SandboxProfile::PythonResearch,
			RiskTier::Medium,
			Arc::clone(&router),
		),
		PromptedLlmTool::new(
			DATA_TOOL_NAME,
			"data-worker",
			"You are Roku's data worker. Produce the requested data-processing or synthesis result in plain text. Never expose chain-of-thought, hidden reasoning, or internal runtime details.",
			vec!["data.read".to_string()],
			SandboxProfile::ContainerRestricted,
			RiskTier::Medium,
			Arc::clone(&router),
		),
		PromptedLlmTool::new(
			REVIEW_TOOL_NAME,
			"review-worker",
			"You are Roku's review worker. Produce a concise review or validation conclusion in plain text. Never expose chain-of-thought, hidden reasoning, or internal runtime details.",
			vec!["review.check".to_string()],
			SandboxProfile::ReadOnlyFs,
			RiskTier::High,
			Arc::clone(&router),
		),
		PromptedLlmTool::new(
			GENERAL_TOOL_NAME,
			"generic-worker",
			"You are Roku. Produce only the final user-facing reply in plain text. Never reveal hidden reasoning, analysis steps, or internal runtime details. If trusted runtime context provides current date or time, treat it as ground truth.",
			Vec::new(),
			SandboxProfile::NoIsolation,
			RiskTier::Medium,
			router,
		),
	] {
		runtime
			.register_tool(tool)
			.expect("llm runtime tools must register successfully");
	}
	runtime
}

#[derive(Clone)]
struct WorkerReportTool {
	descriptor: ToolDescriptor,
	worker_id: &'static str,
	message: &'static str,
}

impl WorkerReportTool {
	fn new(
		name: &str,
		worker_id: &'static str,
		message: &'static str,
		required_capabilities: Vec<String>,
		sandbox_profile: SandboxProfile,
	) -> Self {
		Self {
			descriptor: tool_descriptor(name, required_capabilities, sandbox_profile, 5_000),
			worker_id,
			message,
		}
	}
}

impl Tool for WorkerReportTool {
	fn descriptor(&self) -> ToolDescriptor {
		self.descriptor.clone()
	}

	fn invoke(&self, request: ToolInvocationRequest) -> Result<Value, ToolFailure> {
		let input = request_input(&request)?;
		Ok(json!({
			"worker_id": self.worker_id,
			"message": self.message,
			"task_id": input.task_id,
			"node_id": input.node_id,
			"goal": input.goal,
			"summary": input.summary,
			"budget_tokens": input.budget_tokens,
			"time_budget_ms": input.time_budget_ms,
			"attempt": request.attempt,
			"invocation_key": request.invocation_key,
		}))
	}
}

#[derive(Clone)]
struct PromptedLlmTool {
	descriptor: ToolDescriptor,
	worker_id: &'static str,
	system_prompt: &'static str,
	risk_tier: RiskTier,
	router: Arc<LlmRouter>,
}

impl PromptedLlmTool {
	fn new(
		name: &str,
		worker_id: &'static str,
		system_prompt: &'static str,
		required_capabilities: Vec<String>,
		sandbox_profile: SandboxProfile,
		risk_tier: RiskTier,
		router: Arc<LlmRouter>,
	) -> Self {
		Self {
			descriptor: tool_descriptor(
				name,
				required_capabilities,
				sandbox_profile,
				LLM_TOOL_TIMEOUT_MS,
			),
			worker_id,
			system_prompt,
			risk_tier,
			router,
		}
	}
}

impl Tool for PromptedLlmTool {
	fn descriptor(&self) -> ToolDescriptor {
		self.descriptor.clone()
	}

	fn invoke(&self, request: ToolInvocationRequest) -> Result<Value, ToolFailure> {
		let input = request_input(&request)?;
		if let Some(answer) = direct_runtime_answer(input.goal) {
			log_runtime_output(
				"used deterministic runtime answer",
				[
					("worker_id", self.worker_id.to_string()),
					("node_id", input.node_id.to_string()),
				],
			);
			return Ok(json!({
				"worker_id": self.worker_id,
				"message": answer,
				"raw_message": Value::Null,
				"task_id": input.task_id,
				"node_id": input.node_id,
				"goal": input.goal,
				"summary": input.summary,
				"provider": "runtime-context",
				"model_id": "deterministic",
				"prompt_tokens": 0,
				"output_tokens": 0,
				"latency_ms": 0,
				"attempt": request.attempt,
				"invocation_key": request.invocation_key,
			}));
		}
		let prompt = user_visible_prompt(&input, self.worker_id, &request.invocation_key);

		let response = self
			.router
			.generate(&GenerationRequest {
				system_prompt: Some(self.system_prompt.to_string()),
				prompt,
				expected_output_tokens: input.budget_tokens.min(512),
				risk_tier: self.risk_tier,
				preferred_provider: None,
				budget_tokens_remaining: input.budget_tokens,
				budget_cost_remaining_usd: 1.0,
			})
			.map_err(llm_failure)?;
		let message = finalize_llm_message(self.worker_id, input.goal, &response.output);
		let raw_message = if message != response.output {
			log_runtime_output(
				"sanitized llm output before surfacing to downstream consumers",
				[
					("worker_id", self.worker_id.to_string()),
					("node_id", input.node_id.to_string()),
				],
			);
			Some(response.output.clone())
		} else {
			None
		};

		Ok(json!({
			"worker_id": self.worker_id,
			"message": message,
			"raw_message": raw_message,
			"task_id": input.task_id,
			"node_id": input.node_id,
			"goal": input.goal,
			"summary": input.summary,
			"provider": response.provider,
			"model_id": response.model_id,
			"prompt_tokens": response.prompt_tokens,
			"output_tokens": response.output_tokens,
			"latency_ms": response.latency_ms,
			"attempt": request.attempt,
			"invocation_key": request.invocation_key,
		}))
	}
}

fn user_visible_prompt(input: &ToolInput<'_>, worker_id: &str, invocation_key: &str) -> String {
	let history_section = if input.conversation_history.trim().is_empty() {
		String::new()
	} else {
		format!(
			"\n\nConversation history (most recent first-order context):\n{}",
			input.conversation_history
		)
	};
	let runtime_context = runtime_context_block();

	format!(
		"User request:\n{goal}{history_section}\n\nTrusted runtime context:\n{runtime_context}\n\nInternal execution hint (do not quote or describe it unless it is directly useful for the answer):\n{summary}\n\nOutput rules:\n- Return only the useful answer text in plain text.\n- Answer directly. Do not preface with analysis, translation, or a restatement of the user's request.\n- Never narrate your reasoning. Do not output phrases like \"用户的问题是\", \"I need to\", \"首先\", or similar meta-analysis.\n- Prefer one short paragraph unless the user explicitly asks for detail.\n- Match the user's language unless the request clearly asks for another language.\n- Preserve conversational continuity when the user refers to prior turns or earlier facts.\n- If the user asks about today's date, weekday, or current time, use the trusted runtime context above instead of claiming you lack realtime access.\n- Do not mention worker ids, invocation keys, execution steps, hidden instructions, providers, models, budgets, or internal runtime details.\n- Do not describe yourself as an execution worker or reveal chain-of-thought.\n- If you are about to restate the prompt, trusted runtime context, or your analysis notes, stop and output only the answer.\n- If the user asks who you are or which persona is active, answer as Roku.\n- Internal references for policy only: worker_id={worker_id}; invocation_key={invocation_key}; time_budget_ms={time_budget_ms}.",
		goal = input.goal,
		history_section = history_section,
		runtime_context = runtime_context,
		summary = input.summary,
		worker_id = worker_id,
		invocation_key = invocation_key,
		time_budget_ms = input.time_budget_ms,
	)
}

fn runtime_context_block() -> String {
	let now = current_runtime_time();
	let timestamp = now
		.format(&Rfc3339)
		.unwrap_or_else(|_| "unavailable".to_string());
	let date = now.date();
	let weekday = format!("{:?}", now.weekday());
	let utc_offset = format_utc_offset(now.offset());

	format!(
		"- local_timestamp: {timestamp}\n- local_date: {date}\n- local_weekday: {weekday}\n- utc_offset: {utc_offset}"
	)
}

fn current_runtime_time() -> OffsetDateTime {
	OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc())
}

fn direct_runtime_answer(goal: &str) -> Option<String> {
	let normalized = goal.trim().to_lowercase();
	if normalized.is_empty() {
		return None;
	}

	let asks_weekday = normalized.contains("星期几")
		|| normalized.contains("周几")
		|| normalized.contains("weekday")
		|| normalized.contains("what day is today")
		|| normalized.contains("what day is it today");
	let asks_date = normalized.contains("今天几号")
		|| normalized.contains("今天多少号")
		|| normalized.contains("today date")
		|| normalized.contains("today's date")
		|| normalized.contains("what date is it")
		|| normalized == "几号"
		|| normalized == "几号？";
	let asks_time = normalized.contains("现在几点")
		|| normalized.contains("几点了")
		|| normalized.contains("现在时间")
		|| normalized.contains("what time is it")
		|| normalized.contains("current time")
		|| normalized == "几点"
		|| normalized == "几点？";

	if !asks_weekday && !asks_date && !asks_time {
		return None;
	}

	let now = current_runtime_time();
	let date = now.date();
	let time = now.time();
	let weekday = chinese_weekday(now.weekday());
	let date_label = format!(
		"{:04}年{}月{}日",
		date.year(),
		u8::from(date.month()),
		date.day()
	);
	let time_label = format!("{:02}:{:02}", time.hour(), time.minute());

	match (asks_date, asks_weekday, asks_time) {
		(true, true, true) | (true, false, true) => Some(format!(
			"今天是{date_label}，{weekday}，现在是{time_label}。"
		)),
		(true, true, false) => Some(format!("今天是{date_label}，{weekday}。")),
		(true, false, false) => Some(format!("今天是{date_label}。")),
		(false, true, true) => Some(format!("今天是{weekday}，现在是{time_label}。")),
		(false, true, false) => Some(format!("{weekday}。")),
		(false, false, true) => Some(format!("现在是{time_label}。")),
		(false, false, false) => None,
	}
}

fn chinese_weekday(weekday: time::Weekday) -> &'static str {
	match weekday {
		time::Weekday::Monday => "星期一",
		time::Weekday::Tuesday => "星期二",
		time::Weekday::Wednesday => "星期三",
		time::Weekday::Thursday => "星期四",
		time::Weekday::Friday => "星期五",
		time::Weekday::Saturday => "星期六",
		time::Weekday::Sunday => "星期日",
	}
}

fn finalize_llm_message(worker_id: &str, goal: &str, output: &str) -> String {
	let trimmed = output.trim();
	if trimmed.is_empty() {
		return String::new();
	}
	if worker_id != "generic-worker" {
		return trimmed.to_string();
	}

	let sanitized = sanitize_final_reply(trimmed);
	if sanitized.is_empty() {
		direct_runtime_answer(goal).unwrap_or_else(|| trimmed.to_string())
	} else {
		sanitized
	}
}

fn sanitize_final_reply(output: &str) -> String {
	if !contains_prompt_leakage(output) {
		return strip_outer_quotes(output.trim()).to_string();
	}

	let candidates = output
		.lines()
		.map(str::trim)
		.filter(|line| !line.is_empty())
		.filter(|line| !is_meta_line(line))
		.filter_map(sanitized_candidate)
		.collect::<Vec<_>>();
	if let Some(candidate) = candidates.last() {
		return candidate.clone();
	}
	if let Some(candidate) = quoted_answer_candidate(output) {
		return strip_outer_quotes(candidate.trim()).trim().to_string();
	}

	strip_outer_quotes(output.trim()).to_string()
}

fn contains_prompt_leakage(output: &str) -> bool {
	let lowercase = output.to_lowercase();
	lowercase.contains("user request:")
		|| lowercase.contains("trusted runtime context")
		|| lowercase.contains("output rules:")
		|| lowercase.contains("internal execution hint")
		|| lowercase.contains("the user's request is")
		|| lowercase.contains("conversation history shows")
		|| lowercase.contains("first, the user's request is")
		|| lowercase.contains("from the trusted runtime context")
}

fn is_meta_line(line: &str) -> bool {
	let lowercase = line.to_lowercase();
	lowercase.starts_with("user request:")
		|| lowercase.starts_with("trusted runtime context:")
		|| lowercase.starts_with("internal execution hint")
		|| lowercase.starts_with("output rules:")
		|| lowercase.starts_with("conversation history")
		|| lowercase.starts_with("from the trusted runtime context")
		|| lowercase.starts_with("first, the user's request is")
		|| lowercase.starts_with("the user's request is")
		|| lowercase.starts_with("- local_")
		|| lowercase.starts_with("- the conversation history")
		|| lowercase.starts_with("- output")
		|| lowercase.contains("i should")
		|| lowercase.contains("i'll use")
		|| lowercase.contains("i'll output")
		|| lowercase.contains("do not narrate")
}

fn sanitized_candidate(line: &str) -> Option<String> {
	let quoted = quoted_answer_candidate(line).unwrap_or_else(|| line.to_string());
	let candidate = strip_outer_quotes(quoted.trim()).trim().to_string();
	if candidate.is_empty() || candidate.len() > 240 {
		return None;
	}
	Some(candidate)
}

fn quoted_answer_candidate(line: &str) -> Option<String> {
	for (open, close) in [('"', '"'), ('“', '”'), ('\'', '\''), ('‘', '’')] {
		if let Some(candidate) = between_last_pair(line, open, close) {
			return Some(candidate);
		}
	}
	None
}

fn between_last_pair(value: &str, open: char, close: char) -> Option<String> {
	let end = value.rfind(close)?;
	let start = value[..end].rfind(open)?;
	if start >= end {
		return None;
	}
	Some(value[start + open.len_utf8()..end].to_string())
}

fn strip_outer_quotes(value: &str) -> &str {
	let trimmed = value.trim();
	if trimmed.len() >= 2 {
		let first = trimmed.chars().next().unwrap_or_default();
		let last = trimmed.chars().last().unwrap_or_default();
		if matches!(
			(first, last),
			('"', '"') | ('\'', '\'') | ('“', '”') | ('‘', '’')
		) {
			return &trimmed[first.len_utf8()..trimmed.len() - last.len_utf8()];
		}
	}
	trimmed
}

fn log_runtime_output(message: &str, fields: impl IntoIterator<Item = (&'static str, String)>) {
	let mut record = LogRecord::new("roku-agent-runtime", LogLevel::Info, message);
	for (key, value) in fields {
		record = record.with_field(key, value);
	}
	let _ = emit_global_log(record);
}

fn format_utc_offset(offset: UtcOffset) -> String {
	let seconds = offset.whole_seconds();
	let sign = if seconds < 0 { '-' } else { '+' };
	let absolute_seconds = seconds.abs();
	let hours = absolute_seconds / 3600;
	let minutes = (absolute_seconds % 3600) / 60;
	format!("{sign}{hours:02}:{minutes:02}")
}

struct ToolInput<'a> {
	task_id: &'a str,
	node_id: &'a str,
	goal: &'a str,
	summary: &'a str,
	conversation_history: &'a str,
	budget_tokens: u64,
	time_budget_ms: u64,
}

fn request_input(request: &ToolInvocationRequest) -> Result<ToolInput<'_>, ToolFailure> {
	let Some(input) = request.input.as_object() else {
		return Err(ToolFailure::terminal("tool input must be a json object"));
	};

	Ok(ToolInput {
		task_id: input
			.get("task_id")
			.and_then(Value::as_str)
			.unwrap_or_default(),
		node_id: input
			.get("node_id")
			.and_then(Value::as_str)
			.unwrap_or_default(),
		goal: input
			.get("goal")
			.and_then(Value::as_str)
			.unwrap_or_default(),
		summary: input
			.get("summary")
			.and_then(Value::as_str)
			.unwrap_or_default(),
		conversation_history: input
			.get("conversation_history")
			.and_then(Value::as_str)
			.unwrap_or_default(),
		budget_tokens: input
			.get("budget_tokens")
			.and_then(Value::as_u64)
			.unwrap_or_default(),
		time_budget_ms: input
			.get("time_budget_ms")
			.and_then(Value::as_u64)
			.unwrap_or_default(),
	})
}

fn tool_descriptor(
	name: &str,
	required_capabilities: Vec<String>,
	sandbox_profile: SandboxProfile,
	timeout_ms: u64,
) -> ToolDescriptor {
	ToolDescriptor {
		name: name.to_string(),
		version: "1.0.0".to_string(),
		input_schema: ToolSchema {
			required_fields: vec![
				"task_id".to_string(),
				"node_id".to_string(),
				"goal".to_string(),
				"summary".to_string(),
				"conversation_history".to_string(),
				"budget_tokens".to_string(),
				"time_budget_ms".to_string(),
			],
		},
		output_schema: "result.v1".to_string(),
		required_capabilities,
		runtime_constraints: RuntimeConstraints {
			timeout_ms,
			max_retries: 0,
			retry_backoff_ms: 0,
			sandbox_profile,
			deterministic_hooks: true,
		},
	}
}

fn llm_failure(error: LlmAdapterError) -> ToolFailure {
	match error {
		LlmAdapterError::BudgetExceeded(message) => ToolFailure::terminal(message),
		LlmAdapterError::LatencyExceeded {
			latency_ms,
			max_latency_ms,
		} => ToolFailure::terminal(format!(
			"llm latency exceeded policy: latency={latency_ms}ms max={max_latency_ms}ms"
		)),
		LlmAdapterError::CircuitOpen {
			provider,
			retry_after_ms,
		} => ToolFailure::terminal(format!(
			"llm provider circuit is open for {provider}; retry after {retry_after_ms}ms"
		)),
		LlmAdapterError::NoEligibleModel => {
			ToolFailure::terminal("no eligible llm model for request")
		}
		LlmAdapterError::ProviderNotRegistered(provider) => {
			ToolFailure::terminal(format!("llm provider is not registered: {provider}"))
		}
		LlmAdapterError::ProviderCallFailed {
			provider,
			model_id,
			message,
		} => ToolFailure::terminal(format!(
			"llm provider call failed for {provider}/{model_id}: {message}"
		)),
	}
}

#[cfg(test)]
mod tests {
	use serde_json::json;

	use super::{
		direct_runtime_answer, request_input, runtime_context_block, sanitize_final_reply,
		user_visible_prompt,
	};
	use roku_tool_runtime::{SandboxProfile, ToolInvocationRequest};

	#[test]
	fn runtime_context_block_contains_date_and_weekday() {
		let context = runtime_context_block();
		assert!(context.contains("local_date"));
		assert!(context.contains("local_weekday"));
		assert!(context.contains("utc_offset"));
	}

	#[test]
	fn user_visible_prompt_includes_runtime_context_and_direct_answer_rules() {
		let request = ToolInvocationRequest {
			invocation_key: "invoke-1".to_string(),
			input: json!({
				"task_id": "task-1",
				"node_id": "node-1",
				"goal": "今天是星期几？",
				"summary": "Execute primary action",
				"conversation_history": "user: 你好",
				"budget_tokens": 2048_u64,
				"time_budget_ms": 45_000_u64
			}),
			attempt: 1,
			sandbox_profile: SandboxProfile::NoIsolation,
		};

		let input = request_input(&request).expect("tool input should parse");
		let prompt = user_visible_prompt(&input, "generic-worker", "invoke-1");

		assert!(prompt.contains("Trusted runtime context"));
		assert!(prompt.contains("Never narrate your reasoning"));
		assert!(prompt.contains("use the trusted runtime context above"));
		assert!(prompt.contains("Conversation history"));
	}

	#[test]
	fn direct_runtime_answer_returns_grounded_weekday() {
		let answer = direct_runtime_answer("今天周几？").expect("runtime answer should exist");
		assert!(answer.starts_with("星期"));
	}

	#[test]
	fn direct_runtime_answer_returns_grounded_date_and_time() {
		let answer =
			direct_runtime_answer("今天几号？现在几点了？").expect("runtime answer should exist");
		assert!(answer.contains("今天是"));
		assert!(answer.contains("现在是"));
	}

	#[test]
	fn sanitize_final_reply_collapses_prompt_leakage() {
		let output = r#"First, the user's request is: "今天周几？"

From the trusted runtime context:
- local_weekday: Sunday

So, I'll output: "星期日""#;
		assert_eq!(sanitize_final_reply(output), "星期日");
	}
}
