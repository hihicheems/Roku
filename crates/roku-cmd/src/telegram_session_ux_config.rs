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

//! CLI-side Telegram session UX defaults.
//!
//! These values influence Telegram control-surface rendering and continuity window sizing inside
//! `roku-cmd`. They are intentionally kept as "typed default only" configuration:
//!
//! - they are worth centralizing and naming
//! - they are not yet operator-facing deployment knobs
//! - they do not redefine Roku-owned provider-neutral session semantics

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TelegramSessionUxConfig {
	/// Number of recent short-term continuity turns loaded for Telegram session snapshots/requests.
	pub short_term_history_turn_limit: usize,
	/// Number of sessions shown in one `/sessions` page.
	pub sessions_page_size: usize,
	/// Maximum number of Unicode characters retained in one latest-activity preview.
	pub latest_activity_preview_chars: usize,
}

impl Default for TelegramSessionUxConfig {
	fn default() -> Self {
		Self {
			short_term_history_turn_limit: 12,
			sessions_page_size: 5,
			latest_activity_preview_chars: 96,
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn typed_defaults_are_non_zero_and_stable() {
		let config = TelegramSessionUxConfig::default();
		assert_eq!(config.short_term_history_turn_limit, 12);
		assert_eq!(config.sessions_page_size, 5);
		assert_eq!(config.latest_activity_preview_chars, 96);
	}
}
