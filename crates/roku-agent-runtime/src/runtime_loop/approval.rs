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

//! Tool execution approval gate.
//!
//! Determines whether a tool invocation should proceed, ask for user
//! confirmation, or be denied. Callers (CLI, Telegram) provide a gate
//! implementation that matches their interaction model.

use serde_json::Value;

/// Result of an approval check.
#[derive(Debug, Clone)]
pub enum ApprovalDecision {
	/// Proceed without asking.
	Approve,
	/// Deny execution; the message is returned to the model as the tool result.
	Deny(String),
}

/// Gate called before executing a tool that may require approval.
///
/// Implementations may block (e.g., waiting for user input on stdin).
/// The turn loop calls this inside `tokio::task::block_in_place`.
pub trait ToolApprovalGate: Send + Sync {
	/// Check whether the given tool invocation should proceed.
	fn check(&self, tool_name: &str, arguments: &Value) -> ApprovalDecision;
}

/// Default gate that auto-approves everything (current behavior).
pub struct AutoApproveGate;

impl ToolApprovalGate for AutoApproveGate {
	fn check(&self, _tool_name: &str, _arguments: &Value) -> ApprovalDecision {
		ApprovalDecision::Approve
	}
}

/// Risk-level-based gate that classifies tools by their default risk.
/// Write operations require explicit approval via a callback.
pub struct RiskBasedGate<F: Fn(&str, &Value) -> ApprovalDecision + Send + Sync> {
	prompt_fn: F,
}

impl<F: Fn(&str, &Value) -> ApprovalDecision + Send + Sync> RiskBasedGate<F> {
	pub fn new(prompt_fn: F) -> Self {
		Self { prompt_fn }
	}
}

impl<F: Fn(&str, &Value) -> ApprovalDecision + Send + Sync> ToolApprovalGate for RiskBasedGate<F> {
	fn check(&self, tool_name: &str, arguments: &Value) -> ApprovalDecision {
		match classify_tool_risk(tool_name, arguments) {
			ToolRiskLevel::Safe => ApprovalDecision::Approve,
			ToolRiskLevel::RequiresApproval => (self.prompt_fn)(tool_name, arguments),
			ToolRiskLevel::Denied => ApprovalDecision::Deny(format!(
				"Tool `{tool_name}` is denied by the current security policy."
			)),
		}
	}
}

/// Risk classification for a tool invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolRiskLevel {
	/// Read-only or informational — auto-approve.
	Safe,
	/// Writes, mutations, or external actions — ask user.
	RequiresApproval,
	/// Explicitly blocked.
	Denied,
}

/// Classify a tool invocation by risk level.
///
/// Default classification:
/// - Safe: fs.read_text, fs.glob, fs.exists, fs.inspect, fs.list_dir, fs.find,
///   fs.grep, web.search, web.fetch, table.*, final_answer, ask_user, fail
/// - RequiresApproval: fs.write, fs.edit, command.run, python.run, skill.*
/// - command.run is further classified by the command content:
///   destructive patterns (rm -rf /, dd if=, etc.) → Denied
pub fn classify_tool_risk(tool_name: &str, arguments: &Value) -> ToolRiskLevel {
	match tool_name {
		// Read-only tools — always safe
		"fs.read_text" | "fs.glob" | "fs.exists" | "fs.inspect" | "fs.list_dir" | "fs.find"
		| "fs.grep" | "web.search" | "web.fetch" | "table.inspect" | "table.list_sheets"
		| "table.preview" | "table.schema" | "final_answer" | "ask_user" | "fail" => ToolRiskLevel::Safe,

		// command.run — classify by command content
		"command.run" => classify_command_risk(arguments),

		// Write tools — require approval
		"fs.write" | "fs.edit" | "python.run" | "skill.ensure_installed" | "skill.execute" => {
			ToolRiskLevel::RequiresApproval
		}

		// Unknown tools — require approval (safe default)
		_ => ToolRiskLevel::RequiresApproval,
	}
}

/// Classify command.run risk by inspecting the command text.
fn classify_command_risk(arguments: &Value) -> ToolRiskLevel {
	let command = arguments
		.get("command")
		.and_then(Value::as_str)
		.unwrap_or("");

	// Destructive patterns → denied
	let lower = command.to_lowercase();
	for pattern in DENIED_COMMAND_PATTERNS {
		if lower.contains(pattern) {
			return ToolRiskLevel::Denied;
		}
	}

	// Read-only command patterns → safe
	for pattern in SAFE_COMMAND_PATTERNS {
		if lower.starts_with(pattern) {
			return ToolRiskLevel::Safe;
		}
	}

	// Default: require approval for commands
	ToolRiskLevel::RequiresApproval
}

/// Command patterns that are always denied.
const DENIED_COMMAND_PATTERNS: &[&str] = &[
	"rm -rf /",
	"rm -rf ~",
	"mkfs.",
	"dd if=",
	"> /dev/sd",
	":(){ :|:& };:",
];

/// Command patterns that are considered safe (read-only).
const SAFE_COMMAND_PATTERNS: &[&str] = &[
	"ls",
	"cat ",
	"head ",
	"tail ",
	"wc ",
	"file ",
	"echo ",
	"printf ",
	"git status",
	"git log",
	"git diff",
	"git show",
	"git branch",
	"git remote",
	"git rev-parse",
	"git describe",
	"cargo check",
	"cargo test",
	"cargo build",
	"cargo clippy",
	"cargo fmt",
	"cargo doc",
	"grep ",
	"rg ",
	"find ",
	"which ",
	"type ",
	"pwd",
	"whoami",
	"uname",
	"date",
	"env",
	"gh repo view",
	"gh issue list",
	"gh pr list",
	"gh pr view",
	"just ",
	"make -n",
	"node --version",
	"python3 --version",
	"rustc --version",
	"curl -s",
	"wget -q",
];

#[cfg(test)]
mod tests {
	use super::*;
	use serde_json::json;

	// --- classify_tool_risk ---

	#[test]
	fn classify_safe_read_tools() {
		for tool in &[
			"fs.read_text",
			"fs.glob",
			"fs.exists",
			"fs.inspect",
			"fs.list_dir",
			"fs.find",
			"fs.grep",
			"web.search",
			"web.fetch",
			"table.inspect",
			"table.list_sheets",
			"table.preview",
			"table.schema",
			"final_answer",
			"ask_user",
			"fail",
		] {
			assert_eq!(
				classify_tool_risk(tool, &Value::Null),
				ToolRiskLevel::Safe,
				"expected Safe for {tool}"
			);
		}
	}

	#[test]
	fn classify_write_tools_require_approval() {
		for tool in &[
			"fs.write",
			"fs.edit",
			"python.run",
			"skill.ensure_installed",
			"skill.execute",
		] {
			assert_eq!(
				classify_tool_risk(tool, &Value::Null),
				ToolRiskLevel::RequiresApproval,
				"expected RequiresApproval for {tool}"
			);
		}
	}

	#[test]
	fn classify_unknown_tool_requires_approval() {
		assert_eq!(
			classify_tool_risk("some.unknown_tool", &Value::Null),
			ToolRiskLevel::RequiresApproval
		);
	}

	#[test]
	fn classify_command_run_safe_patterns() {
		let safe_commands = &[
			"git status",
			"git log --oneline",
			"cargo check --all",
			"ls -la",
			"cat Cargo.toml",
			"pwd",
			"whoami",
		];
		for cmd in safe_commands {
			let args = json!({ "command": cmd });
			assert_eq!(
				classify_tool_risk("command.run", &args),
				ToolRiskLevel::Safe,
				"expected Safe for command: {cmd}"
			);
		}
	}

	#[test]
	fn classify_command_run_risky_requires_approval() {
		let risky_commands = &[
			"git commit -m 'test'",
			"git push",
			"touch foo.txt",
			"mkdir bar",
		];
		for cmd in risky_commands {
			let args = json!({ "command": cmd });
			assert_eq!(
				classify_tool_risk("command.run", &args),
				ToolRiskLevel::RequiresApproval,
				"expected RequiresApproval for command: {cmd}"
			);
		}
	}

	#[test]
	fn classify_command_run_denied_patterns() {
		let denied_commands = &[
			"rm -rf /home",
			"mkfs.ext4 /dev/sda",
			"dd if=/dev/urandom of=/dev/sda",
		];
		for cmd in denied_commands {
			let args = json!({ "command": cmd });
			assert_eq!(
				classify_tool_risk("command.run", &args),
				ToolRiskLevel::Denied,
				"expected Denied for command: {cmd}"
			);
		}
	}

	// --- AutoApproveGate ---

	#[test]
	fn auto_approve_gate_always_approves() {
		let gate = AutoApproveGate;
		let result = gate.check("fs.write", &json!({ "path": "/tmp/test" }));
		assert!(matches!(result, ApprovalDecision::Approve));

		let result = gate.check("command.run", &json!({ "command": "rm -rf /" }));
		assert!(matches!(result, ApprovalDecision::Approve));
	}

	// --- RiskBasedGate ---

	#[test]
	fn risk_based_gate_auto_approves_safe_tools() {
		let gate = RiskBasedGate::new(|_tool, _args| {
			panic!("prompt_fn should not be called for safe tools");
		});
		let result = gate.check("fs.read_text", &json!({ "path": "/tmp/foo" }));
		assert!(matches!(result, ApprovalDecision::Approve));
	}

	#[test]
	fn risk_based_gate_calls_prompt_for_write_tools() {
		let gate = RiskBasedGate::new(|tool_name, _args| {
			ApprovalDecision::Deny(format!("denied {tool_name}"))
		});
		let result = gate.check(
			"fs.write",
			&json!({ "path": "/tmp/test", "content": "hello" }),
		);
		match result {
			ApprovalDecision::Deny(msg) => assert!(msg.contains("fs.write")),
			ApprovalDecision::Approve => panic!("expected Deny for write tool"),
		}
	}

	#[test]
	fn risk_based_gate_denies_explicitly_denied_commands() {
		let gate = RiskBasedGate::new(|_tool, _args| {
			panic!("prompt_fn should not be called for denied tools");
		});
		let result = gate.check("command.run", &json!({ "command": "rm -rf /var" }));
		match result {
			ApprovalDecision::Deny(msg) => {
				assert!(msg.contains("command.run") && msg.contains("security policy"))
			}
			ApprovalDecision::Approve => panic!("expected Deny for denied command"),
		}
	}
}
