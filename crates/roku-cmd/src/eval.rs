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

//! Eval framework: run TOML-defined benchmark scenarios against the live runtime
//! and emit a pass/fail report to stdout.
//!
//! Each scenario defines a `query`, expected `answer_keywords` (case-insensitive
//! substring match), expected `tool_calls` (by tool name only), and a `max_steps`
//! budget. Scenarios are loaded from `.toml` files in a directory (default
//! `config/eval`).
//!
//! If no LLM provider is configured the service falls back to the deterministic
//! runtime, which cannot satisfy most live scenarios. Those scenarios are marked
//! SKIP rather than FAIL so the report remains actionable.

use std::collections::HashSet;
use std::path::Path;
use std::time::Instant;

use roku_agent_runtime::LoopEvent;
use tokio::sync::mpsc;

use crate::CommandError;
use crate::runtime::{
	ExecutionRequestOptions, build_live_runtime_service_from_env, next_cli_request_sequence,
};

// ---------------------------------------------------------------------------
// Scenario data types
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Deserialize)]
struct EvalScenarioFile {
	scenario: ScenarioMeta,
	expect: ScenarioExpectation,
}

#[derive(Debug, serde::Deserialize)]
struct ScenarioMeta {
	name: String,
	// Used by TOML deserialization; read but not surfaced in current reports.
	#[allow(dead_code)]
	description: String,
	query: String,
}

#[derive(Debug, serde::Deserialize)]
struct ScenarioExpectation {
	answer_keywords: Vec<String>,
	#[serde(default)]
	tool_calls: Vec<String>,
	#[serde(default = "default_max_steps")]
	max_steps: u32,
}

fn default_max_steps() -> u32 {
	5
}

// ---------------------------------------------------------------------------
// Outcome types
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum ScenarioOutcome {
	Pass {
		steps: u32,
		tools_called: usize,
		elapsed_ms: u64,
		total_tokens: u64,
	},
	Fail {
		reason: String,
	},
	Skip {
		reason: String,
	},
}

struct ScenarioResult {
	name: String,
	outcome: ScenarioOutcome,
}

// ---------------------------------------------------------------------------
// Core eval logic
// ---------------------------------------------------------------------------

/// Run all `.toml` scenario files in `scenarios_dir`, filtering by `filter` if set.
///
/// Reports are printed to stdout; logs go to stderr via the global log sink.
pub(crate) fn run_eval(
	rt: &tokio::runtime::Runtime,
	scenarios_dir: &str,
	filter: Option<&str>,
) -> Result<Option<String>, CommandError> {
	let dir = Path::new(scenarios_dir);
	if !dir.exists() {
		return Err(CommandError::Usage(format!(
			"eval scenarios directory not found: {}",
			dir.display()
		)));
	}

	let scenarios = load_scenarios(dir)?;
	if scenarios.is_empty() {
		return Ok(Some(format!(
			"No scenario TOML files found in {}",
			dir.display()
		)));
	}

	let filtered: Vec<EvalScenarioFile> = scenarios
		.into_iter()
		.filter(|s| filter.map(|f| s.scenario.name.contains(f)).unwrap_or(true))
		.collect();

	if filtered.is_empty() {
		return Ok(Some(format!(
			"No scenarios matched filter {:?}",
			filter.unwrap_or("")
		)));
	}

	// Detect whether the live runtime is available by attempting to bootstrap it
	// once. If bootstrap fails we set a skip_reason that applies to all scenarios
	// that need live LLM access.
	let live_available = is_live_runtime_available();

	let mut results: Vec<ScenarioResult> = Vec::with_capacity(filtered.len());
	for scenario_file in filtered {
		let result = run_single_scenario(rt, &scenario_file, live_available);
		results.push(result);
	}

	Ok(Some(format_report(&results)))
}

fn load_scenarios(dir: &Path) -> Result<Vec<EvalScenarioFile>, CommandError> {
	let mut out = Vec::new();
	let entries = std::fs::read_dir(dir).map_err(CommandError::Io)?;
	let mut paths: Vec<_> = entries
		.filter_map(|entry| entry.ok())
		.map(|e| e.path())
		.filter(|p| p.extension().and_then(|e| e.to_str()) == Some("toml"))
		.collect();
	paths.sort();

	for path in paths {
		let content = std::fs::read_to_string(&path).map_err(CommandError::Io)?;
		let scenario: EvalScenarioFile = toml::from_str(&content).map_err(|e| {
			CommandError::Usage(format!(
				"failed to parse eval scenario {}: {}",
				path.display(),
				e
			))
		})?;
		out.push(scenario);
	}
	Ok(out)
}

/// Returns `true` if the live runtime can be bootstrapped (i.e. an LLM provider
/// is configured). This is checked once and shared across all scenarios.
fn is_live_runtime_available() -> bool {
	// build_live_runtime_service_from_env is a blocking call; call it directly.
	match build_live_runtime_service_from_env() {
		Ok(service) => {
			use roku_agent_runtime::RuntimeExecutionMode;
			service.runtime_mode_report().effective == RuntimeExecutionMode::LiveReact
		}
		Err(_) => false,
	}
}

fn run_single_scenario(
	rt: &tokio::runtime::Runtime,
	scenario_file: &EvalScenarioFile,
	live_available: bool,
) -> ScenarioResult {
	let name = scenario_file.scenario.name.clone();
	let needs_tools = !scenario_file.expect.tool_calls.is_empty();

	// Scenarios that require tool calls need a live LLM provider.
	if needs_tools && !live_available {
		return ScenarioResult {
			name,
			outcome: ScenarioOutcome::Skip {
				reason: "no LLM provider configured".to_string(),
			},
		};
	}

	let start = Instant::now();

	let options = ExecutionRequestOptions {
		session_id: format!("eval-{}", &name),
		goal: scenario_file.scenario.query.clone(),
		generated_skill_root: None,
	};

	let outcome = rt.block_on(async {
		let (tx, mut rx) = mpsc::unbounded_channel::<LoopEvent>();

		let service = match tokio::task::block_in_place(build_live_runtime_service_from_env) {
			Ok(s) => s,
			Err(e) => {
				return ScenarioOutcome::Skip {
					reason: format!("runtime bootstrap failed: {e}"),
				};
			}
		};

		use roku_api_gateway::{Gateway, RawRequest};
		let gateway = Gateway;
		let request = gateway.normalize(
			RawRequest {
				session_id: options.session_id.clone(),
				goal: options.goal.clone(),
			},
			next_cli_request_sequence(),
		);

		let exec =
			service.execute_with_mode(request, roku_agent_runtime::RunMode::Normal, Some(&tx));

		// Collect events in a parallel task.
		let collector = tokio::spawn(async move {
			let mut tool_names: HashSet<String> = HashSet::new();
			let mut steps: u32 = 0;
			let mut total_tokens: u64 = 0;

			while let Some(event) = rx.recv().await {
				match event {
					LoopEvent::ToolStart { tool_name, .. } => {
						tool_names.insert(tool_name);
					}
					LoopEvent::StepComplete { step } => {
						steps = steps.max(step);
					}
					LoopEvent::TokenUsage {
						total_tokens: t, ..
					} => {
						total_tokens = t;
					}
					_ => {}
				}
			}
			(tool_names, steps, total_tokens)
		});

		let response = match exec.await {
			Ok(r) => r,
			Err(e) => {
				drop(tx);
				let _ = collector.await;
				return ScenarioOutcome::Fail {
					reason: format!("runtime error: {e}"),
				};
			}
		};
		drop(tx);

		let (tool_names, steps, total_tokens) = collector.await.unwrap_or_default();

		check_expectations(
			scenario_file,
			&response.message,
			&tool_names,
			steps,
			total_tokens,
		)
	});

	let elapsed_ms = start.elapsed().as_millis() as u64;

	// Attach timing to Pass outcomes.
	let outcome = if let ScenarioOutcome::Pass {
		steps,
		tools_called,
		total_tokens,
		..
	} = outcome
	{
		ScenarioOutcome::Pass {
			steps,
			tools_called,
			elapsed_ms,
			total_tokens,
		}
	} else {
		outcome
	};

	ScenarioResult { name, outcome }
}

fn check_expectations(
	scenario_file: &EvalScenarioFile,
	response_text: &str,
	tool_names: &HashSet<String>,
	steps: u32,
	total_tokens: u64,
) -> ScenarioOutcome {
	let expect = &scenario_file.expect;
	let lower_response = response_text.to_lowercase();

	// Check answer keywords (case-insensitive substring).
	if !expect.answer_keywords.is_empty() {
		let any_match = expect
			.answer_keywords
			.iter()
			.any(|kw| lower_response.contains(&kw.to_lowercase()));
		if !any_match {
			return ScenarioOutcome::Fail {
				reason: format!(
					"missing keyword(s) {:?} in response",
					expect.answer_keywords
				),
			};
		}
	}

	// Check expected tool calls (by name only, subset match: all expected must appear).
	for expected_tool in &expect.tool_calls {
		if !tool_names.contains(expected_tool.as_str()) {
			return ScenarioOutcome::Fail {
				reason: format!("expected tool call '{}' did not occur", expected_tool),
			};
		}
	}

	// Check step budget.
	if steps > expect.max_steps {
		return ScenarioOutcome::Fail {
			reason: format!("used {} steps, max allowed is {}", steps, expect.max_steps),
		};
	}

	ScenarioOutcome::Pass {
		steps,
		tools_called: tool_names.len(),
		elapsed_ms: 0, // filled in by the caller
		total_tokens,
	}
}

// ---------------------------------------------------------------------------
// Report formatting
// ---------------------------------------------------------------------------

fn format_report(results: &[ScenarioResult]) -> String {
	let mut out = String::new();

	out.push_str("Roku Eval Report\n");
	out.push_str("================\n");

	let mut passed = 0u32;
	let mut failed = 0u32;
	let mut skipped = 0u32;

	for r in results {
		match &r.outcome {
			ScenarioOutcome::Pass {
				steps,
				tools_called,
				elapsed_ms,
				total_tokens,
			} => {
				passed += 1;
				let elapsed_s = *elapsed_ms as f64 / 1000.0;
				out.push_str(&format!(
					"PASS  {:<24} ({} step{}, {} tool{}, {:.1}s, {} tokens)\n",
					r.name,
					steps,
					if *steps == 1 { "" } else { "s" },
					tools_called,
					if *tools_called == 1 { "" } else { "s" },
					elapsed_s,
					total_tokens,
				));
			}
			ScenarioOutcome::Fail { reason } => {
				failed += 1;
				out.push_str(&format!("FAIL  {:<24} {}\n", r.name, reason));
			}
			ScenarioOutcome::Skip { reason } => {
				skipped += 1;
				out.push_str(&format!("SKIP  {:<24} {}\n", r.name, reason));
			}
		}
	}

	out.push_str("================\n");
	out.push_str(&format!(
		"Passed: {}/{}  Skipped: {}  Failed: {}\n",
		passed,
		results.len(),
		skipped,
		failed,
	));
	out
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
	use super::*;

	fn make_scenario(
		name: &str,
		query: &str,
		answer_keywords: Vec<&str>,
		tool_calls: Vec<&str>,
		max_steps: u32,
	) -> EvalScenarioFile {
		EvalScenarioFile {
			scenario: ScenarioMeta {
				name: name.to_string(),
				description: String::new(),
				query: query.to_string(),
			},
			expect: ScenarioExpectation {
				answer_keywords: answer_keywords.into_iter().map(String::from).collect(),
				tool_calls: tool_calls.into_iter().map(String::from).collect(),
				max_steps,
			},
		}
	}

	#[test]
	fn keyword_match_case_insensitive() {
		let scenario = make_scenario("s", "q", vec!["Hello"], vec![], 5);
		let tools = HashSet::new();
		let outcome = check_expectations(&scenario, "hello world", &tools, 1, 0);
		assert!(matches!(outcome, ScenarioOutcome::Pass { .. }));
	}

	#[test]
	fn keyword_miss_fails() {
		let scenario = make_scenario("s", "q", vec!["xyz"], vec![], 5);
		let tools = HashSet::new();
		let outcome = check_expectations(&scenario, "no match here", &tools, 1, 0);
		assert!(matches!(outcome, ScenarioOutcome::Fail { .. }));
	}

	#[test]
	fn missing_tool_call_fails() {
		let scenario = make_scenario("s", "q", vec!["ok"], vec!["fs.list_dir"], 5);
		let tools = HashSet::new();
		let outcome = check_expectations(&scenario, "ok", &tools, 1, 0);
		assert!(
			matches!(outcome, ScenarioOutcome::Fail { reason } if reason.contains("fs.list_dir"))
		);
	}

	#[test]
	fn tool_call_present_passes() {
		let scenario = make_scenario("s", "q", vec!["ok"], vec!["fs.list_dir"], 5);
		let mut tools = HashSet::new();
		tools.insert("fs.list_dir".to_string());
		let outcome = check_expectations(&scenario, "ok", &tools, 1, 0);
		assert!(matches!(outcome, ScenarioOutcome::Pass { .. }));
	}

	#[test]
	fn step_budget_exceeded_fails() {
		let scenario = make_scenario("s", "q", vec!["ok"], vec![], 2);
		let tools = HashSet::new();
		let outcome = check_expectations(&scenario, "ok", &tools, 3, 0);
		assert!(matches!(outcome, ScenarioOutcome::Fail { reason } if reason.contains("3 steps")));
	}

	#[test]
	fn step_at_exact_budget_passes() {
		let scenario = make_scenario("s", "q", vec!["ok"], vec![], 3);
		let tools = HashSet::new();
		let outcome = check_expectations(&scenario, "ok", &tools, 3, 0);
		assert!(matches!(outcome, ScenarioOutcome::Pass { .. }));
	}

	#[test]
	fn empty_keywords_no_keyword_check() {
		let scenario = make_scenario("s", "q", vec![], vec![], 5);
		let tools = HashSet::new();
		let outcome = check_expectations(&scenario, "anything", &tools, 1, 0);
		assert!(matches!(outcome, ScenarioOutcome::Pass { .. }));
	}

	#[test]
	fn any_one_keyword_is_sufficient() {
		let scenario = make_scenario("s", "q", vec!["alpha", "beta", "gamma"], vec![], 5);
		let tools = HashSet::new();
		// Only "beta" appears in the response.
		let outcome = check_expectations(&scenario, "beta present", &tools, 1, 0);
		assert!(matches!(outcome, ScenarioOutcome::Pass { .. }));
	}

	#[test]
	fn format_report_summary_counts() {
		let results = vec![
			ScenarioResult {
				name: "a".to_string(),
				outcome: ScenarioOutcome::Pass {
					steps: 1,
					tools_called: 0,
					elapsed_ms: 100,
					total_tokens: 50,
				},
			},
			ScenarioResult {
				name: "b".to_string(),
				outcome: ScenarioOutcome::Fail {
					reason: "missing keyword".to_string(),
				},
			},
			ScenarioResult {
				name: "c".to_string(),
				outcome: ScenarioOutcome::Skip {
					reason: "no provider".to_string(),
				},
			},
		];
		let report = format_report(&results);
		assert!(report.contains("Passed: 1/3"));
		assert!(report.contains("Skipped: 1"));
		assert!(report.contains("Failed: 1"));
		assert!(report.contains("PASS"));
		assert!(report.contains("FAIL"));
		assert!(report.contains("SKIP"));
	}
}
