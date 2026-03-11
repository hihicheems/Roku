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

use std::env;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use glob::glob;
use roku_plugin_catalog::{CatalogDescriptor, ResourceCost, ResourceKind, ResourceRisk};
use roku_plugin_host::{
	RuntimeConstraints, SandboxProfile, Tool, ToolDescriptor, ToolFailure, ToolInvocationRequest,
	ToolRuntime, ToolRuntimeError, ToolSchema,
};
use serde_json::{Value, json};

const MAX_DIR_ENTRIES: usize = 200;
const DEFAULT_MAX_BYTES: usize = 4_096;
const MAX_GLOB_MATCHES: usize = 200;

pub(crate) fn catalog_descriptors() -> Vec<CatalogDescriptor> {
	vec![
		descriptor_catalog(
			"fs.inspect",
			"Inspect a filesystem path and return metadata such as kind, size, and timestamps.",
			&["file metadata", "inspect a path"],
			&["Read metadata for Cargo.toml."],
			&["path"],
			&["fs.inspect"],
		),
		descriptor_catalog(
			"fs.list_dir",
			"List entries in a directory with bounded output and truncation metadata.",
			&["list files", "directory contents", "folder listing"],
			&["List the files under crates/roku-plugins."],
			&["path"],
			&["fs.list_dir"],
		),
		descriptor_catalog(
			"fs.read_text",
			"Read a text file with a maximum byte budget and truncation metadata.",
			&["read file", "open text", "show file contents"],
			&["Read the first part of Cargo.toml."],
			&["path", "max_bytes"],
			&["fs.read_text"],
		),
		descriptor_catalog(
			"fs.glob",
			"Expand a filesystem glob pattern within the allowed workspace roots.",
			&["glob", "pattern match", "find matching files"],
			&["Find all Rust files under crates/roku-plugins/**/*.rs."],
			&["pattern"],
			&["fs.glob"],
		),
		descriptor_catalog(
			"fs.exists",
			"Check whether a filesystem path exists and report its kind if present.",
			&["path exists", "does file exist", "check directory"],
			&["Does tmp/test-excel.xlsx exist?"],
			&["path"],
			&["fs.exists"],
		),
	]
}

pub(crate) fn register_tools(runtime: &mut ToolRuntime) -> Result<(), ToolRuntimeError> {
	runtime.register_tool(FsInspectTool)?;
	runtime.register_tool(FsListDirTool)?;
	runtime.register_tool(FsReadTextTool)?;
	runtime.register_tool(FsGlobTool)?;
	runtime.register_tool(FsExistsTool)?;
	Ok(())
}

struct FsInspectTool;
struct FsListDirTool;
struct FsReadTextTool;
struct FsGlobTool;
struct FsExistsTool;

impl Tool for FsInspectTool {
	fn descriptor(&self) -> ToolDescriptor {
		tool_descriptor("fs.inspect", &["path"], &["fs.inspect"])
	}

	fn invoke(&self, request: ToolInvocationRequest) -> Result<Value, ToolFailure> {
		let path = required_string(&request.input, "path")?;
		let roots = allowed_read_roots(&request)?;
		let resolved = resolve_existing_path(path, &roots)?;
		let metadata = fs::symlink_metadata(&resolved).map_err(|error| {
			ToolFailure::terminal(format!("failed to inspect path `{path}`: {error}"))
		})?;
		let kind = path_kind(&metadata);
		let message = format!("Inspected `{}` ({kind}).", resolved.display());
		Ok(json!({
			"message": message,
			"path": resolved.display().to_string(),
			"kind": kind,
			"exists": true,
			"size": metadata.len(),
			"readonly": metadata.permissions().readonly(),
		}))
	}
}

impl Tool for FsListDirTool {
	fn descriptor(&self) -> ToolDescriptor {
		tool_descriptor("fs.list_dir", &["path"], &["fs.list_dir"])
	}

	fn invoke(&self, request: ToolInvocationRequest) -> Result<Value, ToolFailure> {
		let path = required_string(&request.input, "path")?;
		let roots = allowed_read_roots(&request)?;
		let resolved = resolve_existing_path(path, &roots)?;
		if !resolved.is_dir() {
			return Err(ToolFailure::terminal(format!(
				"`{}` is not a directory",
				resolved.display()
			)));
		}
		let mut entries = fs::read_dir(&resolved)
			.map_err(|error| {
				ToolFailure::terminal(format!("failed to list `{}`: {error}", resolved.display()))
			})?
			.filter_map(Result::ok)
			.collect::<Vec<_>>();
		entries.sort_by_key(|entry| entry.file_name());
		let truncated = entries.len() > MAX_DIR_ENTRIES;
		let items = entries
			.into_iter()
			.take(MAX_DIR_ENTRIES)
			.map(|entry| {
				let entry_path = entry.path();
				let metadata = fs::symlink_metadata(&entry_path).ok();
				json!({
					"name": entry.file_name().to_string_lossy().to_string(),
					"path": entry_path.display().to_string(),
					"kind": metadata.as_ref().map(path_kind).unwrap_or("unknown"),
					"size": metadata.as_ref().map(|value| value.len()),
				})
			})
			.collect::<Vec<_>>();
		let message = render_directory_message(&resolved, &items, truncated);
		Ok(json!({
			"message": message,
			"path": resolved.display().to_string(),
			"entries": items,
			"truncated": truncated,
		}))
	}
}

impl Tool for FsReadTextTool {
	fn descriptor(&self) -> ToolDescriptor {
		tool_descriptor("fs.read_text", &["path"], &["fs.read_text"])
	}

	fn invoke(&self, request: ToolInvocationRequest) -> Result<Value, ToolFailure> {
		let path = required_string(&request.input, "path")?;
		let max_bytes = request
			.input
			.get("max_bytes")
			.and_then(Value::as_u64)
			.and_then(|value| usize::try_from(value).ok())
			.unwrap_or(DEFAULT_MAX_BYTES);
		let roots = allowed_read_roots(&request)?;
		let resolved = resolve_existing_path(path, &roots)?;
		let mut file = File::open(&resolved).map_err(|error| {
			ToolFailure::terminal(format!("failed to open `{}`: {error}", resolved.display()))
		})?;
		let mut buffer = vec![0_u8; max_bytes.saturating_add(1)];
		let bytes_read = file.read(&mut buffer).map_err(|error| {
			ToolFailure::terminal(format!("failed to read `{}`: {error}", resolved.display()))
		})?;
		buffer.truncate(bytes_read);
		let truncated = bytes_read > max_bytes;
		let content_bytes = if truncated {
			&buffer[..max_bytes]
		} else {
			buffer.as_slice()
		};
		let content = String::from_utf8_lossy(content_bytes).to_string();
		let message = if content.trim().is_empty() {
			format!("`{}` is empty.", resolved.display())
		} else {
			content.clone()
		};
		Ok(json!({
			"message": message,
			"path": resolved.display().to_string(),
			"content": content,
			"bytes_read": bytes_read.min(max_bytes),
			"truncated": truncated,
			"encoding": "utf-8-lossy",
		}))
	}
}

impl Tool for FsGlobTool {
	fn descriptor(&self) -> ToolDescriptor {
		tool_descriptor("fs.glob", &["pattern"], &["fs.glob"])
	}

	fn invoke(&self, request: ToolInvocationRequest) -> Result<Value, ToolFailure> {
		let pattern = required_string(&request.input, "pattern")?;
		let roots = allowed_read_roots(&request)?;
		let compiled = compile_glob_pattern(pattern, &roots)?;
		let mut matches = Vec::new();
		for entry in glob(&compiled).map_err(|error| {
			ToolFailure::terminal(format!("invalid glob pattern `{pattern}`: {error}"))
		})? {
			let path = entry
				.map_err(|error| ToolFailure::terminal(format!("glob match failed: {error}")))?;
			let canonical = path.canonicalize().map_err(|error| {
				ToolFailure::terminal(format!("failed to resolve `{}`: {error}", path.display()))
			})?;
			ensure_allowed(&canonical, &roots)?;
			matches.push(canonical.display().to_string());
			if matches.len() >= MAX_GLOB_MATCHES {
				break;
			}
		}
		let truncated = matches.len() >= MAX_GLOB_MATCHES;
		let message = if matches.is_empty() {
			format!("No files matched `{pattern}`.")
		} else if truncated {
			format!("Found the first {} matches for `{pattern}`.", matches.len())
		} else {
			format!("Found {} matches for `{pattern}`.", matches.len())
		};
		Ok(json!({
			"message": message,
			"pattern": pattern,
			"matches": matches,
			"truncated": truncated,
		}))
	}
}

impl Tool for FsExistsTool {
	fn descriptor(&self) -> ToolDescriptor {
		tool_descriptor("fs.exists", &["path"], &["fs.exists"])
	}

	fn invoke(&self, request: ToolInvocationRequest) -> Result<Value, ToolFailure> {
		let path = required_string(&request.input, "path")?;
		let roots = allowed_read_roots(&request)?;
		let candidate = resolve_candidate_path(path, &roots)?;
		let exists = candidate.exists();
		let (kind, size) = if exists {
			let metadata = fs::symlink_metadata(&candidate).map_err(|error| {
				ToolFailure::terminal(format!(
					"failed to inspect `{}`: {error}",
					candidate.display()
				))
			})?;
			(path_kind(&metadata).to_string(), Some(metadata.len()))
		} else {
			("missing".to_string(), None)
		};
		let message = if exists {
			format!("`{}` exists as {}.", candidate.display(), kind)
		} else {
			format!("`{}` does not exist.", candidate.display())
		};
		Ok(json!({
			"message": message,
			"path": candidate.display().to_string(),
			"exists": exists,
			"kind": kind,
			"size": size,
		}))
	}
}

fn descriptor_catalog(
	name: &str,
	description: &str,
	tags: &[&str],
	examples: &[&str],
	input_schema: &[&str],
	required_capabilities: &[&str],
) -> CatalogDescriptor {
	CatalogDescriptor {
		selector: roku_common_types::ResourceSelector::tool(name),
		kind: ResourceKind::Tool,
		name: name.to_string(),
		role: Some("core_fs".to_string()),
		description: description.to_string(),
		discoverable: true,
		tags: tags.iter().map(|value| (*value).to_string()).collect(),
		examples: examples.iter().map(|value| (*value).to_string()).collect(),
		input_schema: input_schema
			.iter()
			.map(|value| (*value).to_string())
			.collect(),
		risk: ResourceRisk::Low,
		cost: ResourceCost {
			estimated_tokens: 0,
			estimated_latency_ms: 500,
		},
		required_capabilities: required_capabilities
			.iter()
			.map(|value| (*value).to_string())
			.collect(),
		summary: description.to_string(),
		key_commands: Vec::new(),
		use_cases: Vec::new(),
	}
}

fn tool_descriptor(
	name: &str,
	required_fields: &[&str],
	required_capabilities: &[&str],
) -> ToolDescriptor {
	ToolDescriptor {
		name: name.to_string(),
		version: "1.0.0".to_string(),
		input_schema: ToolSchema {
			required_fields: base_required_fields(required_fields),
		},
		output_schema: "result.v1".to_string(),
		required_capabilities: required_capabilities
			.iter()
			.map(|value| (*value).to_string())
			.collect(),
		runtime_constraints: RuntimeConstraints {
			timeout_ms: 10_000,
			max_retries: 0,
			retry_backoff_ms: 0,
			sandbox_profile: SandboxProfile::ReadOnlyFs,
			deterministic_hooks: true,
			allowed_read_roots: default_allowed_roots(),
			allowed_write_roots: Vec::new(),
		},
	}
}

fn base_required_fields(extra: &[&str]) -> Vec<String> {
	let mut fields = vec![
		"task_id".to_string(),
		"node_id".to_string(),
		"goal".to_string(),
		"summary".to_string(),
		"conversation_history".to_string(),
		"budget_tokens".to_string(),
		"time_budget_ms".to_string(),
	];
	for field in extra {
		if !fields.iter().any(|existing| existing == field) {
			fields.push((*field).to_string());
		}
	}
	fields
}

fn default_allowed_roots() -> Vec<PathBuf> {
	env::current_dir()
		.ok()
		.and_then(|path| path.canonicalize().ok())
		.map(|path| vec![path])
		.unwrap_or_default()
}

fn required_string<'a>(input: &'a Value, field: &str) -> Result<&'a str, ToolFailure> {
	input
		.get(field)
		.and_then(Value::as_str)
		.filter(|value| !value.trim().is_empty())
		.ok_or_else(|| ToolFailure::terminal(format!("missing required field `{field}`")))
}

fn allowed_read_roots(request: &ToolInvocationRequest) -> Result<Vec<PathBuf>, ToolFailure> {
	let mut roots = if request.allowed_read_roots.is_empty() {
		default_allowed_roots()
	} else {
		request.allowed_read_roots.clone()
	};
	if roots.is_empty() {
		return Err(ToolFailure::terminal(
			"no allowed read roots are configured for filesystem tools",
		));
	}
	for root in &mut roots {
		if !root.is_absolute() {
			*root = env::current_dir()
				.map(|cwd| cwd.join(&*root))
				.unwrap_or_else(|_| root.clone());
		}
		*root = root.canonicalize().map_err(|error| {
			ToolFailure::terminal(format!(
				"failed to canonicalize root `{}`: {error}",
				root.display()
			))
		})?;
	}
	Ok(roots)
}

fn resolve_existing_path(raw: &str, roots: &[PathBuf]) -> Result<PathBuf, ToolFailure> {
	let candidate = resolve_candidate_path(raw, roots)?;
	let canonical = candidate
		.canonicalize()
		.map_err(|error| ToolFailure::terminal(format!("failed to resolve `{raw}`: {error}")))?;
	ensure_allowed(&canonical, roots)?;
	Ok(canonical)
}

fn resolve_candidate_path(raw: &str, roots: &[PathBuf]) -> Result<PathBuf, ToolFailure> {
	let path = PathBuf::from(raw);
	let candidate = if path.is_absolute() {
		path
	} else {
		roots
			.first()
			.cloned()
			.unwrap_or_else(|| PathBuf::from("."))
			.join(path)
	};
	if candidate.exists() {
		let canonical = candidate.canonicalize().map_err(|error| {
			ToolFailure::terminal(format!("failed to resolve `{raw}`: {error}"))
		})?;
		ensure_allowed(&canonical, roots)?;
		return Ok(canonical);
	}
	if let Some(parent) = candidate.parent() {
		let canonical_parent = parent.canonicalize().map_err(|error| {
			ToolFailure::terminal(format!("failed to resolve parent of `{raw}`: {error}"))
		})?;
		ensure_allowed(&canonical_parent, roots)?;
		return Ok(candidate);
	}
	ensure_allowed(&candidate, roots)?;
	Ok(candidate)
}

fn ensure_allowed(path: &Path, roots: &[PathBuf]) -> Result<(), ToolFailure> {
	if roots.iter().any(|root| path.starts_with(root)) {
		Ok(())
	} else {
		Err(ToolFailure::terminal(format!(
			"path `{}` is outside the allowed read roots",
			path.display()
		)))
	}
}

fn compile_glob_pattern(pattern: &str, roots: &[PathBuf]) -> Result<String, ToolFailure> {
	let path = Path::new(pattern);
	if path.is_absolute() {
		ensure_allowed(path, roots)?;
		return Ok(pattern.to_string());
	}
	let root = roots.first().ok_or_else(|| {
		ToolFailure::terminal("no allowed read roots are configured for glob matching")
	})?;
	Ok(root.join(path).display().to_string())
}

fn path_kind(metadata: &fs::Metadata) -> &'static str {
	let file_type = metadata.file_type();
	if file_type.is_dir() {
		"directory"
	} else if file_type.is_file() {
		"file"
	} else if file_type.is_symlink() {
		"symlink"
	} else {
		"other"
	}
}

fn render_directory_message(path: &Path, entries: &[Value], truncated: bool) -> String {
	if entries.is_empty() {
		return format!("`{}` is empty.", path.display());
	}
	let mut lines = entries
		.iter()
		.filter_map(|entry| entry.get("name").and_then(Value::as_str))
		.map(|name| format!("- {name}"))
		.collect::<Vec<_>>();
	if truncated {
		lines.push(format!("... truncated to {} entries", entries.len()));
	}
	lines.join("\n")
}
