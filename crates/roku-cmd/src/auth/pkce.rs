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

//! PKCE (Proof Key for Code Exchange, RFC 7636) code generation.
//!
//! Generates a `code_verifier` from 64 random bytes encoded as base64url
//! without padding, and a `code_challenge` that is `SHA-256(code_verifier)`
//! also encoded as base64url without padding.

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::RngExt;
use sha2::{Digest, Sha256};

/// A PKCE verifier/challenge pair.
#[derive(Debug, Clone)]
pub struct PkceCodes {
	/// The raw verifier sent in the token-exchange step.
	pub code_verifier: String,
	/// The SHA-256 digest of the verifier, sent in the authorisation URL.
	pub code_challenge: String,
}

/// Generate a fresh [`PkceCodes`] pair using the S256 method.
///
/// The verifier is built from 64 cryptographically random bytes encoded as
/// base64url without padding. The challenge is `BASE64URL(SHA256(verifier))`.
pub fn generate() -> PkceCodes {
	let mut rng = rand::rng();
	let mut bytes = [0u8; 64];
	rng.fill(&mut bytes);

	let code_verifier = URL_SAFE_NO_PAD.encode(bytes);

	let digest = Sha256::digest(code_verifier.as_bytes());
	let code_challenge = URL_SAFE_NO_PAD.encode(digest);

	PkceCodes {
		code_verifier,
		code_challenge,
	}
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
	use super::*;

	fn is_base64url_no_pad(s: &str) -> bool {
		s.chars()
			.all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
			&& !s.contains('=')
	}

	#[test]
	fn generates_valid_base64url_no_pad() {
		let codes = generate();
		assert!(
			is_base64url_no_pad(&codes.code_verifier),
			"code_verifier is not base64url-no-pad: {:?}",
			codes.code_verifier
		);
		assert!(
			is_base64url_no_pad(&codes.code_challenge),
			"code_challenge is not base64url-no-pad: {:?}",
			codes.code_challenge
		);
	}

	#[test]
	fn verifier_and_challenge_are_different() {
		let codes = generate();
		assert_ne!(
			codes.code_verifier, codes.code_challenge,
			"verifier and challenge must differ"
		);
	}

	#[test]
	fn challenge_is_sha256_of_verifier() {
		let codes = generate();
		let digest = Sha256::digest(codes.code_verifier.as_bytes());
		let expected = URL_SAFE_NO_PAD.encode(digest);
		assert_eq!(codes.code_challenge, expected);
	}

	#[test]
	fn two_calls_produce_distinct_verifiers() {
		let a = generate();
		let b = generate();
		assert_ne!(a.code_verifier, b.code_verifier);
	}
}
