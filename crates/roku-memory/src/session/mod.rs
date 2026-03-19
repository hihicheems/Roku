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

//! Roku-owned session namespace.
//!
//! Transport-specific persistence remains adapter work; the session model
//! itself belongs to Roku's memory subsystem.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use roku_common_types::SessionPreferences;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Minimum allowed Unicode character count for one provider-neutral session name.
pub const SESSION_NAME_MIN_CHARS: usize = 1;
/// Maximum allowed Unicode character count for one provider-neutral session name.
pub const SESSION_NAME_MAX_CHARS: usize = 50;

/// Session-scoped transport/runtime continuity state.
///
/// This currently reuses [`SessionPreferences`] for wire compatibility while
/// the provider-neutral ownership moves into `roku-memory`.
pub type SessionState = SessionPreferences;

/// Error returned by session-state backends.
#[derive(Debug, Error)]
pub enum SessionStateError {
	#[error("session-state backend failed: {0}")]
	Backend(String),
}

/// Provider-neutral session descriptor owned by Roku's memory subsystem.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionDescriptor {
	pub session_id: String,
	pub name: String,
	pub created_at_unix_ms: i64,
	pub updated_at_unix_ms: i64,
}

/// Lightweight session list/status projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSummary {
	pub session_id: String,
	pub name: String,
	pub updated_at_unix_ms: i64,
}

impl From<&SessionDescriptor> for SessionSummary {
	fn from(value: &SessionDescriptor) -> Self {
		Self {
			session_id: value.session_id.clone(),
			name: value.name.clone(),
			updated_at_unix_ms: value.updated_at_unix_ms,
		}
	}
}

/// Request used to create one logical session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCreateRequest {
	#[serde(default)]
	pub requested_name: Option<String>,
}

/// Deletion modes for provider-neutral session management.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionDeleteMode {
	#[default]
	RetainLongTermMemory,
}

/// Errors produced by provider-neutral session management backends.
#[derive(Debug, Error)]
pub enum SessionManagementError {
	#[error("session validation failed: {0}")]
	Validation(String),
	#[error("session not found: {0}")]
	NotFound(String),
	#[error("session management unsupported: {0}")]
	Unsupported(String),
	#[error("session management backend failed: {0}")]
	Backend(String),
}

/// Normalizes and validates a provider-neutral session name.
pub fn normalize_session_name(value: &str) -> Result<String, SessionManagementError> {
	let normalized = value.trim();
	if normalized.is_empty() {
		return Err(SessionManagementError::Validation(
			"session name must not be empty".to_string(),
		));
	}

	let char_count = normalized.chars().count();
	if char_count > SESSION_NAME_MAX_CHARS {
		return Err(SessionManagementError::Validation(format!(
			"session name must be between {SESSION_NAME_MIN_CHARS} and {SESSION_NAME_MAX_CHARS} Unicode characters; got {char_count}"
		)));
	}

	Ok(normalized.to_string())
}

/// Resolves the final session name for a create request.
pub fn resolve_session_name(
	request: &SessionCreateRequest,
	session_id: &str,
) -> Result<String, SessionManagementError> {
	match request.requested_name.as_deref() {
		Some(name) => normalize_session_name(name),
		None => normalize_session_name(session_id),
	}
}

/// Provider-neutral session-state contract.
pub trait SessionStateBackend: Send {
	fn save_session_state(
		&mut self,
		session_id: &str,
		state: SessionState,
	) -> Result<(), SessionStateError>;

	fn load_session_state(
		&self,
		session_id: &str,
	) -> Result<Option<SessionState>, SessionStateError>;

	fn delete_session_state(&mut self, session_id: &str) -> Result<(), SessionStateError>;
}

/// Provider-neutral session management contract.
pub trait SessionManagementBackend: Send {
	fn create_session(
		&mut self,
		binding_id: &str,
		request: SessionCreateRequest,
	) -> Result<SessionDescriptor, SessionManagementError>;

	fn get_session(
		&self,
		binding_id: &str,
		session_id: &str,
	) -> Result<Option<SessionDescriptor>, SessionManagementError>;

	fn list_sessions(
		&self,
		binding_id: &str,
	) -> Result<Vec<SessionSummary>, SessionManagementError>;

	fn rename_session(
		&mut self,
		binding_id: &str,
		session_id: &str,
		new_name: &str,
	) -> Result<SessionDescriptor, SessionManagementError>;

	fn delete_session(
		&mut self,
		binding_id: &str,
		session_id: &str,
		mode: SessionDeleteMode,
	) -> Result<(), SessionManagementError>;

	fn get_active_session(
		&self,
		binding_id: &str,
	) -> Result<Option<SessionDescriptor>, SessionManagementError>;

	fn select_active_session(
		&mut self,
		binding_id: &str,
		session_id: &str,
	) -> Result<SessionDescriptor, SessionManagementError>;
}

/// Disabled session-state backend used when transport/session persistence is unavailable.
#[derive(Debug, Default)]
pub struct NoopSessionStateBackend;

impl SessionStateBackend for NoopSessionStateBackend {
	fn save_session_state(
		&mut self,
		_session_id: &str,
		_state: SessionState,
	) -> Result<(), SessionStateError> {
		Ok(())
	}

	fn load_session_state(
		&self,
		_session_id: &str,
	) -> Result<Option<SessionState>, SessionStateError> {
		Ok(None)
	}

	fn delete_session_state(&mut self, _session_id: &str) -> Result<(), SessionStateError> {
		Ok(())
	}
}

/// Disabled session-management backend used when catalog/pointer persistence is unavailable.
#[derive(Debug, Default)]
pub struct NoopSessionManagementBackend;

impl SessionManagementBackend for NoopSessionManagementBackend {
	fn create_session(
		&mut self,
		_binding_id: &str,
		_request: SessionCreateRequest,
	) -> Result<SessionDescriptor, SessionManagementError> {
		Err(SessionManagementError::Unsupported(
			"session catalog is unavailable".to_string(),
		))
	}

	fn get_session(
		&self,
		_binding_id: &str,
		_session_id: &str,
	) -> Result<Option<SessionDescriptor>, SessionManagementError> {
		Ok(None)
	}

	fn list_sessions(
		&self,
		_binding_id: &str,
	) -> Result<Vec<SessionSummary>, SessionManagementError> {
		Ok(Vec::new())
	}

	fn rename_session(
		&mut self,
		_binding_id: &str,
		_session_id: &str,
		_new_name: &str,
	) -> Result<SessionDescriptor, SessionManagementError> {
		Err(SessionManagementError::Unsupported(
			"session catalog is unavailable".to_string(),
		))
	}

	fn delete_session(
		&mut self,
		_binding_id: &str,
		_session_id: &str,
		_mode: SessionDeleteMode,
	) -> Result<(), SessionManagementError> {
		Err(SessionManagementError::Unsupported(
			"session catalog is unavailable".to_string(),
		))
	}

	fn get_active_session(
		&self,
		_binding_id: &str,
	) -> Result<Option<SessionDescriptor>, SessionManagementError> {
		Ok(None)
	}

	fn select_active_session(
		&mut self,
		_binding_id: &str,
		_session_id: &str,
	) -> Result<SessionDescriptor, SessionManagementError> {
		Err(SessionManagementError::Unsupported(
			"session catalog is unavailable".to_string(),
		))
	}
}

/// In-memory session-state backend used by core tests and lightweight entry tests.
///
/// This lives in `roku-memory` so test-only session behavior does not need to
/// reach back into transitional persistence crates.
#[derive(Debug, Default)]
pub struct InMemorySessionStateBackend {
	states: HashMap<String, SessionState>,
}

impl SessionStateBackend for InMemorySessionStateBackend {
	fn save_session_state(
		&mut self,
		session_id: &str,
		state: SessionState,
	) -> Result<(), SessionStateError> {
		self.states.insert(session_id.to_string(), state);
		Ok(())
	}

	fn load_session_state(
		&self,
		session_id: &str,
	) -> Result<Option<SessionState>, SessionStateError> {
		Ok(self.states.get(session_id).cloned())
	}

	fn delete_session_state(&mut self, session_id: &str) -> Result<(), SessionStateError> {
		self.states.remove(session_id);
		Ok(())
	}
}

/// In-memory session-management backend for core and entry tests.
#[derive(Debug, Default)]
pub struct InMemorySessionManagementBackend {
	sessions_by_binding: HashMap<String, HashMap<String, SessionDescriptor>>,
	active_by_binding: HashMap<String, String>,
}

impl InMemorySessionManagementBackend {
	fn binding_sessions_mut(
		&mut self,
		binding_id: &str,
	) -> Result<&mut HashMap<String, SessionDescriptor>, SessionManagementError> {
		let binding_id = normalize_binding_id(binding_id)?;
		Ok(self.sessions_by_binding.entry(binding_id).or_default())
	}

	fn binding_sessions(
		&self,
		binding_id: &str,
	) -> Result<Option<&HashMap<String, SessionDescriptor>>, SessionManagementError> {
		let binding_id = normalize_binding_id(binding_id)?;
		Ok(self.sessions_by_binding.get(&binding_id))
	}
}

impl SessionManagementBackend for InMemorySessionManagementBackend {
	fn create_session(
		&mut self,
		binding_id: &str,
		request: SessionCreateRequest,
	) -> Result<SessionDescriptor, SessionManagementError> {
		let session_id = generate_session_id();
		let name = resolve_session_name(&request, &session_id)?;
		let now = now_unix_ms_i64();
		let descriptor = SessionDescriptor {
			session_id: session_id.clone(),
			name,
			created_at_unix_ms: now,
			updated_at_unix_ms: now,
		};
		self.binding_sessions_mut(binding_id)?
			.insert(session_id, descriptor.clone());
		Ok(descriptor)
	}

	fn get_session(
		&self,
		binding_id: &str,
		session_id: &str,
	) -> Result<Option<SessionDescriptor>, SessionManagementError> {
		let session_id = normalize_session_id(session_id)?;
		Ok(self
			.binding_sessions(binding_id)?
			.and_then(|sessions| sessions.get(&session_id).cloned()))
	}

	fn list_sessions(
		&self,
		binding_id: &str,
	) -> Result<Vec<SessionSummary>, SessionManagementError> {
		Ok(self
			.binding_sessions(binding_id)?
			.into_iter()
			.flat_map(|sessions| sessions.values())
			.map(SessionSummary::from)
			.collect())
	}

	fn rename_session(
		&mut self,
		binding_id: &str,
		session_id: &str,
		new_name: &str,
	) -> Result<SessionDescriptor, SessionManagementError> {
		let session_id = normalize_session_id(session_id)?;
		let normalized_name = normalize_session_name(new_name)?;
		let descriptor = self
			.binding_sessions_mut(binding_id)?
			.get_mut(&session_id)
			.ok_or_else(|| SessionManagementError::NotFound(session_id.clone()))?;
		descriptor.name = normalized_name;
		descriptor.updated_at_unix_ms = now_unix_ms_i64();
		Ok(descriptor.clone())
	}

	fn delete_session(
		&mut self,
		binding_id: &str,
		session_id: &str,
		_mode: SessionDeleteMode,
	) -> Result<(), SessionManagementError> {
		let binding_id = normalize_binding_id(binding_id)?;
		let session_id = normalize_session_id(session_id)?;
		let sessions = self
			.sessions_by_binding
			.get_mut(&binding_id)
			.ok_or_else(|| SessionManagementError::NotFound(session_id.clone()))?;
		if sessions.remove(&session_id).is_none() {
			return Err(SessionManagementError::NotFound(session_id.clone()));
		}
		if sessions.is_empty() {
			self.sessions_by_binding.remove(&binding_id);
		}
		if self
			.active_by_binding
			.get(&binding_id)
			.is_some_and(|active| active == &session_id)
		{
			self.active_by_binding.remove(&binding_id);
		}
		Ok(())
	}

	fn get_active_session(
		&self,
		binding_id: &str,
	) -> Result<Option<SessionDescriptor>, SessionManagementError> {
		let binding_id = normalize_binding_id(binding_id)?;
		let Some(active) = self.active_by_binding.get(&binding_id) else {
			return Ok(None);
		};
		Ok(self
			.sessions_by_binding
			.get(&binding_id)
			.and_then(|sessions| sessions.get(active).cloned()))
	}

	fn select_active_session(
		&mut self,
		binding_id: &str,
		session_id: &str,
	) -> Result<SessionDescriptor, SessionManagementError> {
		let binding_id = normalize_binding_id(binding_id)?;
		let session_id = normalize_session_id(session_id)?;
		let sessions = self
			.sessions_by_binding
			.get_mut(&binding_id)
			.ok_or_else(|| SessionManagementError::NotFound(session_id.clone()))?;
		let descriptor = sessions
			.get_mut(&session_id)
			.ok_or_else(|| SessionManagementError::NotFound(session_id.clone()))?;
		descriptor.updated_at_unix_ms = now_unix_ms_i64();
		let descriptor = descriptor.clone();
		self.active_by_binding.insert(binding_id, session_id);
		Ok(descriptor)
	}
}

fn normalize_binding_id(value: &str) -> Result<String, SessionManagementError> {
	let normalized = value.trim();
	if normalized.is_empty() {
		return Err(SessionManagementError::Validation(
			"binding_id must not be empty".to_string(),
		));
	}
	Ok(normalized.to_string())
}

fn normalize_session_id(value: &str) -> Result<String, SessionManagementError> {
	let normalized = value.trim();
	if normalized.is_empty() {
		return Err(SessionManagementError::Validation(
			"session_id must not be empty".to_string(),
		));
	}
	Ok(normalized.to_string())
}

fn generate_session_id() -> String {
	static NEXT_SESSION_COUNTER: AtomicU64 = AtomicU64::new(1);
	let counter = NEXT_SESSION_COUNTER.fetch_add(1, Ordering::Relaxed);
	format!("session-{:020}-{:06}", now_unix_ms_i64().max(0), counter)
}

fn now_unix_ms_i64() -> i64 {
	SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.unwrap_or_default()
		.as_millis()
		.try_into()
		.unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
	use roku_common_types::{PendingLoopBinding, PlanningModeHint};

	use super::*;

	#[test]
	fn in_memory_session_state_backend_roundtrips_state() {
		let mut backend = InMemorySessionStateBackend::default();
		let state = SessionState {
			planning_mode: Some(PlanningModeHint::TreeSearch),
			pending_loop: Some(PendingLoopBinding {
				run_id: "loop-1".to_string(),
				loop_state_json: "{\"status\":\"paused\"}".to_string(),
			}),
		};

		backend
			.save_session_state("session-1", state.clone())
			.expect("state should save");
		assert_eq!(
			backend
				.load_session_state("session-1")
				.expect("state should load"),
			Some(state)
		);

		backend
			.delete_session_state("session-1")
			.expect("state should delete");
		assert_eq!(
			backend
				.load_session_state("session-1")
				.expect("deleted state should load"),
			None
		);
	}

	#[test]
	fn normalize_session_name_rejects_empty_and_too_long_values() {
		assert!(normalize_session_name("   ").is_err());
		assert!(normalize_session_name(&"你".repeat(51)).is_err());
		assert_eq!(
			normalize_session_name("  Hello Session  ").expect("name should normalize"),
			"Hello Session"
		);
	}

	#[test]
	fn in_memory_session_management_backend_manages_sessions_per_binding() {
		let mut backend = InMemorySessionManagementBackend::default();
		let created = backend
			.create_session(
				"chat-1",
				SessionCreateRequest {
					requested_name: Some("Main Session".to_string()),
				},
			)
			.expect("session should create");
		assert_eq!(created.name, "Main Session");

		let renamed = backend
			.rename_session("chat-1", &created.session_id, "Renamed Session")
			.expect("session should rename");
		assert_eq!(renamed.name, "Renamed Session");

		let listed = backend
			.list_sessions("chat-1")
			.expect("sessions should list");
		assert_eq!(listed.len(), 1);
		assert_eq!(listed[0].session_id, created.session_id);

		let active = backend
			.select_active_session("chat-1", &created.session_id)
			.expect("session should become active");
		assert_eq!(active.session_id, created.session_id);
		assert_eq!(
			backend
				.get_active_session("chat-1")
				.expect("active session should load")
				.expect("active session should exist")
				.session_id,
			created.session_id
		);

		backend
			.delete_session("chat-1", &created.session_id, SessionDeleteMode::default())
			.expect("session should delete");
		assert!(
			backend
				.get_active_session("chat-1")
				.expect("active session should load")
				.is_none()
		);
	}
}
