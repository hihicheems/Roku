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
/// OpenAI 3-tier: cache_write == input (cache writes are not separately priced).
pub static KNOWN_COST_PROFILES: &[ModelCostProfile] = &[
	// --- Anthropic ---
	ModelCostProfile {
		model_id_prefix: "claude-opus-4",
		provider: "anthropic",
		input_per_mtok: 15.00,
		cache_write_per_mtok: 18.75,
		cache_read_per_mtok: 1.50,
		output_per_mtok: 75.00,
		max_output_tokens: 32768,
		as_of: "2026-04",
	},
	ModelCostProfile {
		model_id_prefix: "claude-sonnet-4",
		provider: "anthropic",
		input_per_mtok: 3.00,
		cache_write_per_mtok: 3.75,
		cache_read_per_mtok: 0.30,
		output_per_mtok: 15.00,
		max_output_tokens: 65536,
		as_of: "2026-04",
	},
	ModelCostProfile {
		model_id_prefix: "claude-haiku-4",
		provider: "anthropic",
		input_per_mtok: 0.80,
		cache_write_per_mtok: 1.00,
		cache_read_per_mtok: 0.08,
		output_per_mtok: 4.00,
		max_output_tokens: 16384,
		as_of: "2026-04",
	},
	// --- OpenAI ---
	// For OpenAI, cache_write_per_mtok == input_per_mtok (cache writes are not separately billed).
	// IMPORTANT: more-specific prefixes must come before less-specific ones
	// (e.g. "gpt-4o-mini" before "gpt-4o") because lookup uses first-match.
	ModelCostProfile {
		model_id_prefix: "gpt-4o-mini",
		provider: "openai",
		input_per_mtok: 0.15,
		cache_write_per_mtok: 0.15,
		cache_read_per_mtok: 0.075,
		output_per_mtok: 0.60,
		max_output_tokens: 16384,
		as_of: "2026-04",
	},
	ModelCostProfile {
		model_id_prefix: "gpt-4o",
		provider: "openai",
		input_per_mtok: 2.50,
		cache_write_per_mtok: 2.50,
		cache_read_per_mtok: 1.25,
		output_per_mtok: 10.00,
		max_output_tokens: 16384,
		as_of: "2026-04",
	},
	ModelCostProfile {
		model_id_prefix: "gpt-4.1",
		provider: "openai",
		input_per_mtok: 2.00,
		cache_write_per_mtok: 2.00,
		cache_read_per_mtok: 0.50,
		output_per_mtok: 8.00,
		max_output_tokens: 32768,
		as_of: "2026-04",
	},
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
	ModelCostProfile {
		model_id_prefix: "o3",
		provider: "openai",
		input_per_mtok: 2.00,
		cache_write_per_mtok: 2.00,
		cache_read_per_mtok: 1.00,
		output_per_mtok: 8.00,
		max_output_tokens: 100000,
		as_of: "2026-04",
	},
	ModelCostProfile {
		model_id_prefix: "gpt-4.1-mini",
		provider: "openai",
		input_per_mtok: 0.40,
		cache_write_per_mtok: 0.40,
		cache_read_per_mtok: 0.10,
		output_per_mtok: 1.60,
		max_output_tokens: 32768,
		as_of: "2026-04",
	},
];

/// Returns true if the model is a reasoning-capable model.
///
/// OpenAI reasoning models are identified by model ID prefix (o1, o3, o4).
/// For Anthropic, reasoning is determined at request time by `thinking_effort`
/// being set — not by model ID — so this function does not cover Anthropic models.
pub fn is_reasoning_model(model_id: &str) -> bool {
	// OpenAI reasoning models
	model_id.starts_with("o1") || model_id.starts_with("o3") || model_id.starts_with("o4")
}

/// Look up the cost profile for a model by prefix matching.
///
/// Returns the first profile whose `model_id_prefix` is a prefix of `model_id`.
/// Returns `None` when no profile matches.
pub fn lookup_cost_profile(model_id: &str) -> Option<&'static ModelCostProfile> {
	KNOWN_COST_PROFILES
		.iter()
		.find(|p| model_id.starts_with(p.model_id_prefix))
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
			.expect("should match claude-sonnet-4");
		// 1_000_000 prompt tokens (200k uncached, 500k cache_write, 300k cache_read), 100k output
		let cost = compute_turn_cost_usd(profile, 1_000_000, 100_000, 500_000, 300_000);
		// uncached_input = 1_000_000 - (500_000 + 300_000) = 200_000
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
	fn test_openai_gpt41_cost() {
		let profile = lookup_cost_profile("gpt-4.1").expect("should match gpt-4.1");
		// For OpenAI cache_write == input price
		// 500_000 prompt (200k uncached, 300k cached), 0 cache_creation, 50k output
		let cost = compute_turn_cost_usd(profile, 500_000, 50_000, 0, 300_000);
		// uncached_input = 500_000 - (0 + 300_000) = 200_000
		// uncached_input_usd = 200_000 * 2.00 / 1_000_000 = 0.40
		// cache_write_usd   = 0 * 2.00 / 1_000_000 = 0.00
		// cache_read_usd    = 300_000 * 0.50 / 1_000_000 = 0.15
		// output_usd        = 50_000 * 8.00 / 1_000_000 = 0.40
		// total             = 0.95
		assert!((cost.uncached_input_usd - 0.40).abs() < 1e-9);
		assert!((cost.cache_write_usd - 0.00).abs() < 1e-9);
		assert!((cost.cache_read_usd - 0.15).abs() < 1e-9);
		assert!((cost.output_usd - 0.40).abs() < 1e-9);
		assert!((cost.total_usd - 0.95).abs() < 1e-9);
	}

	#[test]
	fn test_lookup_prefix_match() {
		let profile = lookup_cost_profile("claude-sonnet-4-5-20250929");
		assert!(profile.is_some());
		let p = profile.unwrap();
		assert_eq!(p.model_id_prefix, "claude-sonnet-4");
		assert_eq!(p.provider, "anthropic");
	}

	#[test]
	fn test_unknown_model_returns_none() {
		assert!(lookup_cost_profile("unknown-model-xyz-9999").is_none());
		assert!(lookup_cost_profile("").is_none());
	}

	#[test]
	fn test_is_reasoning_model() {
		// OpenAI reasoning models
		assert!(is_reasoning_model("o1"));
		assert!(is_reasoning_model("o1-mini"));
		assert!(is_reasoning_model("o1-preview"));
		assert!(is_reasoning_model("o3"));
		assert!(is_reasoning_model("o3-mini"));
		assert!(is_reasoning_model("o4"));
		assert!(is_reasoning_model("o4-mini"));

		// Non-reasoning models
		assert!(!is_reasoning_model("gpt-4o"));
		assert!(!is_reasoning_model("gpt-4.1"));
		assert!(!is_reasoning_model("claude-sonnet-4"));
		assert!(!is_reasoning_model("claude-opus-4"));
		assert!(!is_reasoning_model(""));
		// "o" prefix alone does not match
		assert!(!is_reasoning_model("ollama-mistral"));
	}
}
