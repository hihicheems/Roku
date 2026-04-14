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

//! Retry utilities: exponential backoff with jitter.

use std::time::Duration;

use crate::types::{ProviderCallError, ProviderResiliencePolicy};

/// Compute backoff delay for the given attempt index.
///
/// Formula: `initial_backoff_ms × 2^attempt_index × uniform(0.9, 1.1)`,
/// capped at `max_backoff_ms`. If the error carries a `suggested_delay`,
/// that takes precedence.
pub fn backoff_for_attempt(
	attempt_index: usize,
	policy: &ProviderResiliencePolicy,
	error: Option<&ProviderCallError>,
) -> Duration {
	// Respect server-suggested delay if present, capped at max_backoff_ms to
	// prevent a malicious/misconfigured server from blocking indefinitely.
	if let Some(suggested) = error.and_then(|e| e.suggested_delay()) {
		let cap = Duration::from_millis(policy.max_backoff_ms);
		return suggested.min(cap);
	}

	if policy.initial_backoff_ms == 0 {
		return Duration::ZERO;
	}

	let multiplier = 2_u64.saturating_pow(u32::try_from(attempt_index).unwrap_or(u32::MAX));
	let base_ms = policy
		.initial_backoff_ms
		.saturating_mul(multiplier)
		.min(policy.max_backoff_ms);

	// Apply ±10% jitter using a simple deterministic-ish source.
	// We avoid pulling in `rand` — use the low bits of the current time.
	let nanos = std::time::SystemTime::now()
		.duration_since(std::time::UNIX_EPOCH)
		.unwrap_or_default()
		.subsec_nanos();
	// Map nanos to a jitter factor in [0.9, 1.1].
	let jitter_factor = 0.9 + (f64::from(nanos % 2001) / 10_000.0); // 0..2000 -> 0.0..0.2 -> 0.9..1.1
	let jittered_ms = (base_ms as f64 * jitter_factor) as u64;

	Duration::from_millis(jittered_ms)
}
