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

//! Authentication subsystem for Roku CLI.
//!
//! Provides:
//! - OAuth 2.0 PKCE flow for OpenAI (`run_openai_oauth`)
//! - Token refresh (`refresh_openai_token`)
//! - Credential persistence (`AuthStore`, `AuthFile`)
//! - PKCE code generation (`pkce::generate`)

#[allow(dead_code)] // OAuth helpers used in tests; refresh_openai_token is pending wiring.
pub mod oauth;
pub mod pkce;
pub mod storage;

pub(crate) use oauth::run_openai_oauth;
pub(crate) use storage::{AuthStore, CredentialEntry};

/// Errors produced by the auth subsystem.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
	/// A file-system operation on `auth.json` failed.
	#[error("storage error: {0}")]
	Storage(String),
	/// An HTTP request to the OAuth server failed.
	#[error("http error: {0}")]
	Http(String),
	/// The local callback server encountered an error.
	#[error("callback error: {0}")]
	Callback(String),
}
