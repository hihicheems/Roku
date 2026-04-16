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

//! Per-model, per-tier cost lookup and computation.
//!
//! `KNOWN_COST_PROFILES` is the single source of truth for pricing.
//! No other file in the codebase should define token prices.

use crate::types::ModelCostProfile;

/// Static pricing table. All prices are USD per million tokens.
///
/// Anthropic 4-tier: input / cache_write / cache_read / output.
///     - cache_write (5min TTL) = 1.25× input; cache_read = 0.10× input.
///     OpenAI 3-tier: cache_write == input (cache writes are not separately priced);
///     cache_read discount varies by model family.
///
/// Sources (verified 2026-04-16):
///   - Anthropic: <https://docs.anthropic.com/en/docs/about-claude/pricing>
///   - OpenAI:    <https://developers.openai.com/api/docs/pricing>
///
/// Lookup uses longest-prefix-match, so entry ordering does not matter.
pub static KNOWN_COST_PROFILES: &[ModelCostProfile] = &[
	// ── Anthropic (active models only) ──────────────────────────────────
	// Opus 4.6 — $5 input, 1M context, 128k output
	ModelCostProfile {
		model_id_prefix: "claude-opus-4-6",
		provider: "anthropic",
		input_per_mtok: 5.00,
		cache_write_per_mtok: 6.25,
		cache_read_per_mtok: 0.50,
		output_per_mtok: 25.00,
		max_output_tokens: 128000,
		as_of: "2026-04",
	},
	// Opus 4.5 — same pricing as 4.6, 200k context, 64k output
	ModelCostProfile {
		model_id_prefix: "claude-opus-4-5",
		provider: "anthropic",
		input_per_mtok: 5.00,
		cache_write_per_mtok: 6.25,
		cache_read_per_mtok: 0.50,
		output_per_mtok: 25.00,
		max_output_tokens: 65536,
		as_of: "2026-04",
	},
	// Opus 4.1 — legacy pricing, retirement not before Aug 2026
	ModelCostProfile {
		model_id_prefix: "claude-opus-4-1",
		provider: "anthropic",
		input_per_mtok: 15.00,
		cache_write_per_mtok: 18.75,
		cache_read_per_mtok: 1.50,
		output_per_mtok: 75.00,
		max_output_tokens: 32768,
		as_of: "2026-04",
	},
	// Sonnet 4.6 — $3 input, 1M context, 64k output
	ModelCostProfile {
		model_id_prefix: "claude-sonnet-4-6",
		provider: "anthropic",
		input_per_mtok: 3.00,
		cache_write_per_mtok: 3.75,
		cache_read_per_mtok: 0.30,
		output_per_mtok: 15.00,
		max_output_tokens: 65536,
		as_of: "2026-04",
	},
	// Sonnet 4.5 — same pricing as 4.6, 200k context
	ModelCostProfile {
		model_id_prefix: "claude-sonnet-4-5",
		provider: "anthropic",
		input_per_mtok: 3.00,
		cache_write_per_mtok: 3.75,
		cache_read_per_mtok: 0.30,
		output_per_mtok: 15.00,
		max_output_tokens: 65536,
		as_of: "2026-04",
	},
	// Haiku 4.5 — $1 input, 200k context, 64k output
	ModelCostProfile {
		model_id_prefix: "claude-haiku-4-5",
		provider: "anthropic",
		input_per_mtok: 1.00,
		cache_write_per_mtok: 1.25,
		cache_read_per_mtok: 0.10,
		output_per_mtok: 5.00,
		max_output_tokens: 65536,
		as_of: "2026-04",
	},
	// ── OpenAI (active models only) ─────────────────────────────────────
	// For OpenAI, cache_write_per_mtok == input_per_mtok (cache writes are not separately billed).
	//
	// GPT-5.4 — flagship, 1.05M context, 128k output
	ModelCostProfile {
		model_id_prefix: "gpt-5.4-pro",
		provider: "openai",
		input_per_mtok: 30.00,
		cache_write_per_mtok: 30.00,
		cache_read_per_mtok: 3.00,
		output_per_mtok: 180.00,
		max_output_tokens: 128000,
		as_of: "2026-04",
	},
	ModelCostProfile {
		model_id_prefix: "gpt-5.4-nano",
		provider: "openai",
		input_per_mtok: 0.20,
		cache_write_per_mtok: 0.20,
		cache_read_per_mtok: 0.02,
		output_per_mtok: 1.25,
		max_output_tokens: 128000,
		as_of: "2026-04",
	},
	ModelCostProfile {
		model_id_prefix: "gpt-5.4-mini",
		provider: "openai",
		input_per_mtok: 0.75,
		cache_write_per_mtok: 0.75,
		cache_read_per_mtok: 0.075,
		output_per_mtok: 4.50,
		max_output_tokens: 128000,
		as_of: "2026-04",
	},
	ModelCostProfile {
		model_id_prefix: "gpt-5.4",
		provider: "openai",
		input_per_mtok: 2.50,
		cache_write_per_mtok: 2.50,
		cache_read_per_mtok: 0.25,
		output_per_mtok: 15.00,
		max_output_tokens: 128000,
		as_of: "2026-04",
	},
	// GPT-5.3 (codex / chat)
	ModelCostProfile {
		model_id_prefix: "gpt-5.3",
		provider: "openai",
		input_per_mtok: 1.75,
		cache_write_per_mtok: 1.75,
		cache_read_per_mtok: 0.175,
		output_per_mtok: 14.00,
		max_output_tokens: 128000,
		as_of: "2026-04",
	},
	// GPT-5.2 — former flagship, 400k context
	ModelCostProfile {
		model_id_prefix: "gpt-5.2",
		provider: "openai",
		input_per_mtok: 1.75,
		cache_write_per_mtok: 1.75,
		cache_read_per_mtok: 0.175,
		output_per_mtok: 14.00,
		max_output_tokens: 128000,
		as_of: "2026-04",
	},
	// GPT-5.1-codex-mini — lightweight codex, 400k context
	ModelCostProfile {
		model_id_prefix: "gpt-5.1-codex-mini",
		provider: "openai",
		input_per_mtok: 0.25,
		cache_write_per_mtok: 0.25,
		cache_read_per_mtok: 0.025,
		output_per_mtok: 2.00,
		max_output_tokens: 128000,
		as_of: "2026-04",
	},
	// GPT-5.1 — 400k context
	ModelCostProfile {
		model_id_prefix: "gpt-5.1",
		provider: "openai",
		input_per_mtok: 1.25,
		cache_write_per_mtok: 1.25,
		cache_read_per_mtok: 0.125,
		output_per_mtok: 10.00,
		max_output_tokens: 128000,
		as_of: "2026-04",
	},
	// GPT-5-mini — lightweight, 400k context
	ModelCostProfile {
		model_id_prefix: "gpt-5-mini",
		provider: "openai",
		input_per_mtok: 0.25,
		cache_write_per_mtok: 0.25,
		cache_read_per_mtok: 0.025,
		output_per_mtok: 2.00,
		max_output_tokens: 128000,
		as_of: "2026-04",
	},
	// GPT-5 — initial release, 400k context
	ModelCostProfile {
		model_id_prefix: "gpt-5",
		provider: "openai",
		input_per_mtok: 1.25,
		cache_write_per_mtok: 1.25,
		cache_read_per_mtok: 0.125,
		output_per_mtok: 10.00,
		max_output_tokens: 128000,
		as_of: "2026-04",
	},
	// o4-mini — reasoning, 200k context, 100k output
	ModelCostProfile {
		model_id_prefix: "o4-mini",
		provider: "openai",
		input_per_mtok: 1.10,
		cache_write_per_mtok: 1.10,
		cache_read_per_mtok: 0.275,
		output_per_mtok: 4.40,
		max_output_tokens: 100000,
		as_of: "2026-04",
	},
	// o3-mini — reasoning, 200k context, 100k output
	ModelCostProfile {
		model_id_prefix: "o3-mini",
		provider: "openai",
		input_per_mtok: 1.10,
		cache_write_per_mtok: 1.10,
		cache_read_per_mtok: 0.55,
		output_per_mtok: 4.40,
		max_output_tokens: 100000,
		as_of: "2026-04",
	},
	// o3 — reasoning, 200k context, 100k output
	ModelCostProfile {
		model_id_prefix: "o3",
		provider: "openai",
		input_per_mtok: 2.00,
		cache_write_per_mtok: 2.00,
		cache_read_per_mtok: 0.50,
		output_per_mtok: 8.00,
		max_output_tokens: 100000,
		as_of: "2026-04",
	},
	// o1 — legacy reasoning, 200k context, 100k output (o1-mini/preview retired)
	ModelCostProfile {
		model_id_prefix: "o1",
		provider: "openai",
		input_per_mtok: 15.00,
		cache_write_per_mtok: 15.00,
		cache_read_per_mtok: 7.50,
		output_per_mtok: 60.00,
		max_output_tokens: 100000,
		as_of: "2026-04",
	},
];

/// Returns true if the model is a reasoning-capable model.
///
/// OpenAI reasoning models are identified by model ID prefix (o1, o3, o4).
/// Note: o1-mini and o1-preview were retired, but `o1` itself is still active.
/// GPT-5.x models support reasoning tokens via `reasoning_effort`, but they are
/// not classified as "reasoning models" in the traditional sense — their pricing
/// structure is standard (input/cached/output), not the o-series pattern.
/// For Anthropic, reasoning is determined at request time by `thinking_effort`
/// being set — not by model ID — so this function does not cover Anthropic models.
pub fn is_reasoning_model(model_id: &str) -> bool {
	model_id.starts_with("o1") || model_id.starts_with("o3") || model_id.starts_with("o4")
}

/// Look up the cost profile for a model by longest-prefix matching.
///
/// Iterates all entries and returns the profile with the longest
/// `model_id_prefix` that matches `model_id`. This eliminates the
/// ordering dependency that plagued the earlier first-match approach
/// (e.g. `gpt-4o-mini` accidentally matching the `gpt-4o` entry).
///
/// Returns `None` when no profile matches.
pub fn lookup_cost_profile(model_id: &str) -> Option<&'static ModelCostProfile> {
	KNOWN_COST_PROFILES
		.iter()
		.filter(|p| model_id.starts_with(p.model_id_prefix))
		.max_by_key(|p| p.model_id_prefix.len())
}

/// Per-tier cost breakdown for a single LLM turn.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnCost {
	pub uncached_input_usd: f64,
	pub cache_write_usd: f64,
	pub cache_read_usd: f64,
	pub output_usd: f64,
	pub total_usd: f64,
}

/// Compute the per-tier cost for a single LLM turn.
///
/// `prompt_tokens` is the total input token count as reported by the provider.
/// `cache_creation_tokens` and `cache_read_tokens` come from the provider's
/// cache-usage fields; they are `0` when the provider did not report cache activity.
pub fn compute_turn_cost_usd(
	profile: &ModelCostProfile,
	prompt_tokens: u64,
	output_tokens: u64,
	cache_creation_tokens: u64,
	cache_read_tokens: u64,
) -> TurnCost {
	let uncached_input_tokens =
		prompt_tokens.saturating_sub(cache_creation_tokens.saturating_add(cache_read_tokens));
	let uncached_input_usd = uncached_input_tokens as f64 * profile.input_per_mtok / 1_000_000.0;
	let cache_write_usd = cache_creation_tokens as f64 * profile.cache_write_per_mtok / 1_000_000.0;
	let cache_read_usd = cache_read_tokens as f64 * profile.cache_read_per_mtok / 1_000_000.0;
	let output_usd = output_tokens as f64 * profile.output_per_mtok / 1_000_000.0;
	let total_usd = uncached_input_usd + cache_write_usd + cache_read_usd + output_usd;
	TurnCost {
		uncached_input_usd,
		cache_write_usd,
		cache_read_usd,
		output_usd,
		total_usd,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_anthropic_sonnet_cost() {
		let profile = lookup_cost_profile("claude-sonnet-4-5-20250929")
			.expect("should match claude-sonnet-4-5");
		assert_eq!(profile.input_per_mtok, 3.00);
		// 1M prompt (200k uncached, 500k cache_write, 300k cache_read), 100k output
		let cost = compute_turn_cost_usd(profile, 1_000_000, 100_000, 500_000, 300_000);
		// uncached_input_usd = 200_000 * 3.00 / 1_000_000 = 0.60
		// cache_write_usd   = 500_000 * 3.75 / 1_000_000 = 1.875
		// cache_read_usd    = 300_000 * 0.30 / 1_000_000 = 0.09
		// output_usd        = 100_000 * 15.00 / 1_000_000 = 1.50
		// total             = 4.065
		assert!((cost.uncached_input_usd - 0.60).abs() < 1e-9);
		assert!((cost.cache_write_usd - 1.875).abs() < 1e-9);
		assert!((cost.cache_read_usd - 0.09).abs() < 1e-9);
		assert!((cost.output_usd - 1.50).abs() < 1e-9);
		assert!((cost.total_usd - 4.065).abs() < 1e-9);
	}

	#[test]
	fn test_anthropic_opus_46_cost() {
		let profile = lookup_cost_profile("claude-opus-4-6").expect("should match claude-opus-4-6");
		assert_eq!(profile.input_per_mtok, 5.00);
		assert_eq!(profile.output_per_mtok, 25.00);
		assert_eq!(profile.max_output_tokens, 128000);
	}

	#[test]
	fn test_openai_gpt54_cost() {
		let profile = lookup_cost_profile("gpt-5.4").expect("should match gpt-5.4");
		assert_eq!(profile.input_per_mtok, 2.50);
		assert_eq!(profile.cache_read_per_mtok, 0.25);
		assert_eq!(profile.output_per_mtok, 15.00);
	}

	#[test]
	fn test_openai_gpt54_mini_cost() {
		let profile = lookup_cost_profile("gpt-5.4-mini").expect("should match gpt-5.4-mini");
		assert_eq!(profile.input_per_mtok, 0.75);
		// Must NOT match gpt-5.4 (longest prefix wins)
		assert_eq!(profile.model_id_prefix, "gpt-5.4-mini");
	}

	#[test]
	fn test_longest_prefix_match() {
		// gpt-5.4-mini must match the more specific "gpt-5.4-mini" entry, not "gpt-5.4"
		let p = lookup_cost_profile("gpt-5.4-mini-2026-01-01").unwrap();
		assert_eq!(p.model_id_prefix, "gpt-5.4-mini");

		// o3-mini must match "o3-mini", not "o3"
		let p = lookup_cost_profile("o3-mini-2025-01-31").unwrap();
		assert_eq!(p.model_id_prefix, "o3-mini");

		// claude-sonnet-4-6 must match "claude-sonnet-4-6", not "claude-sonnet-4-5"
		let p = lookup_cost_profile("claude-sonnet-4-6").unwrap();
		assert_eq!(p.model_id_prefix, "claude-sonnet-4-6");
	}

	#[test]
	fn test_unknown_model_returns_none() {
		assert!(lookup_cost_profile("unknown-model-xyz-9999").is_none());
		assert!(lookup_cost_profile("").is_none());
		// Retired models should not match
		assert!(lookup_cost_profile("claude-3-5-haiku-20241022").is_none());
		// Removed GPT-4 series should not match
		assert!(lookup_cost_profile("gpt-4o").is_none());
		assert!(lookup_cost_profile("gpt-4o-mini").is_none());
		assert!(lookup_cost_profile("gpt-4.1").is_none());
		assert!(lookup_cost_profile("gpt-4.1-mini").is_none());
		assert!(lookup_cost_profile("gpt-4.1-nano").is_none());
	}

	#[test]
	fn test_is_reasoning_model() {
		// Active reasoning models
		assert!(is_reasoning_model("o1"));
		assert!(is_reasoning_model("o3"));
		assert!(is_reasoning_model("o3-mini"));
		assert!(is_reasoning_model("o4-mini"));

		// Non-reasoning models (including GPT-5.x with reasoning_effort support)
		assert!(!is_reasoning_model("gpt-5.4"));
		assert!(!is_reasoning_model("gpt-4o"));
		assert!(!is_reasoning_model("gpt-4.1"));
		assert!(!is_reasoning_model("claude-sonnet-4-6"));
		assert!(!is_reasoning_model("claude-opus-4-6"));
		assert!(!is_reasoning_model(""));
		// "o" prefix alone does not match
		assert!(!is_reasoning_model("ollama-mistral"));
	}

	#[test]
	fn test_o4_mini_profile() {
		let profile = lookup_cost_profile("o4-mini").expect("should match o4-mini");
		assert_eq!(profile.input_per_mtok, 1.10);
		assert_eq!(profile.cache_read_per_mtok, 0.275);
		assert_eq!(profile.output_per_mtok, 4.40);
	}
}
