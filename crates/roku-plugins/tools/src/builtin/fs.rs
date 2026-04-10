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
use std::io::{Read, Write as _};
use std::path::{Path, PathBuf};

use crate::contract::{
	contract_input_schema, contract_tool_schema, grounding_contract, grounding_contract_simple,
	input_contract, input_field, output_contract, runtime_contract, selection_contract,
};
use crate::runtime_config::{
	FsToolRuntimeConfig, HARD_MAX_DESCENDANT_SCAN_ENTRIES, HARD_MAX_READ_BYTES,
};
use glob::glob;
use roku_common_types::{
	ApprovalRequirement, ApprovalRequirementScope, CanonicalDigest, CanonicalExecution,
	ExecutionActionClass, ExecutionEnvPolicy, ExecutionEnvPolicyMode, ExecutionResourceScope,
	ExtractionHint, GroundingStrategy, InvocationMode, PolicyDecision, PolicyOutcome,
	PolicyReasonCode, ToolContract, ToolOutputEnvelope, ToolRetryPolicy, ToolSideEffectPolicy,
};
use roku_common_types::{CatalogDescriptor, ResourceCost, ResourceKind, ResourceRisk};
use roku_plugin_host::{
	RuntimeConstraints, SandboxProfile, Tool, ToolDescriptor, ToolFailure, ToolInvocationRequest,
	ToolRuntime, ToolRuntimeError,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// Returns catalog metadata for all fs builtin tools (`fs.find`, `fs.inspect`, `fs.list_dir`,
/// `fs.read_text`, `fs.glob`, `fs.exists`).
///
/// Used when the core-fs plugin is enabled: [`build_resource_catalog_with_plugin_snapshot_and_runtime_capabilities`]
/// in `builders` extends its tool entries with this list, then builds a [`ResourceCatalog`]. That catalog is
/// used by the router/classifier for: retrieval over compact selection text (BM25 + embedding), building the
/// route classifier's compact selection inventory, resolving a chosen tool name to a [`ResourceSelector`], and
/// risk/cost for routing decisions. Tool names here must match the tools registered for execution via
/// [`register_tools`] in this module.
#[allow(dead_code)]
pub(crate) fn catalog_descriptors() -> Vec<CatalogDescriptor> {
	catalog_descriptors_with_config(&FsToolRuntimeConfig::default())
}

pub(crate) fn catalog_descriptors_with_config(
	_runtime_config: &FsToolRuntimeConfig,
) -> Vec<CatalogDescriptor> {
	vec![
		descriptor_catalog(
			"fs.find",
			"Use this when you only know one basename or fuzzy filesystem reference inside the workspace and need grounded candidates before doing anything else. Do not use it when you already have a concrete path, when you expect many repeated matches, or when the task is counting files across directories; `fs.glob` is the right tool for that. It returns zero, one, or many candidate paths that the agent can disambiguate or feed into a later tool call.",
			"Resolve one fuzzy workspace file or directory name before a follow-up filesystem step.",
			&[
				"find file",
				"basename grounding",
				"resolve file name",
				"模糊文件定位",
				"文件定位",
			],
			&["Find pr-check-ci.yml", "Find the directory named docs"],
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
			"Use this when you need metadata about a known path or need to ground the current working directory. Do not use it to list directory entries or read file contents. It returns bounded path facts like kind, size, and timestamps.",
			"Inspect metadata for a known path or the current working directory.",
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
			"Use this when you already know the directory path and need its immediate entries, including hidden ones, in bounded form. Do not use it when the path is still fuzzy or when you need file contents instead of a listing. It returns a truncated-safe entry list plus enough metadata to answer listing questions or choose a follow-up path.",
			"List the immediate entries in a known directory.",
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
			"Use this when you already have a concrete text file path and need its contents or the first bounded chunk of it. Do not use it for directories, binary inspection, or fuzzy names; resolve those first with `fs.find` or `fs.inspect`. It returns lossy UTF-8 text plus truncation metadata that can be quoted, summarized, or passed to another worker.",
			"Read text content from a known file path.",
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
			"Use this when the task is about many matching paths at once, especially wildcard searches, repeated filenames across directories, or counts like 'how many Cargo.toml files are there'. Do not use it for a single fuzzy basename or a path you expect to resolve to one best candidate; `fs.find` is better for that. It returns a bounded match set that is good for counting, enumerating, or selecting follow-up files.",
			"Find many workspace paths that match one glob pattern.",
			&["glob", "pattern match", "find matching files"],
			&[
				"Find all Rust files under crates/roku-plugins/**/*.rs.",
				"Count all Cargo.toml files in the workspace.",
			],
			&["pattern"],
			&["fs.glob"],
			&["glob <pattern>"],
			&[
				"find files that match a glob pattern inside the workspace",
				"count repeated filenames across directories",
			],
		),
		descriptor_catalog(
			"fs.exists",
			"Use this for a yes/no existence check on a concrete path. Do not use it when you also need metadata, directory contents, or file contents. It returns existence plus kind when present.",
			"Check whether a known path exists.",
			&["path exists", "does file exist", "check directory", "存在"],
			&["Does tmp/test-excel.xlsx exist?"],
			&["path"],
			&["fs.exists"],
			&["test -e <path>", "exists <path>"],
			&["check whether a grounded file or directory exists"],
		),
		{
			let mut desc = descriptor_catalog(
				"fs.edit",
				"Use this when you need to make a precise string replacement in an existing file. Provide a unique old_string that appears exactly once in the file, along with the new_string to replace it. Do not use it for creating new files or overwriting entire files; use fs.write for that.",
				"Replace a unique string in an existing file.",
				&["edit", "replace", "modify", "file mutation"],
				&["Replace 'foo' with 'bar' in config.toml"],
				&["file_path", "old_string", "new_string"],
				&["fs.edit"],
				&["edit <path>"],
				&[
					"replace a unique string in an existing file",
					"make a targeted text substitution in a source file",
				],
			);
			desc.risk = ResourceRisk::Medium;
			desc
		},
		{
			let mut desc = descriptor_catalog(
				"fs.write",
				"Use this when you need to create a new file or overwrite an existing one entirely. Do not use it for targeted edits within an existing file; use fs.edit for that.",
				"Create a new file or overwrite an existing one.",
				&["write", "create", "overwrite", "file mutation"],
				&["Create a new README.md with content"],
				&["file_path", "content"],
				&["fs.write"],
				&["write <path>"],
				&[
					"create a new file with specified content",
					"overwrite an existing file with new content",
				],
			);
			desc.risk = ResourceRisk::Medium;
			desc
		},
		descriptor_catalog(
			"fs.grep",
			"Use this when you need to search for a pattern in file contents across the workspace. Returns matched lines with file paths and line numbers. Do not use it for filename-based search; use fs.find or fs.glob for that.",
			"Search file contents for a regex or literal pattern.",
			&[
				"grep",
				"search content",
				"find in files",
				"pattern match",
				"code search",
			],
			&[
				"Search for 'TODO' in all Rust files.",
				"Find function definitions matching 'fn main'.",
			],
			&["pattern"],
			&["fs.grep"],
			&["grep <pattern>"],
			&[
				"search for a pattern in file contents",
				"find all occurrences of a string across workspace files",
			],
		),
	]
}

#[allow(dead_code)]
pub(crate) fn register_tools(runtime: &mut ToolRuntime) -> Result<(), ToolRuntimeError> {
	register_tools_with_config(runtime, &FsToolRuntimeConfig::default())
}

pub(crate) fn register_tools_with_config(
	runtime: &mut ToolRuntime,
	config: &FsToolRuntimeConfig,
) -> Result<(), ToolRuntimeError> {
	runtime.register_tool(FsFindTool {
		config: config.clone(),
	})?;
	runtime.register_tool(FsInspectTool {
		config: config.clone(),
	})?;
	runtime.register_tool(FsListDirTool {
		config: config.clone(),
	})?;
	runtime.register_tool(FsReadTextTool {
		config: config.clone(),
	})?;
	runtime.register_tool(FsGlobTool {
		config: config.clone(),
	})?;
	runtime.register_tool(FsExistsTool {
		config: config.clone(),
	})?;
	runtime.register_tool(FsEditTool {
		config: config.clone(),
	})?;
	runtime.register_tool(FsWriteTool {
		config: config.clone(),
	})?;
	runtime.register_tool(FsGrepTool {
		config: config.clone(),
	})?;
	Ok(())
}

pub(crate) fn canonical_execution_from_runtime_input(
	tool_name: &str,
	input: &Value,
) -> Option<CanonicalExecution> {
	match tool_name {
		"fs.exists" | "fs.inspect" | "fs.list_dir" | "fs.read_text" => {
			let request = ToolInvocationRequest {
				invocation_key: format!("{tool_name}:agent-runtime-canonicalization"),
				attempt: 1,
				input: input.clone(),
				sandbox_profile: SandboxProfile::ReadOnlyFs,
				attachments: Vec::new(),
				allowed_read_roots: default_allowed_roots(),
				allowed_write_roots: Vec::new(),
			};
			canonical_fs_execution(tool_name, &request).ok()
		}
		"fs.edit" | "fs.write" => {
			let request = ToolInvocationRequest {
				invocation_key: format!("{tool_name}:agent-runtime-canonicalization"),
				attempt: 1,
				input: input.clone(),
				sandbox_profile: SandboxProfile::ReadOnlyFs,
				attachments: Vec::new(),
				allowed_read_roots: Vec::new(),
				allowed_write_roots: default_allowed_roots(),
			};
			canonical_fs_write_execution(tool_name, &request).ok()
		}
		_ => None,
	}
}

#[derive(Clone)]
struct FsFindTool {
	config: FsToolRuntimeConfig,
}

#[derive(Clone)]
struct FsInspectTool {
	config: FsToolRuntimeConfig,
}

#[derive(Clone)]
struct FsListDirTool {
	config: FsToolRuntimeConfig,
}

#[derive(Clone)]
struct FsReadTextTool {
	config: FsToolRuntimeConfig,
}

#[derive(Clone)]
struct FsGlobTool {
	config: FsToolRuntimeConfig,
}

#[derive(Clone)]
struct FsExistsTool {
	config: FsToolRuntimeConfig,
}

#[derive(Clone)]
struct FsEditTool {
	#[allow(dead_code)]
	config: FsToolRuntimeConfig,
}

#[derive(Clone)]
struct FsWriteTool {
	#[allow(dead_code)]
	config: FsToolRuntimeConfig,
}

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
		let match_summary =
			find_descendant_matches(name, kind, &roots, self.config.max_descendant_scan_entries)?;
		let (ok, error_type, message) = match match_summary.matches.len() {
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
		let resolved_path =
			(match_summary.matches.len() == 1).then(|| match_summary.matches[0].clone());
		let data = json!({
			"name": name,
			"kind": kind,
			"match_count": match_summary.matches.len(),
			"matches": match_summary.matches,
			"resolved_path": resolved_path,
			"exact_match_count": match_summary.exact_match_count,
			"fuzzy_match_count": match_summary.fuzzy_match_count,
			"match_mode": match_summary.match_mode,
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
		let _ = &self.config;
		let path = required_string(&request.input, "path")?;
		let roots = allowed_read_roots(&request)?;
		let resolved =
			resolve_existing_path(path, &roots, self.config.max_descendant_scan_entries)?;
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

	fn policy_decision(&self, execution: &CanonicalExecution) -> Option<PolicyDecision> {
		(execution.tool_name == "fs.inspect").then(|| evaluate_fs_policy(execution))
	}
}

impl Tool for FsListDirTool {
	fn descriptor(&self) -> ToolDescriptor {
		tool_descriptor("fs.list_dir", &["path"], &["fs.list_dir"])
	}

	fn invoke(&self, request: ToolInvocationRequest) -> Result<Value, ToolFailure> {
		let path = required_string(&request.input, "path")?;
		let roots = allowed_read_roots(&request)?;
		let resolved =
			resolve_existing_path(path, &roots, self.config.max_descendant_scan_entries)?;
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
		let truncated = entries.len() > self.config.max_dir_entries;
		let items = entries
			.into_iter()
			.take(self.config.max_dir_entries)
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

	fn policy_decision(&self, execution: &CanonicalExecution) -> Option<PolicyDecision> {
		(execution.tool_name == "fs.list_dir").then(|| evaluate_fs_policy(execution))
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
			.unwrap_or(self.config.default_max_bytes)
			.min(HARD_MAX_READ_BYTES);
		let roots = allowed_read_roots(&request)?;
		let resolved =
			resolve_existing_path(path, &roots, self.config.max_descendant_scan_entries)?;
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

	fn policy_decision(&self, execution: &CanonicalExecution) -> Option<PolicyDecision> {
		(execution.tool_name == "fs.read_text").then(|| evaluate_fs_policy(execution))
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
			if matches.len() >= self.config.max_glob_matches {
				break;
			}
		}
		let truncated = matches.len() >= self.config.max_glob_matches;
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
		let _ = &self.config;
		let path = required_string(&request.input, "path")?;
		let roots = allowed_read_roots(&request)?;
		let candidate =
			resolve_candidate_path(path, &roots, self.config.max_descendant_scan_entries)?;
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

	fn policy_decision(&self, execution: &CanonicalExecution) -> Option<PolicyDecision> {
		(execution.tool_name == "fs.exists").then(|| evaluate_fs_policy(execution))
	}
}

impl Tool for FsEditTool {
	fn descriptor(&self) -> ToolDescriptor {
		write_tool_descriptor(
			"fs.edit",
			&["file_path", "old_string", "new_string"],
			&["fs.edit"],
		)
	}

	fn invoke(&self, request: ToolInvocationRequest) -> Result<Value, ToolFailure> {
		let file_path = required_string(&request.input, "file_path")?;
		let old_string = required_string(&request.input, "old_string")?;
		let new_string = required_string(&request.input, "new_string")?;
		let roots = allowed_write_roots(&request)?;
		let working_directory = roots.first().cloned().ok_or_else(|| {
			ToolFailure::terminal("no allowed write roots are configured for filesystem tools")
		})?;
		let resolved = resolve_scope_target(file_path, &working_directory)?;
		ensure_allowed_write(&resolved, &roots)?;

		if !resolved.exists() {
			let data = json!({ "file_path": resolved.display().to_string() });
			return Ok(observation_like_output(
				format!("`{}` does not exist.", resolved.display()),
				false,
				Some("file_not_found"),
				false,
				data,
			));
		}

		let content = fs::read_to_string(&resolved).map_err(|error| {
			ToolFailure::terminal(format!("failed to read `{}`: {error}", resolved.display()))
		})?;

		let match_count = content.matches(old_string).count();
		match match_count {
			0 => {
				// Provide a short excerpt of the file so the agent knows what it
				// actually contains and can retry with the correct old_string.
				let excerpt: String = content.lines().take(20).collect::<Vec<_>>().join("\n");
				let data = json!({
					"file_path": resolved.display().to_string(),
					"match_count": 0,
					"file_excerpt": excerpt,
				});
				Ok(observation_like_output(
					format!(
						"string not found in `{}`. File has {} lines.",
						resolved.display(),
						content.lines().count()
					),
					false,
					Some("string_not_found"),
					false,
					data,
				))
			}
			1 => {
				let new_content = content.replacen(old_string, new_string, 1);
				let byte_offset = content.find(old_string).unwrap_or(0);
				let line_start = content[..byte_offset].matches('\n').count() + 1;
				let line_end = line_start + old_string.matches('\n').count();

				fs::write(&resolved, &new_content).map_err(|error| {
					ToolFailure::terminal(format!(
						"failed to write `{}`: {error}",
						resolved.display()
					))
				})?;

				let data = json!({
					"file_path": resolved.display().to_string(),
					"line_start": line_start,
					"line_end": line_end,
					"match_count": 1,
				});
				Ok(observation_like_output(
					format!("Edited `{}`.", resolved.display()),
					true,
					None,
					false,
					data,
				))
			}
			count => {
				let data = json!({
					"file_path": resolved.display().to_string(),
					"match_count": count,
				});
				Ok(observation_like_output(
					format!("{count} matches found, provide more surrounding context"),
					false,
					Some("multiple_matches"),
					false,
					data,
				))
			}
		}
	}

	fn policy_decision(&self, execution: &CanonicalExecution) -> Option<PolicyDecision> {
		(execution.tool_name == "fs.edit").then(|| evaluate_fs_write_policy(execution))
	}
}

impl Tool for FsWriteTool {
	fn descriptor(&self) -> ToolDescriptor {
		write_tool_descriptor("fs.write", &["file_path", "content"], &["fs.write"])
	}

	fn invoke(&self, request: ToolInvocationRequest) -> Result<Value, ToolFailure> {
		let file_path = required_string(&request.input, "file_path")?;
		let content = request
			.input
			.get("content")
			.and_then(Value::as_str)
			.ok_or_else(|| ToolFailure::terminal("missing required field `content`"))?;
		let roots = allowed_write_roots(&request)?;
		let working_directory = roots.first().cloned().ok_or_else(|| {
			ToolFailure::terminal("no allowed write roots are configured for filesystem tools")
		})?;
		let resolved = resolve_scope_target(file_path, &working_directory)?;
		ensure_allowed_write(&resolved, &roots)?;

		let created = !resolved.exists();

		if let Some(parent) = resolved.parent()
			&& !parent.exists()
		{
			fs::create_dir_all(parent).map_err(|error| {
				ToolFailure::terminal(format!(
					"failed to create parent directories for `{}`: {error}",
					resolved.display()
				))
			})?;
		}

		let bytes_written = content.len();
		let mut file = File::create(&resolved).map_err(|error| {
			ToolFailure::terminal(format!(
				"failed to create `{}`: {error}",
				resolved.display()
			))
		})?;
		file.write_all(content.as_bytes()).map_err(|error| {
			ToolFailure::terminal(format!("failed to write `{}`: {error}", resolved.display()))
		})?;

		let data = json!({
			"file_path": resolved.display().to_string(),
			"created": created,
			"bytes_written": bytes_written,
		});
		Ok(observation_like_output(
			if created {
				format!("Created `{}`.", resolved.display())
			} else {
				format!("Wrote `{}`.", resolved.display())
			},
			true,
			None,
			false,
			data,
		))
	}

	fn policy_decision(&self, execution: &CanonicalExecution) -> Option<PolicyDecision> {
		(execution.tool_name == "fs.write").then(|| evaluate_fs_write_policy(execution))
	}
}

fn observation_like_output(
	message: String,
	ok: bool,
	error_type: Option<&str>,
	terminal: bool,
	data: Value,
) -> Value {
	ToolOutputEnvelope::new(ok, error_type, terminal, message, data).into_value()
}

fn fs_tool_contract(name: &str) -> Option<ToolContract> {
	let runtime_constraints = RuntimeConstraints {
		timeout_ms: 10_000,
		max_retries: 0,
		retry_backoff_ms: 0,
		sandbox_profile: SandboxProfile::ReadOnlyFs,
		deterministic_hooks: true,
		allowed_read_roots: default_allowed_roots(),
		allowed_write_roots: Vec::new(),
	};
	let runtime = runtime_contract(
		&runtime_constraints,
		ToolSideEffectPolicy::ReadOnly,
		ToolRetryPolicy::Never,
	);
	match name {
		"fs.find" => Some(ToolContract {
			selection: selection_contract(
				&[
					"Use when only a basename or fuzzy workspace reference is known and a grounded path must be resolved first.",
				],
				&[
					"Do not use when a concrete path is already available.",
					"Do not use for recursive file counting or glob-style pattern expansion.",
				],
				&[
					"Commonly confused with fs.glob when the request already contains a wildcard pattern.",
					"Commonly confused with fs.read_text when the path is already concrete.",
				],
			),
			input: input_contract(
				vec![
					input_field(
						"name",
						true,
						"The basename or fuzzy filesystem reference to resolve inside the allowed workspace roots.",
						&["Reject when empty."],
					),
					input_field(
						"kind",
						false,
						"Optional kind filter such as file, dir, or any.",
						&["Reject when the filter is unsupported."],
					),
				],
				&["Search stays inside allowed read roots and returns bounded candidate lists."],
			),
			output: output_contract(
				"Returns zero, one, or many grounded candidate paths plus structured match_count, exact/fuzzy counters, match_mode, and an optional resolved_path when there is exactly one match.",
				"Zero matches surface as an explicit path_not_found observation.",
				&[
					"multiple_candidates is a non-terminal observation that may require ask_user disambiguation.",
					"path_not_found remains a failed observation and does not claim task completion.",
				],
				false,
				false,
			),
			runtime: runtime.clone(),
			grounding: grounding_contract(
				GroundingStrategy::PathBased,
				&["name"],
				Some("name"),
				true,
				ExtractionHint::ExplicitPath,
				serde_json::Map::from_iter([("kind".to_string(), json!("any"))]),
			),
		}),
		"fs.read_text" => Some(ToolContract {
			selection: selection_contract(
				&[
					"Use when the path is already grounded and the user needs textual file contents.",
				],
				&[
					"Do not use for binary files or directory listings.",
					"Do not use when the path is still fuzzy and fs.find should run first.",
				],
				&[
					"Commonly confused with fs.inspect when the user wants metadata instead of contents.",
					"Commonly confused with command.run for shell-based cat requests that a direct read can answer safely.",
				],
			),
			input: input_contract(
				vec![
					input_field(
						"path",
						true,
						"The concrete path to read under the allowed workspace roots.",
						&[
							"Reject when the path resolves to a directory or outside the allowed roots.",
						],
					),
					input_field(
						"max_bytes",
						false,
						"Optional byte cap for the read, clamped by the runtime hard limit.",
						&["Reject when zero or larger than the hard read ceiling."],
					),
				],
				&["Reads are bounded by max_bytes and never write to the workspace."],
			),
			output: output_contract(
				"Returns file content, bytes_read, truncation state, and encoding metadata for a grounded text file.",
				"Empty files still return ok=true with a message that the file is empty.",
				&[
					"Non-text paths and missing paths surface as explicit failed observations.",
					"Successful reads are non-terminal observations that may feed a later synthesis step.",
				],
				true,
				false,
			),
			runtime,
			grounding: grounding_contract_simple(
				GroundingStrategy::PathBased,
				&["path"],
				Some("path"),
				true,
				ExtractionHint::ConcretePath,
			),
		}),
		"fs.list_dir" => Some(ToolContract {
			grounding: grounding_contract_simple(
				GroundingStrategy::PathBased,
				&["path"],
				Some("path"),
				true,
				ExtractionHint::ConcretePath,
			),
			..ToolContract::default()
		}),
		"fs.inspect" => Some(ToolContract {
			grounding: grounding_contract_simple(
				GroundingStrategy::PathBased,
				&["path"],
				Some("path"),
				true,
				ExtractionHint::ConcretePath,
			),
			..ToolContract::default()
		}),
		"fs.exists" => Some(ToolContract {
			grounding: grounding_contract_simple(
				GroundingStrategy::PathBased,
				&["path"],
				Some("path"),
				false,
				ExtractionHint::ConcretePath,
			),
			..ToolContract::default()
		}),
		"fs.glob" => Some(ToolContract {
			grounding: grounding_contract_simple(
				GroundingStrategy::PatternBased,
				&["pattern"],
				Some("pattern"),
				false,
				ExtractionHint::GlobPattern,
			),
			..ToolContract::default()
		}),
		"fs.edit" => Some(ToolContract {
			grounding: grounding_contract_simple(
				GroundingStrategy::PathBased,
				&["file_path", "old_string", "new_string"],
				Some("file_path"),
				true,
				ExtractionHint::ConcretePath,
			),
			..ToolContract::default()
		}),
		"fs.write" => Some(ToolContract {
			grounding: grounding_contract_simple(
				GroundingStrategy::PathBased,
				&["file_path", "content"],
				Some("file_path"),
				true,
				ExtractionHint::ConcretePath,
			),
			..ToolContract::default()
		}),
		"fs.grep" => Some(ToolContract {
			grounding: grounding_contract_simple(
				GroundingStrategy::PatternBased,
				&["pattern"],
				Some("pattern"),
				false,
				ExtractionHint::GrepPattern,
			),
			..ToolContract::default()
		}),
		_ => None,
	}
}

fn descriptor_catalog(
	name: &str,
	description: &str,
	selection_hint: &str,
	tags: &[&str],
	examples: &[&str],
	input_schema: &[&str],
	required_capabilities: &[&str],
	key_commands: &[&str],
	use_cases: &[&str],
) -> CatalogDescriptor {
	let contract = fs_tool_contract(name);
	let fallback_input_schema = input_schema
		.iter()
		.map(|value| (*value).to_string())
		.collect::<Vec<_>>();
	CatalogDescriptor {
		selector: roku_common_types::ResourceSelector::tool(name),
		kind: ResourceKind::Tool,
		name: name.to_string(),
		role: Some("core_fs".to_string()),
		description: description.to_string(),
		selection_hint: selection_hint.to_string(),
		discoverable: true,
		tags: tags.iter().map(|value| (*value).to_string()).collect(),
		examples: examples.iter().map(|value| (*value).to_string()).collect(),
		input_schema: contract_input_schema(contract.as_ref(), &fallback_input_schema),
		risk: ResourceRisk::Low,
		cost: ResourceCost {
			estimated_tokens: 0,
			estimated_latency_ms: 500,
		},
		required_capabilities: required_capabilities
			.iter()
			.map(|value| (*value).to_string())
			.collect(),
		summary: selection_hint.to_string(),
		key_commands: key_commands
			.iter()
			.map(|value| (*value).to_string())
			.collect(),
		use_cases: use_cases.iter().map(|value| (*value).to_string()).collect(),
		contract,
	}
}

fn tool_descriptor(
	name: &str,
	required_fields: &[&str],
	required_capabilities: &[&str],
) -> ToolDescriptor {
	let runtime_constraints = RuntimeConstraints {
		timeout_ms: 10_000,
		max_retries: 0,
		retry_backoff_ms: 0,
		sandbox_profile: SandboxProfile::ReadOnlyFs,
		deterministic_hooks: true,
		allowed_read_roots: default_allowed_roots(),
		allowed_write_roots: Vec::new(),
	};
	let contract = fs_tool_contract(name);
	ToolDescriptor {
		name: name.to_string(),
		version: "1.0.0".to_string(),
		input_schema: contract_tool_schema(
			contract.as_ref(),
			&base_required_field_names(required_fields),
		),
		output_schema: contract
			.as_ref()
			.map(|contract| contract.output.observation_schema.clone())
			.unwrap_or_else(|| "result.v1".to_string()),
		required_capabilities: required_capabilities
			.iter()
			.map(|value| (*value).to_string())
			.collect(),
		runtime_constraints,
		contract,
	}
}

fn write_tool_descriptor(
	name: &str,
	required_fields: &[&str],
	required_capabilities: &[&str],
) -> ToolDescriptor {
	let runtime_constraints = RuntimeConstraints {
		timeout_ms: 10_000,
		max_retries: 0,
		retry_backoff_ms: 0,
		sandbox_profile: SandboxProfile::NoIsolation,
		deterministic_hooks: true,
		allowed_read_roots: default_allowed_roots(),
		allowed_write_roots: default_allowed_roots(),
	};
	let contract = fs_tool_contract(name);
	ToolDescriptor {
		name: name.to_string(),
		version: "1.0.0".to_string(),
		input_schema: contract_tool_schema(
			contract.as_ref(),
			&base_required_field_names(required_fields),
		),
		output_schema: contract
			.as_ref()
			.map(|contract| contract.output.observation_schema.clone())
			.unwrap_or_else(|| "result.v1".to_string()),
		required_capabilities: required_capabilities
			.iter()
			.map(|value| (*value).to_string())
			.collect(),
		runtime_constraints,
		contract,
	}
}

fn base_required_field_names<'a>(extra: &'a [&'a str]) -> Vec<&'a str> {
	let mut fields = vec![
		"task_id",
		"node_id",
		"goal",
		"summary",
		"conversation_history",
		"budget_tokens",
		"time_budget_ms",
	];
	for field in extra {
		if !fields.iter().any(|existing| existing == field) {
			fields.push(field);
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

fn canonical_fs_execution(
	tool_name: &str,
	request: &ToolInvocationRequest,
) -> Result<CanonicalExecution, ToolFailure> {
	let path = required_string(&request.input, "path")?;
	let roots = allowed_read_roots(request)?;
	let working_directory = roots.first().cloned().ok_or_else(|| {
		ToolFailure::terminal("no allowed read roots are configured for filesystem tools")
	})?;
	let resolved_target = resolve_scope_target(path, &working_directory)?;
	let env_policy = ExecutionEnvPolicy {
		mode: ExecutionEnvPolicyMode::Clean,
		allowed_keys: Vec::new(),
	};
	let resource_scope = ExecutionResourceScope {
		working_directory: working_directory.display().to_string(),
		resolved_targets: vec![resolved_target.display().to_string()],
		effective_read_roots: path_strings(&roots),
		effective_write_roots: Vec::new(),
	};
	let digest = compute_fs_digest(tool_name, &working_directory, &env_policy, &resource_scope)?;

	Ok(CanonicalExecution {
		tool_name: tool_name.to_string(),
		program: tool_name.to_string(),
		argv: vec![tool_name.to_string(), resolved_target.display().to_string()],
		invocation_mode: InvocationMode::DirectExec,
		shell_context: None,
		cwd: working_directory.display().to_string(),
		env_policy,
		resource_scope,
		action_class: ExecutionActionClass::Read,
		digest,
	})
}

fn compute_fs_digest(
	tool_name: &str,
	working_directory: &Path,
	env_policy: &ExecutionEnvPolicy,
	resource_scope: &ExecutionResourceScope,
) -> Result<CanonicalDigest, ToolFailure> {
	let payload = json!({
		"tool_name": tool_name,
		"program": tool_name,
		"argv": [
			tool_name,
			resource_scope
				.resolved_targets
				.first()
				.cloned()
				.unwrap_or_default()
		],
		"invocation_mode": InvocationMode::DirectExec,
		"cwd": working_directory.display().to_string(),
		"env_policy": env_policy,
		"resource_scope": resource_scope,
		"action_class": ExecutionActionClass::Read,
	});
	let bytes = serde_json::to_vec(&payload).map_err(|error| {
		ToolFailure::terminal(format!(
			"failed to encode canonical fs digest input: {error}"
		))
	})?;
	let mut hasher = Sha256::new();
	hasher.update(bytes);
	let digest = hasher.finalize();
	Ok(CanonicalDigest(
		digest
			.iter()
			.map(|b| format!("{b:02x}"))
			.collect::<String>(),
	))
}

fn compute_fs_write_digest(
	tool_name: &str,
	argv: &[String],
	working_directory: &Path,
	env_policy: &ExecutionEnvPolicy,
	resource_scope: &ExecutionResourceScope,
) -> Result<CanonicalDigest, ToolFailure> {
	let payload = json!({
		"tool_name": tool_name,
		"program": tool_name,
		"argv": argv,
		"invocation_mode": InvocationMode::DirectExec,
		"cwd": working_directory.display().to_string(),
		"env_policy": env_policy,
		"resource_scope": resource_scope,
		"action_class": ExecutionActionClass::Write,
	});
	let bytes = serde_json::to_vec(&payload).map_err(|error| {
		ToolFailure::terminal(format!(
			"failed to encode canonical fs write digest input: {error}"
		))
	})?;
	let mut hasher = Sha256::new();
	hasher.update(bytes);
	let digest = hasher.finalize();
	Ok(CanonicalDigest(
		digest
			.iter()
			.map(|b| format!("{b:02x}"))
			.collect::<String>(),
	))
}

fn path_strings(paths: &[PathBuf]) -> Vec<String> {
	paths
		.iter()
		.map(|path| path.display().to_string())
		.collect()
}

fn evaluate_fs_policy(execution: &CanonicalExecution) -> PolicyDecision {
	if !path_in_any_root(
		&execution.cwd,
		&execution.resource_scope.effective_read_roots,
	) {
		return require_fs_approval(PolicyReasonCode::ApprovalRequiredByOutOfScopePath);
	}
	if execution
		.resource_scope
		.resolved_targets
		.iter()
		.any(|target| !path_in_any_root(target, &execution.resource_scope.effective_read_roots))
	{
		return require_fs_approval(PolicyReasonCode::ApprovalRequiredByOutOfScopePath);
	}
	allow_fs()
}

fn evaluate_fs_write_policy(execution: &CanonicalExecution) -> PolicyDecision {
	if execution
		.resource_scope
		.resolved_targets
		.iter()
		.any(|target| !path_in_any_root(target, &execution.resource_scope.effective_write_roots))
	{
		return require_fs_approval(PolicyReasonCode::ApprovalRequiredByOutOfScopePath);
	}
	// PRD-01 US-002: create-vs-overwrite policy seam.
	// Today both are allowed by default. Downstream policy can inspect
	// is_overwrite_execution() to require higher-level approval for overwrites.
	// This seam exists so that future policy tightening does not require
	// structural changes — only a policy condition update here.
	let _overwrite = is_overwrite_execution(execution);
	allow_fs()
}

fn canonical_fs_write_execution(
	tool_name: &str,
	request: &ToolInvocationRequest,
) -> Result<CanonicalExecution, ToolFailure> {
	let file_path = required_string(&request.input, "file_path")?;
	let roots = allowed_write_roots(request)?;
	let working_directory = roots.first().cloned().ok_or_else(|| {
		ToolFailure::terminal("no allowed write roots are configured for filesystem tools")
	})?;
	let resolved_target = resolve_scope_target(file_path, &working_directory)?;
	// Encode create-vs-overwrite distinction in argv so the policy bridge can
	// differentiate. When the target already exists, argv includes "--overwrite".
	// This is the policy seam required by PRD-01 US-002 — downstream policy can
	// require higher-level approval for overwrites if configured.
	let overwrite_existing = resolved_target.exists();
	let mut argv = vec![tool_name.to_string(), resolved_target.display().to_string()];
	if overwrite_existing {
		argv.push("--overwrite".to_string());
	}
	let env_policy = ExecutionEnvPolicy {
		mode: ExecutionEnvPolicyMode::Clean,
		allowed_keys: Vec::new(),
	};
	let resource_scope = ExecutionResourceScope {
		working_directory: working_directory.display().to_string(),
		resolved_targets: vec![resolved_target.display().to_string()],
		effective_read_roots: Vec::new(),
		effective_write_roots: path_strings(&roots),
	};
	let digest = compute_fs_write_digest(
		tool_name,
		&argv,
		&working_directory,
		&env_policy,
		&resource_scope,
	)?;

	Ok(CanonicalExecution {
		tool_name: tool_name.to_string(),
		program: tool_name.to_string(),
		argv,
		invocation_mode: InvocationMode::DirectExec,
		shell_context: None,
		cwd: working_directory.display().to_string(),
		env_policy,
		resource_scope,
		action_class: ExecutionActionClass::Write,
		digest,
	})
}

/// Returns true if this canonical write execution represents an overwrite of an existing file.
fn is_overwrite_execution(execution: &CanonicalExecution) -> bool {
	execution.argv.iter().any(|arg| arg == "--overwrite")
}

fn allowed_write_roots(request: &ToolInvocationRequest) -> Result<Vec<PathBuf>, ToolFailure> {
	let mut roots = if request.allowed_write_roots.is_empty() {
		default_allowed_roots()
	} else {
		request.allowed_write_roots.clone()
	};
	if roots.is_empty() {
		return Err(ToolFailure::terminal(
			"no allowed write roots are configured for filesystem tools",
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
				"failed to canonicalize write root `{}`: {error}",
				root.display()
			))
		})?;
	}
	Ok(roots)
}

fn ensure_allowed_write(path: &Path, roots: &[PathBuf]) -> Result<(), ToolFailure> {
	// For write targets, the path (or ancestors) may not yet exist, so we walk
	// up to the nearest existing ancestor and canonicalize from there.
	let check = nearest_canonical(path).unwrap_or_else(|| path.to_path_buf());
	if roots.iter().any(|root| check.starts_with(root)) {
		Ok(())
	} else {
		Err(ToolFailure::terminal(format!(
			"path `{}` is outside the allowed write roots",
			path.display()
		)))
	}
}

/// Walk up the ancestor chain until we find an existing directory that can be
/// canonicalized, then re-append the remaining suffix.
fn nearest_canonical(path: &Path) -> Option<PathBuf> {
	let mut current = path.to_path_buf();
	let mut suffix_parts: Vec<std::ffi::OsString> = Vec::new();
	loop {
		if current.exists() {
			let mut canonical = current.canonicalize().ok()?;
			for part in suffix_parts.into_iter().rev() {
				canonical.push(part);
			}
			return Some(canonical);
		}
		if let Some(name) = current.file_name() {
			suffix_parts.push(name.to_os_string());
		} else {
			return None;
		}
		if !current.pop() {
			return None;
		}
	}
}

fn allow_fs() -> PolicyDecision {
	PolicyDecision {
		outcome: PolicyOutcome::Allow,
		reason_code: PolicyReasonCode::AllowedByPolicy,
		approval_requirement: None,
	}
}

fn require_fs_approval(reason_code: PolicyReasonCode) -> PolicyDecision {
	PolicyDecision {
		outcome: PolicyOutcome::RequireApproval,
		reason_code,
		approval_requirement: Some(ApprovalRequirement {
			scope: ApprovalRequirementScope::Invocation,
			reason_code,
		}),
	}
}

fn path_in_any_root(path: &str, roots: &[String]) -> bool {
	if roots.is_empty() {
		return true;
	}

	roots
		.iter()
		.any(|root| Path::new(path).starts_with(Path::new(root)))
}

fn resolve_scope_target(raw: &str, working_directory: &Path) -> Result<PathBuf, ToolFailure> {
	let candidate = expand_user_path(raw, working_directory);
	if candidate.exists() {
		return candidate
			.canonicalize()
			.map_err(|error| ToolFailure::terminal(format!("failed to resolve `{raw}`: {error}")));
	}
	if let Some(parent) = candidate.parent()
		&& parent.exists()
	{
		let canonical_parent = parent.canonicalize().map_err(|error| {
			ToolFailure::terminal(format!("failed to resolve parent of `{raw}`: {error}"))
		})?;
		if let Some(name) = candidate.file_name() {
			return Ok(canonical_parent.join(name));
		}
		return Ok(canonical_parent);
	}
	Ok(candidate)
}

fn expand_user_path(raw: &str, working_directory: &Path) -> PathBuf {
	let trimmed = raw.trim();
	if trimmed == "~" {
		return home_dir().unwrap_or_else(|| PathBuf::from(trimmed));
	}
	if let Some(suffix) = trimmed
		.strip_prefix("~/")
		.or_else(|| trimmed.strip_prefix("~\\"))
		&& let Some(home) = home_dir()
	{
		return if suffix.is_empty() {
			home
		} else {
			home.join(suffix)
		};
	}
	let candidate = PathBuf::from(trimmed);
	if candidate.is_absolute() {
		candidate
	} else {
		working_directory.join(candidate)
	}
}

fn home_dir() -> Option<PathBuf> {
	env::var_os("HOME")
		.map(PathBuf::from)
		.or_else(|| env::var_os("USERPROFILE").map(PathBuf::from))
		.or_else(|| {
			let drive = env::var_os("HOMEDRIVE")?;
			let path = env::var_os("HOMEPATH")?;
			Some(PathBuf::from(format!(
				"{}{}",
				drive.to_string_lossy(),
				path.to_string_lossy()
			)))
		})
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

fn resolve_existing_path(
	raw: &str,
	roots: &[PathBuf],
	descendant_scan_limit: usize,
) -> Result<PathBuf, ToolFailure> {
	let candidate = resolve_candidate_path(raw, roots, descendant_scan_limit)?;
	let canonical = candidate
		.canonicalize()
		.map_err(|error| ToolFailure::terminal(format!("failed to resolve `{raw}`: {error}")))?;
	ensure_allowed(&canonical, roots)?;
	Ok(canonical)
}

fn resolve_candidate_path(
	raw: &str,
	roots: &[PathBuf],
	descendant_scan_limit: usize,
) -> Result<PathBuf, ToolFailure> {
	let base = roots
		.first()
		.cloned()
		.unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
	let path = PathBuf::from(raw);
	let candidate = expand_user_path(raw, &base);
	if candidate.exists() {
		let canonical = candidate.canonicalize().map_err(|error| {
			ToolFailure::terminal(format!("failed to resolve `{raw}`: {error}"))
		})?;
		ensure_allowed(&canonical, roots)?;
		return Ok(canonical);
	}
	if !path.is_absolute()
		&& !raw.starts_with('~')
		&& !raw.contains('/')
		&& !raw.contains('\\')
		&& let Some(resolved) = find_unique_descendant_match(raw, roots, descendant_scan_limit)?
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
	descendant_scan_limit: usize,
) -> Result<Option<PathBuf>, ToolFailure> {
	let matches = find_descendant_matches(target_name, "any", roots, descendant_scan_limit)?;
	if matches.matches.len() > 1 {
		return Err(ToolFailure::terminal(format!(
			"`{target_name}` is ambiguous under the allowed read roots; please provide a more specific path"
		)));
	}
	Ok(matches.matches.into_iter().next().map(PathBuf::from))
}

fn should_skip_workspace_search_dir(name: &str) -> bool {
	matches!(
		name,
		".git" | ".roku" | "target" | "node_modules" | "dist" | "build"
	)
}

#[derive(Debug, Clone)]
struct DescendantMatchSummary {
	matches: Vec<String>,
	exact_match_count: usize,
	fuzzy_match_count: usize,
	match_mode: &'static str,
}

fn find_descendant_matches(
	target_name: &str,
	kind: &str,
	roots: &[PathBuf],
	descendant_scan_limit: usize,
) -> Result<DescendantMatchSummary, ToolFailure> {
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
				if visited > descendant_scan_limit.min(HARD_MAX_DESCENDANT_SCAN_ENTRIES) {
					return Ok(summarize_descendant_matches(exact_matches, fuzzy_matches));
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
	Ok(summarize_descendant_matches(exact_matches, fuzzy_matches))
}

fn summarize_descendant_matches(
	exact_matches: Vec<String>,
	mut fuzzy_matches: Vec<(u8, String)>,
) -> DescendantMatchSummary {
	let exact_match_count = exact_matches.len();
	let fuzzy_match_count = fuzzy_matches.len();
	if !exact_matches.is_empty() {
		let match_mode = match exact_match_count {
			1 => "unique_exact",
			_ => "ambiguous_exact",
		};
		return DescendantMatchSummary {
			matches: exact_matches,
			exact_match_count,
			fuzzy_match_count: 0,
			match_mode,
		};
	}
	if fuzzy_matches.is_empty() {
		return DescendantMatchSummary {
			matches: Vec::new(),
			exact_match_count: 0,
			fuzzy_match_count: 0,
			match_mode: "none",
		};
	}
	fuzzy_matches.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
	let best_score = fuzzy_matches.first().map(|(score, _)| *score).unwrap_or(0);
	let matches = fuzzy_matches
		.into_iter()
		.filter(|(score, _)| *score == best_score)
		.map(|(_, path)| path)
		.collect::<Vec<_>>();
	let match_mode = match matches.len() {
		1 => "unique_fuzzy",
		_ => "ambiguous_mixed",
	};
	DescendantMatchSummary {
		matches,
		exact_match_count,
		fuzzy_match_count,
		match_mode,
	}
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

// ---------------------------------------------------------------------------
// fs.grep
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct FsGrepTool {
	config: FsToolRuntimeConfig,
}

impl Tool for FsGrepTool {
	fn descriptor(&self) -> ToolDescriptor {
		let runtime_constraints = RuntimeConstraints {
			timeout_ms: 15_000,
			max_retries: 0,
			retry_backoff_ms: 0,
			sandbox_profile: SandboxProfile::ReadOnlyFs,
			deterministic_hooks: true,
			allowed_read_roots: Vec::new(),
			allowed_write_roots: Vec::new(),
		};
		ToolDescriptor {
			name: "fs.grep".to_string(),
			version: "1.0.0".to_string(),
			input_schema: contract_tool_schema(None, &base_required_field_names(&["pattern"])),
			output_schema: "tool_observation.v1".to_string(),
			required_capabilities: vec!["fs.grep".to_string()],
			runtime_constraints,
			contract: None,
		}
	}

	fn invoke(&self, request: ToolInvocationRequest) -> Result<Value, ToolFailure> {
		let pattern = required_string(&request.input, "pattern")?;
		let literal = request
			.input
			.get("literal")
			.and_then(Value::as_bool)
			.unwrap_or(false);
		let effective_pattern = if literal {
			regex::escape(pattern)
		} else {
			pattern.to_string()
		};
		let compiled = regex::Regex::new(&effective_pattern)
			.map_err(|error| ToolFailure::terminal(format!("invalid regex pattern: {error}")))?;
		let glob_filter = request
			.input
			.get("glob")
			.and_then(Value::as_str)
			.filter(|value| !value.trim().is_empty())
			.map(|value| {
				glob::Pattern::new(value).map_err(|error| {
					ToolFailure::terminal(format!("invalid glob filter `{value}`: {error}"))
				})
			})
			.transpose()?;
		let file_type = request
			.input
			.get("file_type")
			.and_then(Value::as_str)
			.filter(|value| !value.trim().is_empty())
			.map(str::to_string);
		let context_before = request
			.input
			.get("context_before")
			.and_then(Value::as_u64)
			.unwrap_or(0) as usize;
		let context_after = request
			.input
			.get("context_after")
			.and_then(Value::as_u64)
			.unwrap_or(0) as usize;
		let roots = allowed_read_roots(&request)?;
		let search_root = request
			.input
			.get("path")
			.and_then(Value::as_str)
			.filter(|value| !value.trim().is_empty())
			.map(|value| {
				let candidate = expand_user_path(value, roots.first().unwrap());
				if !candidate.is_dir() {
					return Err(ToolFailure::terminal(format!(
						"`{value}` is not a directory"
					)));
				}
				let canonical = candidate.canonicalize().map_err(|error| {
					ToolFailure::terminal(format!("failed to resolve `{value}`: {error}"))
				})?;
				ensure_allowed(&canonical, &roots)?;
				Ok(canonical)
			})
			.transpose()?
			.unwrap_or_else(|| roots.first().cloned().unwrap_or_default());
		let limit = self.config.max_grep_results;
		let mut matches: Vec<Value> = Vec::new();
		let mut truncated = false;
		grep_walk_directory(
			&search_root,
			&compiled,
			glob_filter.as_ref(),
			file_type.as_deref(),
			context_before,
			context_after,
			limit,
			&mut matches,
			&mut truncated,
		)?;
		let match_count = matches.len();
		let message = if match_count == 0 {
			format!("No matches found for `{pattern}`.")
		} else if truncated {
			format!("Found {match_count} matches for `{pattern}` (truncated at {limit}).")
		} else {
			format!("Found {match_count} matches for `{pattern}`.")
		};
		let data = json!({
			"pattern": pattern,
			"matches": matches,
			"match_count": match_count,
			"truncated": truncated,
		});
		Ok(observation_like_output(message, true, None, false, data))
	}
}

#[allow(dead_code)]
fn grep_walk_directory(
	directory: &Path,
	pattern: &regex::Regex,
	glob_filter: Option<&glob::Pattern>,
	file_type: Option<&str>,
	context_before: usize,
	context_after: usize,
	limit: usize,
	matches: &mut Vec<Value>,
	truncated: &mut bool,
) -> Result<(), ToolFailure> {
	let mut stack = vec![directory.to_path_buf()];
	while let Some(dir) = stack.pop() {
		let entries = match fs::read_dir(&dir) {
			Ok(entries) => entries,
			Err(_) => continue,
		};
		for entry in entries.filter_map(Result::ok) {
			let path = entry.path();
			let name = entry.file_name().to_string_lossy().to_string();
			let metadata = match fs::symlink_metadata(&path) {
				Ok(metadata) => metadata,
				Err(_) => continue,
			};
			if metadata.is_dir() {
				if !should_skip_workspace_search_dir(&name) {
					stack.push(path);
				}
				continue;
			}
			if !metadata.is_file() {
				continue;
			}
			if let Some(filter) = glob_filter
				&& !filter.matches(&name)
			{
				continue;
			}
			if let Some(ext) = file_type {
				let matches_ext = path
					.extension()
					.and_then(|e| e.to_str())
					.map(|e| e == ext)
					.unwrap_or(false);
				if !matches_ext {
					continue;
				}
			}
			if matches.len() >= limit {
				*truncated = true;
				return Ok(());
			}
			grep_search_file(
				&path,
				pattern,
				context_before,
				context_after,
				limit,
				matches,
				truncated,
			)?;
			if *truncated {
				return Ok(());
			}
		}
	}
	Ok(())
}

#[allow(dead_code)]
fn grep_search_file(
	path: &Path,
	pattern: &regex::Regex,
	context_before: usize,
	context_after: usize,
	limit: usize,
	matches: &mut Vec<Value>,
	truncated: &mut bool,
) -> Result<(), ToolFailure> {
	let content = match fs::read_to_string(path) {
		Ok(content) => content,
		Err(_) => return Ok(()),
	};
	if context_before == 0 && context_after == 0 {
		for (line_number, line) in content.lines().enumerate() {
			if pattern.is_match(line) {
				if matches.len() >= limit {
					*truncated = true;
					return Ok(());
				}
				matches.push(json!({
					"file_path": path.display().to_string(),
					"line_number": line_number + 1,
					"line_content": line,
				}));
			}
		}
	} else {
		let lines: Vec<&str> = content.lines().collect();
		let total = lines.len();
		// collect match line indices first, then emit with context, merging overlaps
		let match_indices: Vec<usize> = lines
			.iter()
			.enumerate()
			.filter(|(_, line)| pattern.is_match(line))
			.map(|(i, _)| i)
			.collect();
		// emit merged context windows
		let mut emitted_up_to: Option<usize> = None; // last line index already emitted
		for &idx in &match_indices {
			let window_start = idx.saturating_sub(context_before);
			let window_end = (idx + context_after).min(total - 1);
			// skip lines already emitted via a previous window
			let emit_from = match emitted_up_to {
				Some(last) if last >= window_start => last + 1,
				_ => window_start,
			};
			for (line_idx, line) in lines
				.iter()
				.enumerate()
				.take(window_end + 1)
				.skip(emit_from)
			{
				if matches.len() >= limit {
					*truncated = true;
					return Ok(());
				}
				let is_match = line_idx == idx || match_indices.binary_search(&line_idx).is_ok();
				matches.push(json!({
					"file_path": path.display().to_string(),
					"line_number": line_idx + 1,
					"line_content": *line,
					"is_context": !is_match,
				}));
			}
			// advance emitted_up_to only forward, never backward
			emitted_up_to = Some(match emitted_up_to {
				Some(prev) if prev > window_end => prev,
				_ => window_end,
			});
		}
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;
	use roku_common_types::ToolOutputEnvelope;
	use serde_json::json;
	use tempfile::tempdir;

	#[test]
	fn canonical_execution_from_runtime_input_preserves_out_of_scope_target() {
		let directory = tempdir().expect("tempdir should succeed");
		let target = directory
			.path()
			.canonicalize()
			.expect("tempdir should canonicalize");

		let execution = canonical_execution_from_runtime_input(
			"fs.list_dir",
			&json!({ "path": target.display().to_string() }),
		)
		.expect("filesystem canonical execution should project");

		assert_eq!(execution.tool_name, "fs.list_dir");
		assert_eq!(
			execution.resource_scope.resolved_targets,
			vec![target.display().to_string()]
		);
	}

	#[test]
	fn fs_policy_requires_approval_for_out_of_scope_paths() {
		let execution = CanonicalExecution {
			tool_name: "fs.list_dir".to_string(),
			program: "fs.list_dir".to_string(),
			argv: vec!["fs.list_dir".to_string(), "/tmp/outside".to_string()],
			invocation_mode: InvocationMode::DirectExec,
			shell_context: None,
			cwd: "/workspace".to_string(),
			env_policy: ExecutionEnvPolicy {
				mode: ExecutionEnvPolicyMode::Clean,
				allowed_keys: Vec::new(),
			},
			resource_scope: ExecutionResourceScope {
				working_directory: "/workspace".to_string(),
				resolved_targets: vec!["/tmp/outside".to_string()],
				effective_read_roots: vec!["/workspace".to_string()],
				effective_write_roots: Vec::new(),
			},
			action_class: ExecutionActionClass::Read,
			digest: CanonicalDigest("digest-fs-out-of-scope".to_string()),
		};

		let decision = evaluate_fs_policy(&execution);
		assert_eq!(decision.outcome, PolicyOutcome::RequireApproval);
		assert_eq!(
			decision.reason_code,
			PolicyReasonCode::ApprovalRequiredByOutOfScopePath
		);
	}

	#[test]
	fn fs_exists_emits_tool_output_envelope() {
		let directory = tempdir().expect("tempdir should succeed");
		fs::write(directory.path().join("note.txt"), "hello").expect("fixture should write");
		let tool = FsExistsTool {
			config: FsToolRuntimeConfig::default(),
		};

		let output = tool
			.invoke(ToolInvocationRequest {
				invocation_key: "fs.exists:test".to_string(),
				attempt: 1,
				input: json!({ "path": "note.txt" }),
				sandbox_profile: SandboxProfile::ReadOnlyFs,
				attachments: Vec::new(),
				allowed_read_roots: vec![directory.path().to_path_buf()],
				allowed_write_roots: Vec::new(),
			})
			.expect("fs.exists invocation should succeed");

		let envelope = serde_json::from_value::<ToolOutputEnvelope>(output)
			.expect("fs.exists output should deserialize as ToolOutputEnvelope");
		assert!(envelope.ok);
		assert!(!envelope.terminal);
		assert_eq!(envelope.error_type, None);
		let observed_path = envelope.data["path"]
			.as_str()
			.expect("fs.exists data.path should be a string");
		assert!(observed_path.ends_with("/note.txt"));
		assert_eq!(
			envelope.message,
			format!("`{observed_path}` exists as file.")
		);
		assert_eq!(envelope.data["exists"], true);
		assert_eq!(envelope.data["kind"], "file");
	}

	// -----------------------------------------------------------------------
	// fs.grep tests
	// -----------------------------------------------------------------------

	fn grep_tool() -> FsGrepTool {
		FsGrepTool {
			config: FsToolRuntimeConfig::default(),
		}
	}

	fn grep_request(input: Value, root: &Path) -> ToolInvocationRequest {
		ToolInvocationRequest {
			invocation_key: "fs.grep:test".to_string(),
			attempt: 1,
			input,
			sandbox_profile: SandboxProfile::ReadOnlyFs,
			attachments: Vec::new(),
			allowed_read_roots: vec![root.to_path_buf()],
			allowed_write_roots: Vec::new(),
		}
	}

	#[test]
	fn fs_grep_single_match() {
		let dir = tempdir().expect("tempdir");
		fs::write(dir.path().join("a.txt"), "hello world\ngoodbye\n").unwrap();
		let output = grep_tool()
			.invoke(grep_request(json!({"pattern": "hello"}), dir.path()))
			.expect("invoke");
		let env = serde_json::from_value::<ToolOutputEnvelope>(output).expect("envelope");
		assert!(env.ok);
		let matches = env.data["matches"].as_array().unwrap();
		assert_eq!(matches.len(), 1);
		assert_eq!(matches[0]["line_number"], 1);
		assert!(
			matches[0]["line_content"]
				.as_str()
				.unwrap()
				.contains("hello")
		);
	}

	#[test]
	fn fs_grep_multiple_matches() {
		let dir = tempdir().expect("tempdir");
		fs::write(dir.path().join("a.txt"), "foo\nbar\nfoo again\n").unwrap();
		let output = grep_tool()
			.invoke(grep_request(json!({"pattern": "foo"}), dir.path()))
			.expect("invoke");
		let env = serde_json::from_value::<ToolOutputEnvelope>(output).expect("envelope");
		assert!(env.ok);
		assert_eq!(env.data["match_count"], 2);
	}

	#[test]
	fn fs_grep_regex_pattern() {
		let dir = tempdir().expect("tempdir");
		fs::write(dir.path().join("a.txt"), "abc123\ndef456\nabc789\n").unwrap();
		let output = grep_tool()
			.invoke(grep_request(json!({"pattern": "abc\\d+"}), dir.path()))
			.expect("invoke");
		let env = serde_json::from_value::<ToolOutputEnvelope>(output).expect("envelope");
		assert!(env.ok);
		assert_eq!(env.data["match_count"], 2);
	}

	#[test]
	fn fs_grep_literal_pattern() {
		let dir = tempdir().expect("tempdir");
		fs::write(dir.path().join("a.txt"), "a.b\naxb\na\\.b\n").unwrap();
		let output = grep_tool()
			.invoke(grep_request(
				json!({"pattern": "a.b", "literal": true}),
				dir.path(),
			))
			.expect("invoke");
		let env = serde_json::from_value::<ToolOutputEnvelope>(output).expect("envelope");
		assert!(env.ok);
		// literal "a.b" matches only the first line, not "axb"
		assert_eq!(env.data["match_count"], 1);
		let matches = env.data["matches"].as_array().unwrap();
		assert_eq!(matches[0]["line_content"], "a.b");
	}

	#[test]
	fn fs_grep_invalid_regex_returns_error() {
		let dir = tempdir().expect("tempdir");
		fs::write(dir.path().join("a.txt"), "text").unwrap();
		let result = grep_tool().invoke(grep_request(json!({"pattern": "[invalid"}), dir.path()));
		assert!(result.is_err());
	}

	#[test]
	fn fs_grep_no_matches_returns_success() {
		let dir = tempdir().expect("tempdir");
		fs::write(dir.path().join("a.txt"), "hello world\n").unwrap();
		let output = grep_tool()
			.invoke(grep_request(json!({"pattern": "zzz_no_match"}), dir.path()))
			.expect("invoke");
		let env = serde_json::from_value::<ToolOutputEnvelope>(output).expect("envelope");
		assert!(env.ok);
		assert_eq!(env.data["match_count"], 0);
		assert_eq!(env.data["truncated"], false);
	}

	#[test]
	fn fs_grep_truncation_at_limit() {
		let dir = tempdir().expect("tempdir");
		let lines = (0..300)
			.map(|i| format!("match_{i}"))
			.collect::<Vec<_>>()
			.join("\n");
		fs::write(dir.path().join("a.txt"), &lines).unwrap();
		let tool = FsGrepTool {
			config: FsToolRuntimeConfig {
				max_grep_results: 5,
				..FsToolRuntimeConfig::default()
			},
		};
		let output = tool
			.invoke(grep_request(json!({"pattern": "match_"}), dir.path()))
			.expect("invoke");
		let env = serde_json::from_value::<ToolOutputEnvelope>(output).expect("envelope");
		assert!(env.ok);
		assert_eq!(env.data["match_count"], 5);
		assert_eq!(env.data["truncated"], true);
	}

	#[test]
	fn fs_grep_glob_filter() {
		let dir = tempdir().expect("tempdir");
		fs::write(dir.path().join("a.rs"), "fn main() {}\n").unwrap();
		fs::write(dir.path().join("b.txt"), "fn main() {}\n").unwrap();
		let output = grep_tool()
			.invoke(grep_request(
				json!({"pattern": "fn main", "glob": "*.rs"}),
				dir.path(),
			))
			.expect("invoke");
		let env = serde_json::from_value::<ToolOutputEnvelope>(output).expect("envelope");
		assert!(env.ok);
		assert_eq!(env.data["match_count"], 1);
		let matches = env.data["matches"].as_array().unwrap();
		assert!(matches[0]["file_path"].as_str().unwrap().ends_with("a.rs"));
	}

	#[test]
	fn fs_grep_file_type_filter() {
		let dir = tempdir().expect("tempdir");
		fs::write(dir.path().join("a.rs"), "fn main() {}\n").unwrap();
		fs::write(dir.path().join("b.py"), "fn main() {}\n").unwrap();
		fs::write(dir.path().join("c.txt"), "fn main() {}\n").unwrap();
		let output = grep_tool()
			.invoke(grep_request(
				json!({"pattern": "fn main", "file_type": "rs"}),
				dir.path(),
			))
			.expect("invoke");
		let env = serde_json::from_value::<ToolOutputEnvelope>(output).expect("envelope");
		assert!(env.ok);
		assert_eq!(env.data["match_count"], 1);
		let matches = env.data["matches"].as_array().unwrap();
		assert!(matches[0]["file_path"].as_str().unwrap().ends_with("a.rs"));
	}

	#[test]
	fn fs_grep_file_type_no_match_extension() {
		let dir = tempdir().expect("tempdir");
		fs::write(dir.path().join("a.txt"), "hello world\n").unwrap();
		let output = grep_tool()
			.invoke(grep_request(
				json!({"pattern": "hello", "file_type": "rs"}),
				dir.path(),
			))
			.expect("invoke");
		let env = serde_json::from_value::<ToolOutputEnvelope>(output).expect("envelope");
		assert!(env.ok);
		assert_eq!(env.data["match_count"], 0);
	}

	#[test]
	fn fs_grep_context_before_and_after() {
		let dir = tempdir().expect("tempdir");
		// lines: line1, line2, MATCH, line4, line5
		fs::write(
			dir.path().join("a.txt"),
			"line1\nline2\nMATCH\nline4\nline5\n",
		)
		.unwrap();
		let output = grep_tool()
			.invoke(grep_request(
				json!({"pattern": "MATCH", "context_before": 2, "context_after": 2}),
				dir.path(),
			))
			.expect("invoke");
		let env = serde_json::from_value::<ToolOutputEnvelope>(output).expect("envelope");
		assert!(env.ok);
		let matches = env.data["matches"].as_array().unwrap();
		// expect 5 entries: lines 1-5
		assert_eq!(matches.len(), 5);
		assert_eq!(matches[0]["line_number"], 1);
		assert_eq!(matches[0]["is_context"], true);
		assert_eq!(matches[2]["line_number"], 3);
		assert_eq!(matches[2]["is_context"], false);
		assert_eq!(matches[4]["line_number"], 5);
		assert_eq!(matches[4]["is_context"], true);
	}

	#[test]
	fn fs_grep_context_overlapping_windows_merged() {
		let dir = tempdir().expect("tempdir");
		// lines: MATCH1, line2, MATCH3
		// context_after=2 from MATCH1 reaches line3; context_before=2 from MATCH3 starts at line1
		// merged window should be lines 1-3 with no duplicates
		fs::write(dir.path().join("a.txt"), "MATCH1\nline2\nMATCH3\n").unwrap();
		let output = grep_tool()
			.invoke(grep_request(
				json!({"pattern": "MATCH", "context_before": 2, "context_after": 2}),
				dir.path(),
			))
			.expect("invoke");
		let env = serde_json::from_value::<ToolOutputEnvelope>(output).expect("envelope");
		assert!(env.ok);
		let matches = env.data["matches"].as_array().unwrap();
		// should be exactly 3 lines (no duplicates)
		assert_eq!(matches.len(), 3);
		let line_numbers: Vec<u64> = matches
			.iter()
			.map(|m| m["line_number"].as_u64().unwrap())
			.collect();
		assert_eq!(line_numbers, vec![1, 2, 3]);
	}

	#[test]
	fn fs_grep_context_zero_behaves_like_default() {
		let dir = tempdir().expect("tempdir");
		fs::write(dir.path().join("a.txt"), "hello world\ngoodbye\n").unwrap();
		let output = grep_tool()
			.invoke(grep_request(
				json!({"pattern": "hello", "context_before": 0, "context_after": 0}),
				dir.path(),
			))
			.expect("invoke");
		let env = serde_json::from_value::<ToolOutputEnvelope>(output).expect("envelope");
		assert!(env.ok);
		let matches = env.data["matches"].as_array().unwrap();
		assert_eq!(matches.len(), 1);
		assert_eq!(matches[0]["line_number"], 1);
		// no is_context field in default (zero-context) path
		assert!(matches[0].get("is_context").is_none());
	}

	// -----------------------------------------------------------------------
	// fs.edit tests
	// -----------------------------------------------------------------------

	fn write_request(input: Value, write_roots: Vec<PathBuf>) -> ToolInvocationRequest {
		ToolInvocationRequest {
			invocation_key: "fs-mutation:test".to_string(),
			attempt: 1,
			input,
			sandbox_profile: SandboxProfile::ReadOnlyFs,
			attachments: Vec::new(),
			allowed_read_roots: Vec::new(),
			allowed_write_roots: write_roots,
		}
	}

	#[test]
	fn fs_edit_unique_match_succeeds() {
		let directory = tempdir().expect("tempdir should succeed");
		let file = directory.path().join("hello.txt");
		fs::write(&file, "Hello World").expect("fixture write should succeed");
		let tool = FsEditTool {
			config: FsToolRuntimeConfig::default(),
		};

		let output = tool
			.invoke(write_request(
				json!({
					"file_path": file.display().to_string(),
					"old_string": "World",
					"new_string": "Rust",
				}),
				vec![directory.path().to_path_buf()],
			))
			.expect("fs.edit invocation should succeed");

		let envelope = serde_json::from_value::<ToolOutputEnvelope>(output)
			.expect("fs.edit output should deserialize as ToolOutputEnvelope");
		assert!(envelope.ok);
		assert_eq!(envelope.error_type, None);
		assert_eq!(envelope.data["match_count"], 1);
		assert_eq!(envelope.data["line_start"], 1);
		assert_eq!(envelope.data["line_end"], 1);

		let content = fs::read_to_string(&file).expect("file should be readable");
		assert_eq!(content, "Hello Rust");
	}

	#[test]
	fn fs_edit_zero_matches_returns_error() {
		let directory = tempdir().expect("tempdir should succeed");
		let file = directory.path().join("hello.txt");
		fs::write(&file, "Hello World").expect("fixture write should succeed");
		let tool = FsEditTool {
			config: FsToolRuntimeConfig::default(),
		};

		let output = tool
			.invoke(write_request(
				json!({
					"file_path": file.display().to_string(),
					"old_string": "nonexistent string",
					"new_string": "replacement",
				}),
				vec![directory.path().to_path_buf()],
			))
			.expect("fs.edit invocation should succeed even for zero matches");

		let envelope = serde_json::from_value::<ToolOutputEnvelope>(output)
			.expect("fs.edit output should deserialize as ToolOutputEnvelope");
		assert!(!envelope.ok);
		assert_eq!(envelope.error_type.as_deref(), Some("string_not_found"));
	}

	#[test]
	fn fs_edit_multiple_matches_returns_error() {
		let directory = tempdir().expect("tempdir should succeed");
		let file = directory.path().join("repeat.txt");
		fs::write(&file, "aaa bbb aaa").expect("fixture write should succeed");
		let tool = FsEditTool {
			config: FsToolRuntimeConfig::default(),
		};

		let output = tool
			.invoke(write_request(
				json!({
					"file_path": file.display().to_string(),
					"old_string": "aaa",
					"new_string": "ccc",
				}),
				vec![directory.path().to_path_buf()],
			))
			.expect("fs.edit invocation should succeed even for multiple matches");

		let envelope = serde_json::from_value::<ToolOutputEnvelope>(output)
			.expect("fs.edit output should deserialize as ToolOutputEnvelope");
		assert!(!envelope.ok);
		assert_eq!(envelope.error_type.as_deref(), Some("multiple_matches"));
		assert_eq!(envelope.data["match_count"], 2);

		let content = fs::read_to_string(&file).expect("file should be readable");
		assert_eq!(content, "aaa bbb aaa", "file should not be modified");
	}

	#[test]
	fn fs_edit_nonexistent_file_returns_error() {
		let directory = tempdir().expect("tempdir should succeed");
		let tool = FsEditTool {
			config: FsToolRuntimeConfig::default(),
		};

		let output = tool
			.invoke(write_request(
				json!({
					"file_path": directory.path().join("missing.txt").display().to_string(),
					"old_string": "foo",
					"new_string": "bar",
				}),
				vec![directory.path().to_path_buf()],
			))
			.expect("fs.edit invocation should succeed even for missing file");

		let envelope = serde_json::from_value::<ToolOutputEnvelope>(output)
			.expect("fs.edit output should deserialize as ToolOutputEnvelope");
		assert!(!envelope.ok);
		assert_eq!(envelope.error_type.as_deref(), Some("file_not_found"));
	}

	// -----------------------------------------------------------------------
	// fs.write tests
	// -----------------------------------------------------------------------

	#[test]
	fn fs_write_creates_new_file() {
		let directory = tempdir().expect("tempdir should succeed");
		let file = directory.path().join("new.txt");
		let tool = FsWriteTool {
			config: FsToolRuntimeConfig::default(),
		};

		let output = tool
			.invoke(write_request(
				json!({
					"file_path": file.display().to_string(),
					"content": "brand new content",
				}),
				vec![directory.path().to_path_buf()],
			))
			.expect("fs.write invocation should succeed");

		let envelope = serde_json::from_value::<ToolOutputEnvelope>(output)
			.expect("fs.write output should deserialize as ToolOutputEnvelope");
		assert!(envelope.ok);
		assert_eq!(envelope.data["created"], true);
		assert_eq!(envelope.data["bytes_written"], 17);

		let content = fs::read_to_string(&file).expect("file should be readable");
		assert_eq!(content, "brand new content");
	}

	#[test]
	fn fs_write_overwrites_existing_file() {
		let directory = tempdir().expect("tempdir should succeed");
		let file = directory.path().join("existing.txt");
		fs::write(&file, "old content").expect("fixture write should succeed");
		let tool = FsWriteTool {
			config: FsToolRuntimeConfig::default(),
		};

		let output = tool
			.invoke(write_request(
				json!({
					"file_path": file.display().to_string(),
					"content": "new content",
				}),
				vec![directory.path().to_path_buf()],
			))
			.expect("fs.write invocation should succeed");

		let envelope = serde_json::from_value::<ToolOutputEnvelope>(output)
			.expect("fs.write output should deserialize as ToolOutputEnvelope");
		assert!(envelope.ok);
		assert_eq!(envelope.data["created"], false);

		let content = fs::read_to_string(&file).expect("file should be readable");
		assert_eq!(content, "new content");
	}

	#[test]
	fn fs_write_creates_parent_directories() {
		let directory = tempdir().expect("tempdir should succeed");
		let file = directory.path().join("deep/nested/dir/file.txt");
		let tool = FsWriteTool {
			config: FsToolRuntimeConfig::default(),
		};

		let output = tool
			.invoke(write_request(
				json!({
					"file_path": file.display().to_string(),
					"content": "nested content",
				}),
				vec![directory.path().to_path_buf()],
			))
			.expect("fs.write invocation should succeed");

		let envelope = serde_json::from_value::<ToolOutputEnvelope>(output)
			.expect("fs.write output should deserialize as ToolOutputEnvelope");
		assert!(envelope.ok);
		assert_eq!(envelope.data["created"], true);

		let content = fs::read_to_string(&file).expect("file should be readable");
		assert_eq!(content, "nested content");
	}

	#[test]
	fn fs_write_empty_content_succeeds() {
		let directory = tempdir().expect("tempdir should succeed");
		let file = directory.path().join("empty.txt");
		let tool = FsWriteTool {
			config: FsToolRuntimeConfig::default(),
		};

		let output = tool
			.invoke(write_request(
				json!({
					"file_path": file.display().to_string(),
					"content": "",
				}),
				vec![directory.path().to_path_buf()],
			))
			.expect("fs.write invocation should succeed");

		let envelope = serde_json::from_value::<ToolOutputEnvelope>(output)
			.expect("fs.write output should deserialize as ToolOutputEnvelope");
		assert!(envelope.ok);
		assert_eq!(envelope.data["created"], true);
		assert_eq!(envelope.data["bytes_written"], 0);

		let content = fs::read_to_string(&file).expect("file should be readable");
		assert_eq!(content, "");
	}

	// -----------------------------------------------------------------------
	// fs write policy tests
	// -----------------------------------------------------------------------

	#[test]
	fn fs_write_policy_requires_approval_for_out_of_scope_paths() {
		let execution = CanonicalExecution {
			tool_name: "fs.edit".to_string(),
			program: "fs.edit".to_string(),
			argv: vec!["fs.edit".to_string(), "/tmp/outside".to_string()],
			invocation_mode: InvocationMode::DirectExec,
			shell_context: None,
			cwd: "/workspace".to_string(),
			env_policy: ExecutionEnvPolicy {
				mode: ExecutionEnvPolicyMode::Clean,
				allowed_keys: Vec::new(),
			},
			resource_scope: ExecutionResourceScope {
				working_directory: "/workspace".to_string(),
				resolved_targets: vec!["/tmp/outside".to_string()],
				effective_read_roots: Vec::new(),
				effective_write_roots: vec!["/workspace".to_string()],
			},
			action_class: ExecutionActionClass::Write,
			digest: CanonicalDigest("digest-fs-write-out-of-scope".to_string()),
		};

		let decision = evaluate_fs_write_policy(&execution);
		assert_eq!(decision.outcome, PolicyOutcome::RequireApproval);
		assert_eq!(
			decision.reason_code,
			PolicyReasonCode::ApprovalRequiredByOutOfScopePath
		);
	}

	#[test]
	fn fs_write_policy_allows_in_scope_paths() {
		let execution = CanonicalExecution {
			tool_name: "fs.write".to_string(),
			program: "fs.write".to_string(),
			argv: vec![
				"fs.write".to_string(),
				"/workspace/new_file.txt".to_string(),
			],
			invocation_mode: InvocationMode::DirectExec,
			shell_context: None,
			cwd: "/workspace".to_string(),
			env_policy: ExecutionEnvPolicy {
				mode: ExecutionEnvPolicyMode::Clean,
				allowed_keys: Vec::new(),
			},
			resource_scope: ExecutionResourceScope {
				working_directory: "/workspace".to_string(),
				resolved_targets: vec!["/workspace/new_file.txt".to_string()],
				effective_read_roots: Vec::new(),
				effective_write_roots: vec!["/workspace".to_string()],
			},
			action_class: ExecutionActionClass::Write,
			digest: CanonicalDigest("digest-fs-write-in-scope".to_string()),
		};

		let decision = evaluate_fs_write_policy(&execution);
		assert_eq!(decision.outcome, PolicyOutcome::Allow);
		assert_eq!(decision.reason_code, PolicyReasonCode::AllowedByPolicy);
	}

	#[test]
	fn fs_write_overwrite_execution_detected_via_argv() {
		let overwrite = CanonicalExecution {
			tool_name: "fs.write".to_string(),
			program: "fs.write".to_string(),
			argv: vec![
				"fs.write".to_string(),
				"/workspace/existing.txt".to_string(),
				"--overwrite".to_string(),
			],
			invocation_mode: InvocationMode::DirectExec,
			shell_context: None,
			cwd: "/workspace".to_string(),
			env_policy: ExecutionEnvPolicy {
				mode: ExecutionEnvPolicyMode::Clean,
				allowed_keys: Vec::new(),
			},
			resource_scope: ExecutionResourceScope {
				working_directory: "/workspace".to_string(),
				resolved_targets: vec!["/workspace/existing.txt".to_string()],
				effective_read_roots: Vec::new(),
				effective_write_roots: vec!["/workspace".to_string()],
			},
			action_class: ExecutionActionClass::Write,
			digest: CanonicalDigest("digest-fs-write-overwrite".to_string()),
		};

		assert!(is_overwrite_execution(&overwrite));

		let create = CanonicalExecution {
			tool_name: "fs.write".to_string(),
			program: "fs.write".to_string(),
			argv: vec!["fs.write".to_string(), "/workspace/new.txt".to_string()],
			invocation_mode: InvocationMode::DirectExec,
			shell_context: None,
			cwd: "/workspace".to_string(),
			env_policy: ExecutionEnvPolicy {
				mode: ExecutionEnvPolicyMode::Clean,
				allowed_keys: Vec::new(),
			},
			resource_scope: ExecutionResourceScope {
				working_directory: "/workspace".to_string(),
				resolved_targets: vec!["/workspace/new.txt".to_string()],
				effective_read_roots: Vec::new(),
				effective_write_roots: vec!["/workspace".to_string()],
			},
			action_class: ExecutionActionClass::Write,
			digest: CanonicalDigest("digest-fs-write-create".to_string()),
		};

		assert!(!is_overwrite_execution(&create));
	}
}
