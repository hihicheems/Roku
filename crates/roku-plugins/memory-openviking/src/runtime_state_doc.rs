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

//! Adapter-private compatibility helpers for OpenViking runtime-state documents.
//!
//! Real OpenViking providers may materialize runtime-state text documents as a
//! directory-like root whose visible content lives in an internal child node.
//! This module keeps those provider-specific shape rules entirely inside the
//! OpenViking adapter crate.

use std::path::Path;
use std::thread::sleep;
use std::time::{Duration, Instant};

use roku_memory::MemoryError;

use super::OpenVikingLongTermMemoryBackend;

const MIN_POLL_INTERVAL: Duration = Duration::from_millis(25);
const MAX_POLL_INTERVAL: Duration = Duration::from_millis(100);
const MAX_STABLE_VISIBILITY_WINDOW: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RuntimeStateDocument {
	pub(super) content_uri: String,
	pub(super) content: String,
}

pub(super) fn read_runtime_state_document(
	backend: &OpenVikingLongTermMemoryBackend,
	document_uri: &str,
) -> Result<Option<RuntimeStateDocument>, MemoryError> {
	if !backend.resource_exists(document_uri)? {
		return Ok(None);
	}

	let Some(root_content) = backend.read_text_resource(document_uri)? else {
		return Err(materialized_shape_error(format!(
			"document root was reported as existing but content read returned missing root_uri={document_uri}"
		)));
	};
	if !root_content.trim().is_empty() {
		return Ok(Some(RuntimeStateDocument {
			content_uri: document_uri.to_string(),
			content: root_content,
		}));
	}

	let entries = backend.list_simple_entries(document_uri)?;
	let content_uri = resolve_materialized_content_uri(document_uri, &entries)?;
	let Some(content) = backend.read_text_resource(&content_uri)? else {
		return Err(materialized_shape_error(format!(
			"document root returned empty content and resolved content node was missing root_uri={document_uri} content_uri={content_uri}"
		)));
	};
	if content.trim().is_empty() {
		return Err(materialized_shape_error(format!(
			"document root returned empty content and resolved content node was also empty root_uri={document_uri} content_uri={content_uri}"
		)));
	}

	Ok(Some(RuntimeStateDocument {
		content_uri,
		content,
	}))
}

pub(super) fn replace_runtime_state_document(
	backend: &OpenVikingLongTermMemoryBackend,
	target_uri: &str,
	relative_stage_path: &Path,
	content: &str,
	reason: &str,
	instruction: &str,
) -> Result<(), MemoryError> {
	if backend.resource_exists(target_uri)? {
		backend.remove_resource_if_exists(target_uri, true)?;
		wait_until_runtime_state_document_absent(backend, target_uri)?;
	}
	backend.write_text_resource(
		target_uri,
		relative_stage_path,
		content,
		reason,
		instruction,
	)?;
	wait_until_runtime_state_document_matches(backend, target_uri, content)
}

pub(super) fn delete_runtime_state_document(
	backend: &OpenVikingLongTermMemoryBackend,
	target_uri: &str,
) -> Result<(), MemoryError> {
	if !backend.resource_exists(target_uri)? {
		return Ok(());
	}

	if let Err(error) = backend.remove_resource_if_exists(target_uri, true) {
		if !backend.resource_exists(target_uri)? {
			return Ok(());
		}
		return Err(error);
	}

	wait_until_runtime_state_document_absent(backend, target_uri)
}

fn wait_until_runtime_state_document_matches(
	backend: &OpenVikingLongTermMemoryBackend,
	document_uri: &str,
	expected_content: &str,
) -> Result<(), MemoryError> {
	let deadline = Instant::now() + backend.runtime_state_wait_timeout();
	let poll_interval = runtime_state_poll_interval(backend.runtime_state_wait_timeout());
	let stable_visibility_window =
		runtime_state_stability_window(backend.runtime_state_wait_timeout());
	let mut stable_since = None;

	loop {
		let observation = match read_runtime_state_document(backend, document_uri) {
			Ok(Some(document)) if document.content == expected_content => {
				let now = Instant::now();
				let visible_since = stable_since.get_or_insert(now);
				if now.duration_since(*visible_since) >= stable_visibility_window {
					return Ok(());
				}
				format!(
					"document is readable at {} and is waiting for a stable visibility window of {}ms",
					document.content_uri,
					stable_visibility_window.as_millis()
				)
			}
			Ok(Some(document)) => {
				stable_since = None;
				format!(
					"document became readable at {} but content did not match expected payload",
					document.content_uri
				)
			}
			Ok(None) => {
				stable_since = None;
				"document is still missing".to_string()
			}
			Err(error) if is_runtime_state_visibility_retryable(&error) => {
				stable_since = None;
				error.to_string()
			}
			Err(error) => return Err(error),
		};

		if Instant::now() >= deadline {
			return Err(MemoryError::Internal(format!(
				"timed out waiting for OpenViking runtime-state document to become readable root_uri={document_uri}; last_observation={}",
				observation
			)));
		}

		sleep(poll_interval);
	}
}

fn wait_until_runtime_state_document_absent(
	backend: &OpenVikingLongTermMemoryBackend,
	document_uri: &str,
) -> Result<(), MemoryError> {
	let deadline = Instant::now() + backend.runtime_state_wait_timeout();
	let poll_interval = runtime_state_poll_interval(backend.runtime_state_wait_timeout());

	loop {
		if !backend.resource_exists(document_uri)? {
			return Ok(());
		}

		if Instant::now() >= deadline {
			return Err(MemoryError::Internal(format!(
				"timed out waiting for OpenViking runtime-state document to disappear root_uri={document_uri}"
			)));
		}

		sleep(poll_interval);
	}
}

fn runtime_state_poll_interval(timeout: Duration) -> Duration {
	if timeout <= MIN_POLL_INTERVAL {
		return MIN_POLL_INTERVAL;
	}

	let candidate = Duration::from_millis((timeout.as_millis() / 20).max(1) as u64);
	candidate.clamp(MIN_POLL_INTERVAL, MAX_POLL_INTERVAL)
}

fn runtime_state_stability_window(timeout: Duration) -> Duration {
	timeout
		.min(MAX_STABLE_VISIBILITY_WINDOW)
		.max(MIN_POLL_INTERVAL)
}

fn is_runtime_state_visibility_retryable(error: &MemoryError) -> bool {
	matches!(error, MemoryError::Internal(message) if message.contains("OpenViking runtime-state document compatibility error"))
}

fn resolve_materialized_content_entry(
	document_uri: &str,
	entries: &[String],
) -> Result<String, MemoryError> {
	if entries.is_empty() {
		return Err(materialized_shape_error(format!(
			"document root returned empty content and no visible content nodes were found root_uri={document_uri}"
		)));
	}

	let preferred_md = preferred_markdown_entry_name(document_uri);
	if let Some(entry) = entries
		.iter()
		.find(|entry| runtime_state_entry_name(entry) == preferred_md)
	{
		return Ok(entry.clone());
	}

	let preferred_original = last_uri_segment(document_uri).to_string();
	if let Some(entry) = entries
		.iter()
		.find(|entry| runtime_state_entry_name(entry) == preferred_original)
	{
		return Ok(entry.clone());
	}

	if entries.len() == 1 {
		return Ok(entries[0].clone());
	}

	Err(materialized_shape_error(format!(
		"document root returned empty content and adapter could not uniquely resolve the materialized content node root_uri={document_uri} visible_entries={}",
		entries.join(",")
	)))
}

fn resolve_materialized_content_uri(
	document_uri: &str,
	entries: &[String],
) -> Result<String, MemoryError> {
	let entry = resolve_materialized_content_entry(document_uri, entries)?;
	if looks_like_absolute_uri(&entry) {
		Ok(entry)
	} else {
		Ok(format!(
			"{}/{}",
			document_uri.trim_end_matches('/'),
			entry.trim_start_matches('/')
		))
	}
}

fn preferred_markdown_entry_name(document_uri: &str) -> String {
	let file_name = last_uri_segment(document_uri);
	match file_name.rsplit_once('.') {
		Some((stem, _)) if !stem.is_empty() => format!("{stem}.md"),
		_ => format!("{file_name}.md"),
	}
}

fn last_uri_segment(uri: &str) -> &str {
	uri.rsplit('/').next().unwrap_or(uri)
}

pub(super) fn runtime_state_entry_name(entry: &str) -> &str {
	last_uri_segment(entry.trim_end_matches('/'))
}

fn looks_like_absolute_uri(value: &str) -> bool {
	value.contains("://")
}

fn materialized_shape_error(message: String) -> MemoryError {
	MemoryError::Internal(format!(
		"OpenViking runtime-state document compatibility error: {message}"
	))
}
