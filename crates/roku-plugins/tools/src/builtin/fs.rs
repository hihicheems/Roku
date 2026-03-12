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

// Cap directory listings to keep direct-route responses bounded and readable.
const MAX_DIR_ENTRIES: usize = 200;
// Default upper bound for text reads when the caller does not provide `max_bytes`.
const DEFAULT_MAX_BYTES: usize = 4_096;
// Cap glob expansion results to avoid oversized payloads from broad patterns.
const MAX_GLOB_MATCHES: usize = 200;
// Stop recursive basename search after scanning a bounded number of entries.
const MAX_DESCENDANT_SCAN_ENTRIES: usize = 8_000;

/// Returns catalog metadata for all fs builtin tools (`fs.find`, `fs.inspect`, `fs.list_dir`,
/// `fs.read_text`, `fs.glob`, `fs.exists`).
///
/// Used when the core-fs plugin is enabled: [`build_resource_catalog_with_plugin_snapshot_and_runtime_capabilities`]
/// in `builders` extends its tool entries with this list, then builds a [`ResourceCatalog`]. That catalog is
/// used by the router/classifier for: retrieval over descriptor text (BM25 + embedding), building the LLM
/// "Current inventory" in the route classifier prompt, resolving a chosen tool name to a [`ResourceSelector`],
/// and risk/cost for routing decisions. Tool names here must match the tools registered for execution via
/// [`register_tools`] in this module.
pub(crate) fn catalog_descriptors() -> Vec<CatalogDescriptor> {
	vec![
		descriptor_catalog(
			"fs.find",
			"Resolve a basename or fuzzy filesystem reference inside the allowed workspace roots and report whether it matched zero, one, or many candidates.",
			&[
				"find file",
				"basename grounding",
				"resolve file name",
				"模糊文件定位",
				"文件定位",
			],
			&["Find pr-check-ci.yml", "Find temp.log"],
			&["name", "kind"],
			&["fs.find"],
			&["find <name>"],
			&[
				"resolve a basename before reading a file",
				"ground a fuzzy file reference within the workspace",
				"locate a directory or file when only its name is known",
			],
		),
		descriptor_catalog(
			"fs.inspect",
			"Inspect a filesystem path or the current working directory and return bounded metadata such as kind, size, and timestamps.",
			&[
				"file metadata",
				"path inspection",
				"working directory",
				"cwd",
				"current directory",
				"current path",
				"project root",
				"目录",
				"路径",
				"当前目录",
				"当前路径",
				"工作目录",
			],
			&["pwd", "stat Cargo.toml"],
			&["path"],
			&["fs.inspect"],
			&["pwd", "stat <path>"],
			&[
				"inspect the current working directory",
				"report the current directory path",
				"inspect a file or directory path",
				"return metadata for a grounded path",
			],
		),
		descriptor_catalog(
			"fs.list_dir",
			"List entries in a directory, including hidden entries, with bounded output and truncation metadata.",
			&[
				"list files",
				"directory contents",
				"folder listing",
				"hidden files",
				"current directory",
				"working directory contents",
				"目录内容",
				"当前目录",
				"隐藏文件",
				"隐藏文件夹",
			],
			&["ls .", "ls .cursor", "ls crates/roku-plugins"],
			&["path"],
			&["fs.list_dir"],
			&["ls <path>", "ll <path>", "dir <path>"],
			&[
				"list the current directory",
				"show the current working directory contents",
				"list a nested subdirectory",
				"show hidden files and folders in a grounded directory",
			],
		),
		descriptor_catalog(
			"fs.read_text",
			"Read a text file with a maximum byte budget and truncation metadata after grounding the requested path inside the allowed workspace roots.",
			&[
				"read file",
				"open text",
				"show file contents",
				"read named file",
				"文件内容",
				"读取文件",
			],
			&["cat Cargo.toml", "cat .env.example"],
			&["path", "max_bytes"],
			&["fs.read_text"],
			&["cat <path>", "more <path>"],
			&[
				"read a file from the current directory",
				"read a uniquely grounded file by basename",
				"show the first part of a text file",
			],
		),
		descriptor_catalog(
			"fs.glob",
			"Expand a filesystem glob pattern within the allowed workspace roots.",
			&["glob", "pattern match", "find matching files"],
			&["Find all Rust files under crates/roku-plugins/**/*.rs."],
			&["pattern"],
			&["fs.glob"],
			&["glob <pattern>"],
			&["find files that match a glob pattern inside the workspace"],
		),
		descriptor_catalog(
			"fs.exists",
			"Check whether a filesystem path exists and report its kind if present.",
			&["path exists", "does file exist", "check directory", "存在"],
			&["Does tmp/test-excel.xlsx exist?"],
			&["path"],
			&["fs.exists"],
			&["test -e <path>", "exists <path>"],
			&["check whether a grounded file or directory exists"],
		),
	]
}

pub(crate) fn register_tools(runtime: &mut ToolRuntime) -> Result<(), ToolRuntimeError> {
	runtime.register_tool(FsFindTool)?;
	runtime.register_tool(FsInspectTool)?;
	runtime.register_tool(FsListDirTool)?;
	runtime.register_tool(FsReadTextTool)?;
	runtime.register_tool(FsGlobTool)?;
	runtime.register_tool(FsExistsTool)?;
	Ok(())
}

struct FsFindTool;
struct FsInspectTool;
struct FsListDirTool;
struct FsReadTextTool;
struct FsGlobTool;
struct FsExistsTool;

impl Tool for FsFindTool {
	fn descriptor(&self) -> ToolDescriptor {
		tool_descriptor("fs.find", &["name"], &["fs.find"])
	}

	fn invoke(&self, request: ToolInvocationRequest) -> Result<Value, ToolFailure> {
		let name = required_string(&request.input, "name")?;
		let kind = request
			.input
			.get("kind")
			.and_then(Value::as_str)
			.unwrap_or("any");
		let roots = allowed_read_roots(&request)?;
		let matches = find_descendant_matches(name, kind, &roots)?;
		let (ok, error_type, message) = match matches.len() {
			0 => (
				false,
				Some("path_not_found"),
				format!("Path `{name}` was not found within allowed workspace roots."),
			),
			1 => (
				true,
				None,
				format!("Found 1 matching candidate for `{name}`."),
			),
			count => (
				false,
				Some("multiple_candidates"),
				format!("Found {count} matching candidates for `{name}`."),
			),
		};
		let resolved_path = (matches.len() == 1).then(|| matches[0].clone());
		let data = json!({
			"name": name,
			"kind": kind,
			"match_count": matches.len(),
			"matches": matches,
			"resolved_path": resolved_path,
		});
		Ok(observation_like_output(
			message, ok, error_type, false, data,
		))
	}
}

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
		let data = json!({
			"path": resolved.display().to_string(),
			"kind": kind,
			"exists": true,
			"size": metadata.len(),
			"readonly": metadata.permissions().readonly(),
		});
		Ok(observation_like_output(message, true, None, false, data))
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
		let data = json!({
			"path": resolved.display().to_string(),
			"entries": items,
			"truncated": truncated,
		});
		Ok(observation_like_output(message, true, None, false, data))
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
		let data = json!({
			"path": resolved.display().to_string(),
			"content": content,
			"bytes_read": bytes_read.min(max_bytes),
			"truncated": truncated,
			"encoding": "utf-8-lossy",
		});
		Ok(observation_like_output(message, true, None, false, data))
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
		let data = json!({
			"pattern": pattern,
			"matches": matches,
			"truncated": truncated,
		});
		Ok(observation_like_output(message, true, None, false, data))
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
		let data = json!({
			"path": candidate.display().to_string(),
			"exists": exists,
			"kind": kind,
			"size": size,
		});
		Ok(observation_like_output(
			message,
			exists,
			(!exists).then_some("path_not_found"),
			false,
			data,
		))
	}
}

fn observation_like_output(
	message: String,
	ok: bool,
	error_type: Option<&str>,
	terminal: bool,
	data: Value,
) -> Value {
	let mut object = data.as_object().cloned().unwrap_or_default();
	object.insert("message".to_string(), Value::String(message));
	object.insert("ok".to_string(), Value::Bool(ok));
	object.insert(
		"error_type".to_string(),
		error_type.map(Value::from).unwrap_or(Value::Null),
	);
	object.insert("terminal".to_string(), Value::Bool(terminal));
	object.insert("data".to_string(), data);
	Value::Object(object)
}

fn descriptor_catalog(
	name: &str,
	description: &str,
	tags: &[&str],
	examples: &[&str],
	input_schema: &[&str],
	required_capabilities: &[&str],
	key_commands: &[&str],
	use_cases: &[&str],
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
		key_commands: key_commands
			.iter()
			.map(|value| (*value).to_string())
			.collect(),
		use_cases: use_cases.iter().map(|value| (*value).to_string()).collect(),
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
		path.clone()
	} else {
		roots
			.first()
			.cloned()
			.unwrap_or_else(|| PathBuf::from("."))
			.join(&path)
	};
	if candidate.exists() {
		let canonical = candidate.canonicalize().map_err(|error| {
			ToolFailure::terminal(format!("failed to resolve `{raw}`: {error}"))
		})?;
		ensure_allowed(&canonical, roots)?;
		return Ok(canonical);
	}
	if !path.is_absolute()
		&& !raw.contains('/')
		&& !raw.contains('\\')
		&& let Some(resolved) = find_unique_descendant_match(raw, roots)?
	{
		ensure_allowed(&resolved, roots)?;
		return Ok(resolved);
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

fn find_unique_descendant_match(
	target_name: &str,
	roots: &[PathBuf],
) -> Result<Option<PathBuf>, ToolFailure> {
	let matches = find_descendant_matches(target_name, "any", roots)?;
	if matches.len() > 1 {
		return Err(ToolFailure::terminal(format!(
			"`{target_name}` is ambiguous under the allowed read roots; please provide a more specific path"
		)));
	}
	Ok(matches.into_iter().next().map(PathBuf::from))
}

fn should_skip_workspace_search_dir(name: &str) -> bool {
	matches!(
		name,
		".git" | ".roku" | "target" | "node_modules" | "dist" | "build"
	)
}

fn find_descendant_matches(
	target_name: &str,
	kind: &str,
	roots: &[PathBuf],
) -> Result<Vec<String>, ToolFailure> {
	let mut exact_matches = Vec::new();
	let mut fuzzy_matches = Vec::new();
	let mut visited = 0_usize;
	for root in roots {
		let mut stack = vec![root.clone()];
		while let Some(directory) = stack.pop() {
			let entries = fs::read_dir(&directory).map_err(|error| {
				ToolFailure::terminal(format!(
					"failed to search under `{}`: {error}",
					directory.display()
				))
			})?;
			for entry in entries.filter_map(Result::ok) {
				visited += 1;
				if visited > MAX_DESCENDANT_SCAN_ENTRIES {
					return Ok(select_best_descendant_matches(exact_matches, fuzzy_matches));
				}
				let path = entry.path();
				let name = entry.file_name().to_string_lossy().to_string();
				let metadata = fs::symlink_metadata(&path).map_err(|error| {
					ToolFailure::terminal(format!(
						"failed to inspect `{}` while searching for `{target_name}`: {error}",
						path.display()
					))
				})?;
				if matches_kind(kind, &metadata) {
					let resolved = path.canonicalize().map_err(|error| {
						ToolFailure::terminal(format!(
							"failed to resolve `{}` while searching for `{target_name}`: {error}",
							path.display()
						))
					})?;
					if name == target_name {
						exact_matches.push(resolved.display().to_string());
					} else if let Some(score) = fuzzy_basename_match_score(target_name, &name) {
						fuzzy_matches.push((score, resolved.display().to_string()));
					}
				}
				if metadata.is_dir() && !should_skip_workspace_search_dir(&name) {
					stack.push(path);
				}
			}
		}
	}
	Ok(select_best_descendant_matches(exact_matches, fuzzy_matches))
}

fn select_best_descendant_matches(
	exact_matches: Vec<String>,
	mut fuzzy_matches: Vec<(u8, String)>,
) -> Vec<String> {
	if !exact_matches.is_empty() {
		return exact_matches;
	}
	if fuzzy_matches.is_empty() {
		return Vec::new();
	}
	fuzzy_matches.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
	let best_score = fuzzy_matches.first().map(|(score, _)| *score).unwrap_or(0);
	fuzzy_matches
		.into_iter()
		.filter(|(score, _)| *score == best_score)
		.map(|(_, path)| path)
		.collect()
}

fn fuzzy_basename_match_score(target_name: &str, candidate_name: &str) -> Option<u8> {
	let normalized_target = target_name.trim().to_ascii_lowercase();
	let normalized_candidate = candidate_name.trim().to_ascii_lowercase();
	if normalized_target.is_empty() || normalized_candidate.is_empty() {
		return None;
	}
	if normalized_target == normalized_candidate {
		return Some(0);
	}
	let target_path = Path::new(&normalized_target);
	let candidate_path = Path::new(&normalized_candidate);
	let target_extension = target_path.extension().and_then(|value| value.to_str());
	let candidate_extension = candidate_path.extension().and_then(|value| value.to_str());
	if target_extension != candidate_extension {
		return None;
	}
	if bounded_edit_distance(&normalized_target, &normalized_candidate, 2) <= 2 {
		return Some(1);
	}
	let target_stem = target_path.file_stem().and_then(|value| value.to_str())?;
	let candidate_stem = candidate_path
		.file_stem()
		.and_then(|value| value.to_str())?;
	(bounded_edit_distance(target_stem, candidate_stem, 2) <= 2).then_some(2)
}

fn bounded_edit_distance(left: &str, right: &str, max_distance: usize) -> usize {
	let left_chars = left.chars().collect::<Vec<_>>();
	let right_chars = right.chars().collect::<Vec<_>>();
	if left_chars.is_empty() {
		return right_chars.len();
	}
	if right_chars.is_empty() {
		return left_chars.len();
	}
	if left_chars.len().abs_diff(right_chars.len()) > max_distance {
		return max_distance.saturating_add(1);
	}
	let mut previous = (0..=right_chars.len()).collect::<Vec<_>>();
	let mut current = vec![0; right_chars.len() + 1];
	for (left_index, left_char) in left_chars.iter().enumerate() {
		current[0] = left_index + 1;
		let mut row_min = current[0];
		for (right_index, right_char) in right_chars.iter().enumerate() {
			let substitution_cost = usize::from(left_char != right_char);
			current[right_index + 1] = (previous[right_index + 1] + 1)
				.min(current[right_index] + 1)
				.min(previous[right_index] + substitution_cost);
			row_min = row_min.min(current[right_index + 1]);
		}
		if row_min > max_distance {
			return max_distance.saturating_add(1);
		}
		std::mem::swap(&mut previous, &mut current);
	}
	previous[right_chars.len()]
}

fn matches_kind(kind: &str, metadata: &fs::Metadata) -> bool {
	match kind {
		"file" => metadata.is_file(),
		"directory" => metadata.is_dir(),
		_ => true,
	}
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
