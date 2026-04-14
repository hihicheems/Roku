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

//! Slash command dispatch and definitions.

pub(crate) mod session;
pub(crate) mod setup;

use crate::input::{CommandEntry, SubCommandEntry};

/// Available slash commands with descriptions for the popup.
pub(crate) fn slash_commands() -> Vec<CommandEntry> {
	vec![
		CommandEntry {
			name: "approve",
			description: "Toggle auto-approve for tool execution",
			sub_commands: None,
		},
		CommandEntry {
			name: "clear",
			description: "Clear conversation history",
			sub_commands: None,
		},
		CommandEntry {
			name: "compact",
			description: "Compact conversation history",
			sub_commands: None,
		},
		CommandEntry {
			name: "debug",
			description: "Toggle debug log output",
			sub_commands: None,
		},
		CommandEntry {
			name: "exit",
			description: "Exit the REPL",
			sub_commands: None,
		},
		CommandEntry {
			name: "help",
			description: "Show available commands",
			sub_commands: None,
		},
		CommandEntry {
			name: "login",
			description: "Sign in to a provider",
			sub_commands: None,
		},
		CommandEntry {
			name: "model",
			description: "Select LLM model",
			sub_commands: None,
		},
		CommandEntry {
			name: "logout",
			description: "Sign out current provider",
			sub_commands: None,
		},
		CommandEntry {
			name: "plan",
			description: "Enter plan mode (read-only tools)",
			sub_commands: None,
		},
		CommandEntry {
			name: "plan-execute",
			description: "Exit plan mode and resume normal execution",
			sub_commands: None,
		},
		CommandEntry {
			name: "session",
			description: "Manage chat sessions",
			sub_commands: Some(vec![
				SubCommandEntry {
					name: "list",
					description: "List all sessions",
				},
				SubCommandEntry {
					name: "switch",
					description: "Switch to a different session",
				},
				SubCommandEntry {
					name: "new",
					description: "Create a new session",
				},
			]),
		},
		CommandEntry {
			name: "switch",
			description: "Switch LLM provider",
			sub_commands: None,
		},
		CommandEntry {
			name: "thinking",
			description: "Set thinking/reasoning effort",
			sub_commands: None,
		},
	]
}
