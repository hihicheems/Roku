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

//! OpenAI OAuth 2.0 PKCE flow.
//!
//! Orchestrates:
//! 1. PKCE code generation
//! 2. Authorization URL construction
//! 3. Local loopback callback server (via `tiny_http`)
//! 4. Code-for-token exchange
//! 5. RFC 8693 token-exchange to obtain an API key
//! 6. JWT id_token claim extraction

use std::io;
use std::net::TcpListener;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::Rng;
use serde::Deserialize;

use super::AuthError;
use super::pkce;
use super::storage::IdTokenClaims;

// ---------------------------------------------------------------------------
// Public result types
// ---------------------------------------------------------------------------

/// Successful result from the full OAuth flow.
#[derive(Debug, Clone)]
pub struct OAuthResult {
	/// The API key obtained via RFC 8693 token-exchange (`sk-*`).
	pub api_key: String,
	pub refresh_token: String,
	pub id_token_claims: IdTokenClaims,
}

/// Result from a token-refresh operation.
#[derive(Debug, Clone)]
#[allow(dead_code)] // Will be consumed when automatic token refresh is wired.
pub(crate) struct RefreshResult {
	pub access_token: Option<String>,
	pub refresh_token: Option<String>,
	pub id_token: Option<String>,
}

// ---------------------------------------------------------------------------
// Wire types (serde only, not exposed)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct TokenResponse {
	access_token: Option<String>,
	refresh_token: Option<String>,
	id_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ExchangeResponse {
	/// The RFC 8693 token-exchange returns the API key in `access_token`.
	access_token: Option<String>,
}

// ---------------------------------------------------------------------------
// Authorization URL
// ---------------------------------------------------------------------------

const AUTH_BASE: &str = "https://auth.openai.com/oauth/authorize";
const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const SCOPE: &str = "openid profile email offline_access api.connectors.read api.connectors.invoke";

/// Build the authorization URL that the user must visit.
pub fn build_authorize_url(
	client_id: &str,
	redirect_uri: &str,
	code_challenge: &str,
	state: &str,
) -> String {
	use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
	// Encode everything except unreserved chars (RFC 3986 Section 2.3).
	const ENCODE_SET: &AsciiSet = &CONTROLS
		.add(b' ')
		.add(b'!')
		.add(b'#')
		.add(b'$')
		.add(b'%')
		.add(b'&')
		.add(b'\'')
		.add(b'(')
		.add(b')')
		.add(b'+')
		.add(b',')
		.add(b'/')
		.add(b':')
		.add(b';')
		.add(b'=')
		.add(b'?')
		.add(b'@')
		.add(b'[')
		.add(b']');
	let enc = |s: &str| utf8_percent_encode(s, ENCODE_SET).to_string();
	format!(
		"{AUTH_BASE}?\
		 response_type=code\
		 &client_id={}\
		 &redirect_uri={}\
		 &scope={}\
		 &code_challenge={}\
		 &code_challenge_method=S256\
		 &state={}\
		 &id_token_add_organizations=true\
		 &codex_cli_simplified_flow=true",
		enc(client_id),
		enc(redirect_uri),
		enc(SCOPE),
		enc(code_challenge),
		enc(state),
	)
}

// ---------------------------------------------------------------------------
// Callback server
// ---------------------------------------------------------------------------

const PREFERRED_PORT: u16 = 1455;

/// Attempt to bind on `PREFERRED_PORT`, then fall back to OS-assigned port.
fn bind_callback_listener() -> io::Result<TcpListener> {
	TcpListener::bind(("127.0.0.1", PREFERRED_PORT))
		.or_else(|_| TcpListener::bind(("127.0.0.1", 0)))
}

/// Extract a query-string parameter value from a URL string, percent-decoding it.
fn extract_callback_param(url: &str, key: &str) -> Option<String> {
	let query = url.split_once('?')?.1;
	for pair in query.split('&') {
		if let Some((k, v)) = pair.split_once('=')
			&& k == key
		{
			return Some(
				percent_encoding::percent_decode_str(v)
					.decode_utf8_lossy()
					.into_owned(),
			);
		}
	}
	None
}

// ---------------------------------------------------------------------------
// HTTP helpers
// ---------------------------------------------------------------------------

async fn post_form(
	client: &reqwest::Client,
	url: &str,
	params: &[(&str, &str)],
) -> Result<reqwest::Response, AuthError> {
	client
		.post(url)
		.form(params)
		.send()
		.await
		.map_err(|e| AuthError::Http(format!("POST {url}: {e}")))
}

async fn post_json<T: serde::Serialize>(
	client: &reqwest::Client,
	url: &str,
	body: &T,
) -> Result<reqwest::Response, AuthError> {
	client
		.post(url)
		.json(body)
		.send()
		.await
		.map_err(|e| AuthError::Http(format!("POST {url}: {e}")))
}

// ---------------------------------------------------------------------------
// Token operations
// ---------------------------------------------------------------------------

/// Exchange an authorization code for tokens.
async fn exchange_code_for_tokens(
	client: &reqwest::Client,
	client_id: &str,
	code: &str,
	redirect_uri: &str,
	code_verifier: &str,
) -> Result<TokenResponse, AuthError> {
	let resp = post_form(
		client,
		TOKEN_URL,
		&[
			("grant_type", "authorization_code"),
			("code", code),
			("redirect_uri", redirect_uri),
			("client_id", client_id),
			("code_verifier", code_verifier),
		],
	)
	.await?;

	let status = resp.status();
	let body = resp
		.text()
		.await
		.map_err(|e| AuthError::Http(format!("read token body: {e}")))?;
	if !status.is_success() {
		// Sanitize: do not leak raw response body (may contain tokens or PII).
		let hint = extract_error_hint(&body);
		return Err(AuthError::Http(format!(
			"token exchange failed ({status}){hint}"
		)));
	}
	serde_json::from_str::<TokenResponse>(&body)
		.map_err(|e| AuthError::Http(format!("parse token response: {e}")))
}

/// Attempt RFC 8693 token-exchange to obtain an API key from an id_token.
///
/// Returns `None` if the exchange is not supported for this client/account
/// (e.g. 401). The caller should fall back to the OAuth access_token from
/// the code-for-token step.
async fn try_exchange_id_token_for_api_key(
	client: &reqwest::Client,
	client_id: &str,
	id_token: &str,
) -> Option<String> {
	let resp = post_form(
		client,
		TOKEN_URL,
		&[
			(
				"grant_type",
				"urn:ietf:params:oauth:grant-type:token-exchange",
			),
			("client_id", client_id),
			("requested_token", "openai-api-key"),
			("subject_token", id_token),
			(
				"subject_token_type",
				"urn:ietf:params:oauth:token-type:id_token",
			),
		],
	)
	.await
	.ok()?;

	if !resp.status().is_success() {
		return None;
	}
	let body = resp.text().await.ok()?;
	let exchange: ExchangeResponse = serde_json::from_str(&body).ok()?;
	exchange.access_token
}

// ---------------------------------------------------------------------------
// JWT claim extraction
// ---------------------------------------------------------------------------

/// Decode the payload segment of a JWT and extract the fields we care about.
///
/// **Trust model**: The id_token is received over TLS directly from
/// `auth.openai.com` during the code-for-token exchange. We do NOT verify
/// the JWT signature (would require fetching JWKS — deferred as follow-up).
/// We DO validate `iss` and `exp` as basic sanity checks. The claims are
/// used only for display (email) and as input to the token-exchange endpoint;
/// the actual API key comes from a separate server response.
pub fn parse_id_token_claims(id_token: &str) -> IdTokenClaims {
	let payload = id_token.split('.').nth(1).unwrap_or("");
	let bytes = URL_SAFE_NO_PAD.decode(payload).unwrap_or_default();
	let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
		return IdTokenClaims::default();
	};

	// Basic claims validation (without full JWKS signature verification).
	if let Some(iss) = value.get("iss").and_then(|v| v.as_str()) {
		let normalized = iss.trim_end_matches('/');
		if normalized != "https://auth.openai.com" {
			eprintln!(
				"[warn] id_token issuer mismatch: expected https://auth.openai.com, got {iss}"
			);
			return IdTokenClaims::default();
		}
	}
	if let Some(exp) = value.get("exp").and_then(|v| v.as_u64()) {
		let now = std::time::SystemTime::now()
			.duration_since(std::time::UNIX_EPOCH)
			.map(|d| d.as_secs())
			.unwrap_or(0);
		if now > exp {
			eprintln!("[warn] id_token has expired (exp={exp}, now={now})");
		}
	}

	let email = value
		.get("email")
		.and_then(|v| v.as_str())
		.map(str::to_string);

	// OpenAI-specific extension claim.
	let auth_obj = value.get("https://api.openai.com/auth");
	let user_id = auth_obj
		.and_then(|o| o.get("chatgpt_user_id"))
		.and_then(|v| v.as_str())
		.map(str::to_string);
	let account_id = auth_obj
		.and_then(|o| o.get("chatgpt_account_id"))
		.and_then(|v| v.as_str())
		.map(str::to_string);

	IdTokenClaims {
		email,
		user_id,
		account_id,
	}
}

// ---------------------------------------------------------------------------
// Random state generation
// ---------------------------------------------------------------------------

fn generate_state() -> String {
	let mut rng = rand::rng();
	let mut bytes = [0u8; 32];
	rng.fill(&mut bytes);
	URL_SAFE_NO_PAD.encode(bytes)
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Run the full OpenAI OAuth PKCE flow.
///
/// Opens the user's browser to the authorization URL, starts a local callback
/// server, exchanges the code for tokens, then performs an RFC 8693
/// token-exchange to obtain the actual API key (`sk-*`).
pub async fn run_openai_oauth(client_id: &str) -> Result<OAuthResult, AuthError> {
	let pkce = pkce::generate();
	let state = generate_state();

	// Bind the listener before we open the browser so the port is ready.
	let listener =
		bind_callback_listener().map_err(|e| AuthError::Callback(format!("bind listener: {e}")))?;
	let port = listener
		.local_addr()
		.map_err(|e| AuthError::Callback(format!("local_addr: {e}")))?
		.port();
	let redirect_uri = format!("http://localhost:{port}/auth/callback");

	let auth_url = build_authorize_url(client_id, &redirect_uri, &pkce.code_challenge, &state);

	open::that(&auth_url).map_err(|e| AuthError::Callback(format!("open browser: {e}")))?;

	// Hand the already-bound listener to tiny_http.
	let server = tiny_http::Server::from_listener(listener, None)
		.map_err(|e| AuthError::Callback(format!("tiny_http server: {e}")))?;

	let code = tokio::task::block_in_place(|| receive_callback(&server, &state))?;

	let http_client = reqwest::Client::new();

	let token_resp = exchange_code_for_tokens(
		&http_client,
		client_id,
		&code,
		&redirect_uri,
		&pkce.code_verifier,
	)
	.await?;

	let access_token = token_resp
		.access_token
		.ok_or_else(|| AuthError::Http("token exchange returned no access_token".to_string()))?;
	let id_token = token_resp
		.id_token
		.ok_or_else(|| AuthError::Http("token exchange returned no id_token".to_string()))?;
	let refresh_token = token_resp
		.refresh_token
		.ok_or_else(|| AuthError::Http("token exchange returned no refresh_token".to_string()))?;

	// Try to obtain an API key (sk-*) via RFC 8693 token-exchange.
	// This is optional — some accounts/clients don't support it.
	// Fall back to the OAuth access_token from step 1.
	let api_key = try_exchange_id_token_for_api_key(&http_client, client_id, &id_token).await;
	let usable_token = api_key.unwrap_or_else(|| {
		eprintln!("[oauth] API key exchange not available, using OAuth access token.");
		access_token
	});

	let id_token_claims = parse_id_token_claims(&id_token);

	Ok(OAuthResult {
		api_key: usable_token,
		refresh_token,
		id_token_claims,
	})
}

/// Refresh an existing OAuth token using the stored refresh_token.
pub async fn refresh_openai_token(
	client_id: &str,
	refresh_token: &str,
) -> Result<RefreshResult, AuthError> {
	let http_client = reqwest::Client::new();

	#[derive(serde::Serialize)]
	struct RefreshBody<'a> {
		client_id: &'a str,
		grant_type: &'static str,
		refresh_token: &'a str,
	}

	let resp = post_json(
		&http_client,
		TOKEN_URL,
		&RefreshBody {
			client_id,
			grant_type: "refresh_token",
			refresh_token,
		},
	)
	.await?;

	let status = resp.status();
	let body = resp
		.text()
		.await
		.map_err(|e| AuthError::Http(format!("read refresh body: {e}")))?;
	if !status.is_success() {
		let hint = extract_error_hint(&body);
		return Err(AuthError::Http(format!(
			"token refresh failed ({status}){hint}"
		)));
	}
	let token_resp: TokenResponse = serde_json::from_str(&body)
		.map_err(|e| AuthError::Http(format!("parse refresh response: {e}")))?;

	Ok(RefreshResult {
		access_token: token_resp.access_token,
		refresh_token: token_resp.refresh_token,
		id_token: token_resp.id_token,
	})
}

// ---------------------------------------------------------------------------
// Callback reception helper (extracted for testability)
// ---------------------------------------------------------------------------

fn receive_callback(server: &tiny_http::Server, expected_state: &str) -> Result<String, AuthError> {
	use std::time::{Duration, Instant};

	use crossterm::event::{self, Event, KeyCode, KeyModifiers};
	use crossterm::terminal;

	eprintln!("[setup] Waiting for browser callback... (press Esc to cancel)");

	let deadline = Instant::now() + Duration::from_secs(300);

	let _ = terminal::enable_raw_mode();
	let _guard = OAuthRawModeGuard;

	loop {
		// Non-blocking key check — detect Esc or Ctrl-C immediately.
		if event::poll(Duration::from_millis(0)).unwrap_or(false)
			&& let Ok(Event::Key(key)) = event::read()
			&& key.kind != event::KeyEventKind::Release
			&& (key.code == KeyCode::Esc
				|| (key.code == KeyCode::Char('c')
					&& key.modifiers.contains(KeyModifiers::CONTROL)))
		{
			return Err(AuthError::Callback("cancelled by user".to_string()));
		}

		// Short HTTP poll so we can check keys frequently.
		match server.recv_timeout(Duration::from_millis(200)) {
			Ok(Some(request)) => {
				return process_callback_request(request, expected_state);
			}
			Ok(None) => {
				if Instant::now() >= deadline {
					return Err(AuthError::Callback(
						"OAuth callback timed out after 300 seconds. Try /login again.".to_string(),
					));
				}
			}
			Err(e) => {
				return Err(AuthError::Callback(format!("recv: {e}")));
			}
		}
	}
}

/// Process the HTTP callback request and extract the authorization code.
fn process_callback_request(
	request: tiny_http::Request,
	expected_state: &str,
) -> Result<String, AuthError> {
	let url = request.url().to_string();
	let code = extract_callback_param(&url, "code");
	let state = extract_callback_param(&url, "state");

	let oauth_error = extract_callback_param(&url, "error");
	let oauth_error_desc = extract_callback_param(&url, "error_description");

	let outcome: Result<String, AuthError> = if let Some(err) = oauth_error {
		let detail = oauth_error_desc.unwrap_or_default();
		Err(AuthError::Callback(format!(
			"authorization denied: {err}{}",
			if detail.is_empty() {
				String::new()
			} else {
				format!(" — {detail}")
			}
		)))
	} else {
		match (code, state) {
			(Some(c), Some(s)) if s == expected_state => Ok(c),
			(_, Some(s)) if s != expected_state => Err(AuthError::Callback(
				"state mismatch — possible CSRF".to_string(),
			)),
			_ => Err(AuthError::Callback(
				"missing code or state in callback".to_string(),
			)),
		}
	};

	let html = if outcome.is_ok() {
		SUCCESS_HTML
	} else {
		ERROR_HTML
	};
	let response = tiny_http::Response::from_string(html).with_header(
		"Content-Type: text/html; charset=utf-8"
			.parse::<tiny_http::Header>()
			.expect("static header"),
	);
	let _ = request.respond(response);
	outcome
}

/// RAII guard for raw mode in the OAuth callback wait loop.
struct OAuthRawModeGuard;

impl Drop for OAuthRawModeGuard {
	fn drop(&mut self) {
		let _ = crossterm::terminal::disable_raw_mode();
	}
}

// ---------------------------------------------------------------------------
// Static HTML pages
// ---------------------------------------------------------------------------

const SUCCESS_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head><meta charset="utf-8"><title>Roku — Signed In</title></head>
<body>
  <h2>Authentication successful.</h2>
  <p>You may close this tab and return to the terminal.</p>
</body>
</html>"#;

const ERROR_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head><meta charset="utf-8"><title>Roku — Auth Error</title></head>
<body>
  <h2>Authentication failed.</h2>
  <p>An error occurred during sign-in. Please check the terminal for details.</p>
</body>
</html>"#;

/// Extract a safe, short hint from an OAuth error response body.
/// Only exposes the `error` and `error_description` fields (standard OAuth 2.0
/// error response), never raw tokens or PII.
fn extract_error_hint(body: &str) -> String {
	let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
		return String::new();
	};
	let error = value.get("error").and_then(|v| v.as_str()).unwrap_or("");
	let description = value
		.get("error_description")
		.and_then(|v| v.as_str())
		.unwrap_or("");
	if error.is_empty() && description.is_empty() {
		return String::new();
	}
	if description.is_empty() {
		return format!(": {error}");
	}
	format!(": {error} — {description}")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn authorize_url_contains_required_params() {
		let url = build_authorize_url(
			"client-123",
			"http://localhost:1455/auth/callback",
			"challenge-abc",
			"state-xyz",
		);
		assert!(
			url.starts_with("https://auth.openai.com/oauth/authorize?"),
			"wrong base"
		);
		assert!(url.contains("response_type=code"), "missing response_type");
		assert!(url.contains("client_id=client-123"), "missing client_id");
		assert!(
			url.contains("code_challenge=challenge-abc"),
			"missing code_challenge"
		);
		assert!(url.contains("code_challenge_method=S256"), "missing method");
		assert!(url.contains("state=state-xyz"), "missing state");
		assert!(url.contains("scope="), "missing scope");
		assert!(
			url.contains("codex_cli_simplified_flow=true"),
			"missing simplified_flow"
		);
	}

	#[test]
	fn extract_callback_param_parses_code_and_state() {
		let url = "/auth/callback?code=auth-code-123&state=state-abc";
		assert_eq!(
			extract_callback_param(url, "code"),
			Some("auth-code-123".to_string())
		);
		assert_eq!(
			extract_callback_param(url, "state"),
			Some("state-abc".to_string())
		);
		assert_eq!(extract_callback_param(url, "missing"), None);
	}

	#[test]
	fn parse_id_token_claims_extracts_email_and_ids() {
		// Build a fake JWT: header.payload.signature
		let claims = serde_json::json!({
			"email": "user@example.com",
			"https://api.openai.com/auth": {
				"chatgpt_user_id": "uid-42",
				"chatgpt_account_id": "acc-99"
			}
		});
		let payload = URL_SAFE_NO_PAD.encode(claims.to_string().as_bytes());
		let fake_jwt = format!("header.{payload}.sig");

		let parsed = parse_id_token_claims(&fake_jwt);
		assert_eq!(parsed.email.as_deref(), Some("user@example.com"));
		assert_eq!(parsed.user_id.as_deref(), Some("uid-42"));
		assert_eq!(parsed.account_id.as_deref(), Some("acc-99"));
	}

	#[test]
	fn parse_id_token_claims_handles_missing_fields() {
		let claims = serde_json::json!({ "sub": "1234" });
		let payload = URL_SAFE_NO_PAD.encode(claims.to_string().as_bytes());
		let fake_jwt = format!("header.{payload}.sig");

		let parsed = parse_id_token_claims(&fake_jwt);
		assert!(parsed.email.is_none());
		assert!(parsed.user_id.is_none());
		assert!(parsed.account_id.is_none());
	}

	#[test]
	fn parse_id_token_claims_returns_default_for_garbage() {
		let parsed = parse_id_token_claims("not.a.jwt");
		assert!(parsed.email.is_none());
	}
}
