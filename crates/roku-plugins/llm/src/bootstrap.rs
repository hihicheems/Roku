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

//! Shared bootstrap surface for the LLM plugin.
//!
//! This module owns provider-neutral selection types used by the startup
//! layer. It intentionally contains no transport, protocol, or credential
//! logic — each provider module remains responsible for its own HTTP
//! client, request shaping, and credential source. Keeping selection
//! decoupled from any single provider's internals lets future credential
//! sources (OAuth, TUI picker, per-session overrides) plug in without
//! rewriting the dispatcher.

use serde::Deserialize;

/// Which provider backs the live LLM runtime.
///
/// Deserialized from `[runtime.llm].provider` in `runtime.toml`. Missing
/// field defaults to [`LlmProviderKind::Openrouter`] for backward
/// compatibility with existing deployments.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LlmProviderKind {
	#[default]
	Openrouter,
	Anthropic,
	Openai,
}

impl LlmProviderKind {
	/// Stable, lowercase identifier used in logs and error messages.
	pub fn as_str(&self) -> &'static str {
		match self {
			Self::Openrouter => "openrouter",
			Self::Anthropic => "anthropic",
			Self::Openai => "openai",
		}
	}
}
