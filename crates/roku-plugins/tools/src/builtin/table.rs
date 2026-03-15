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
use std::fs;
use std::path::{Path, PathBuf};

use crate::contract::{
	contract_input_schema, contract_tool_schema, input_contract, input_field, output_contract,
	runtime_contract, selection_contract,
};
use crate::runtime_config::{HARD_MAX_PREVIEW_ROWS, TableToolRuntimeConfig};
use calamine::{Reader, open_workbook_auto};
use csv::ReaderBuilder;
use roku_common_types::{ToolContract, ToolOutputEnvelope, ToolRetryPolicy, ToolSideEffectPolicy};
use roku_plugin_catalog::{CatalogDescriptor, ResourceCost, ResourceKind, ResourceRisk};
use roku_plugin_host::{
	RuntimeConstraints, SandboxProfile, Tool, ToolDescriptor, ToolFailure, ToolInvocationRequest,
	ToolRuntime, ToolRuntimeError,
};
use serde_json::{Value, json};

/// Returns catalog metadata for all table builtin tools (table.inspect, table.list_sheets, table.preview, table.schema).
///
/// Used when the core-table plugin is enabled: [`build_resource_catalog_with_plugin_snapshot_and_runtime_capabilities`]
/// in `builders` extends its tool entries with this list, then builds a [`ResourceCatalog`]. That catalog is
/// used by the router/classifier for: retrieval over compact selection text (BM25 + embedding), building the
/// route classifier's compact selection inventory, resolving a chosen tool name to a [`ResourceSelector`], and
/// risk/cost for routing decisions. Tool names here must match the tools registered for execution via
/// [`register_tools`] in this module.
#[allow(dead_code)]
pub(crate) fn catalog_descriptors() -> Vec<CatalogDescriptor> {
	catalog_descriptors_with_config(&TableToolRuntimeConfig::default())
}

pub(crate) fn catalog_descriptors_with_config(
	_runtime_config: &TableToolRuntimeConfig,
) -> Vec<CatalogDescriptor> {
	vec![
		descriptor_catalog(
			"table.inspect",
			"Use this first when you have a concrete table file and need high-level facts such as format, size, sheet count, or rough structure. Do not use it when the user specifically asked for row samples or column types; `table.preview` and `table.schema` are more precise. It returns bounded metadata for choosing the next table step.",
			"Inspect a known table file for format and high-level structure.",
			&["table", "inspect", "xlsx", "csv", "tsv"],
			&["Inspect tmp/test-excel.xlsx."],
			&["path"],
			&["table.inspect"],
		),
		descriptor_catalog(
			"table.list_sheets",
			"Use this only when the main question is which sheet names exist in a known XLSX workbook. Do not use it for CSV/TSV preview or schema inspection. It returns workbook sheet names, or explains that flat files do not expose named sheets.",
			"List sheet names in a known XLSX workbook.",
			&["table", "sheet", "xlsx"],
			&["List the sheets in tmp/test-excel.xlsx."],
			&["path"],
			&["table.list_sheets"],
		),
		descriptor_catalog(
			"table.preview",
			"Use this when the user wants actual sample rows from a known table or sheet. Do not use it just to learn column names or inferred types; `table.schema` is better for that. It returns a bounded row preview suitable for direct display or downstream summarization.",
			"Preview sample rows from a known table or sheet.",
			&["table", "preview", "rows", "xlsx", "csv"],
			&["Preview the first few rows of tmp/test-excel.xlsx."],
			&["path", "sheet", "rows"],
			&["table.preview"],
		),
		descriptor_catalog(
			"table.schema",
			"Use this when the user wants column names and inferred types from a known table or sheet. Do not use it for row samples or sheet enumeration. It returns structural schema facts that are better for reasoning about the data than `table.inspect` or `table.preview`.",
			"Show column names and inferred types for a known table or sheet.",
			&["table", "schema", "columns", "xlsx", "csv"],
			&["Show the schema of tmp/test-excel.xlsx."],
			&["path", "sheet"],
			&["table.schema"],
		),
	]
}

#[allow(dead_code)]
pub(crate) fn register_tools(runtime: &mut ToolRuntime) -> Result<(), ToolRuntimeError> {
	register_tools_with_config(runtime, &TableToolRuntimeConfig::default())
}

pub(crate) fn register_tools_with_config(
	runtime: &mut ToolRuntime,
	config: &TableToolRuntimeConfig,
) -> Result<(), ToolRuntimeError> {
	runtime.register_tool(TableInspectTool {
		config: config.clone(),
	})?;
	runtime.register_tool(TableListSheetsTool {
		config: config.clone(),
	})?;
	runtime.register_tool(TablePreviewTool {
		config: config.clone(),
	})?;
	runtime.register_tool(TableSchemaTool {
		config: config.clone(),
	})?;
	Ok(())
}

#[derive(Clone)]
struct TableInspectTool {
	config: TableToolRuntimeConfig,
}

#[derive(Clone)]
struct TableListSheetsTool {
	config: TableToolRuntimeConfig,
}

#[derive(Clone)]
struct TablePreviewTool {
	config: TableToolRuntimeConfig,
}

#[derive(Clone)]
struct TableSchemaTool {
	config: TableToolRuntimeConfig,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TableKind {
	Csv,
	Tsv,
	Xlsx,
}

impl TableKind {
	fn label(self) -> &'static str {
		match self {
			Self::Csv => "csv",
			Self::Tsv => "tsv",
			Self::Xlsx => "xlsx",
		}
	}
}

impl Tool for TableInspectTool {
	fn descriptor(&self) -> ToolDescriptor {
		tool_descriptor("table.inspect", &["path"], &["table.inspect"])
	}

	fn invoke(&self, request: ToolInvocationRequest) -> Result<Value, ToolFailure> {
		let _ = &self.config;
		let path = required_string(&request.input, "path")?;
		let roots = allowed_read_roots(&request)?;
		let resolved = resolve_existing_path(path, &roots)?;
		let kind = table_kind(&resolved)?;
		let metadata = fs::metadata(&resolved).map_err(|error| {
			ToolFailure::terminal(format!(
				"failed to inspect `{}`: {error}",
				resolved.display()
			))
		})?;
		let detail = inspect_table(&resolved, kind, self.config.default_preview_rows)?;
		let message = if detail.sheet_names.is_empty() {
			format!(
				"`{}` is a {} table with {} column(s).",
				resolved.display(),
				kind.label(),
				detail.columns
			)
		} else {
			format!(
				"`{}` is a {} workbook with {} sheet(s): {}.",
				resolved.display(),
				kind.label(),
				detail.sheet_names.len(),
				detail.sheet_names.join(", ")
			)
		};
		Ok(observation_like_output(
			message,
			true,
			None,
			false,
			json!({
				"path": resolved.display().to_string(),
				"format": kind.label(),
				"size": metadata.len(),
				"sheet_count": detail.sheet_names.len(),
				"sheets": detail.sheet_names,
				"columns": detail.columns,
			}),
		))
	}
}

impl Tool for TableListSheetsTool {
	fn descriptor(&self) -> ToolDescriptor {
		tool_descriptor("table.list_sheets", &["path"], &["table.list_sheets"])
	}

	fn invoke(&self, request: ToolInvocationRequest) -> Result<Value, ToolFailure> {
		let _ = &self.config;
		let path = required_string(&request.input, "path")?;
		let roots = allowed_read_roots(&request)?;
		let resolved = resolve_existing_path(path, &roots)?;
		let kind = table_kind(&resolved)?;
		let sheets = match kind {
			TableKind::Xlsx => {
				inspect_table(&resolved, kind, self.config.default_preview_rows)?.sheet_names
			}
			TableKind::Csv | TableKind::Tsv => Vec::new(),
		};
		let message = if sheets.is_empty() {
			format!(
				"`{}` is a flat table and does not expose named sheets.",
				resolved.display()
			)
		} else {
			format!(
				"Sheets in `{}`:\n{}",
				resolved.display(),
				render_bullet_lines(&sheets)
			)
		};
		Ok(observation_like_output(
			message,
			true,
			None,
			false,
			json!({
				"path": resolved.display().to_string(),
				"sheets": sheets,
			}),
		))
	}
}

impl Tool for TablePreviewTool {
	fn descriptor(&self) -> ToolDescriptor {
		tool_descriptor("table.preview", &["path"], &["table.preview"])
	}

	fn invoke(&self, request: ToolInvocationRequest) -> Result<Value, ToolFailure> {
		let path = required_string(&request.input, "path")?;
		let rows = request
			.input
			.get("rows")
			.and_then(Value::as_u64)
			.and_then(|value| usize::try_from(value).ok())
			.unwrap_or(self.config.default_preview_rows)
			.clamp(1, HARD_MAX_PREVIEW_ROWS);
		let sheet = request
			.input
			.get("sheet")
			.and_then(Value::as_str)
			.filter(|value| !value.trim().is_empty());
		let roots = allowed_read_roots(&request)?;
		let resolved = resolve_existing_path(path, &roots)?;
		let kind = table_kind(&resolved)?;
		let preview = preview_table(&resolved, kind, sheet, rows)?;
		let message = render_preview_message(&resolved, &preview);
		Ok(observation_like_output(
			message,
			true,
			None,
			false,
			json!({
				"path": resolved.display().to_string(),
				"format": kind.label(),
				"sheet": preview.sheet,
				"headers": preview.headers,
				"rows": preview.rows,
				"truncated": preview.truncated,
			}),
		))
	}
}

impl Tool for TableSchemaTool {
	fn descriptor(&self) -> ToolDescriptor {
		tool_descriptor("table.schema", &["path"], &["table.schema"])
	}

	fn invoke(&self, request: ToolInvocationRequest) -> Result<Value, ToolFailure> {
		let _ = &self.config;
		let path = required_string(&request.input, "path")?;
		let sheet = request
			.input
			.get("sheet")
			.and_then(Value::as_str)
			.filter(|value| !value.trim().is_empty());
		let roots = allowed_read_roots(&request)?;
		let resolved = resolve_existing_path(path, &roots)?;
		let kind = table_kind(&resolved)?;
		let preview = preview_table(&resolved, kind, sheet, 20)?;
		let columns = infer_schema(&preview.headers, &preview.rows);
		let message = render_schema_message(&resolved, &columns);
		Ok(observation_like_output(
			message,
			true,
			None,
			false,
			json!({
				"path": resolved.display().to_string(),
				"format": kind.label(),
				"sheet": preview.sheet,
				"columns": columns,
			}),
		))
	}
}

struct TableInspectSummary {
	sheet_names: Vec<String>,
	columns: usize,
}

struct TablePreview {
	sheet: Option<String>,
	headers: Vec<String>,
	rows: Vec<Vec<String>>,
	truncated: bool,
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

fn table_tool_contract(name: &str) -> Option<ToolContract> {
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
		"table.preview" => Some(ToolContract {
			selection: selection_contract(
				&[
					"Use when the table path is already grounded and the user needs sample rows from a table or one sheet.",
				],
				&[
					"Do not use when the user only wants column names or inferred types.",
					"Do not use for sheet enumeration without row samples.",
				],
				&[
					"Commonly confused with table.schema for structure-only questions.",
					"Commonly confused with table.list_sheets when the user only wants workbook tabs.",
				],
			),
			input: input_contract(
				vec![
					input_field(
						"path",
						true,
						"The grounded table file path under the allowed read roots.",
						&["Reject when the path does not resolve to a supported table file."],
					),
					input_field(
						"sheet",
						false,
						"Optional sheet name for XLSX workbooks.",
						&["Reject when the sheet name does not exist in the workbook."],
					),
					input_field(
						"rows",
						false,
						"Optional preview row count clamped by the runtime preview ceiling.",
						&["Reject when zero or outside the allowed preview range."],
					),
				],
				&[
					"Preview reads stay inside allowed read roots and remain bounded by a row limit.",
				],
			),
			output: output_contract(
				"Returns grounded preview rows, headers, selected sheet, and truncation metadata for the requested table.",
				"An empty table still returns ok=true with headers and zero preview rows.",
				&[
					"Unsupported formats, missing sheets, and missing paths surface as explicit failed observations.",
					"Successful previews are non-terminal observations that can feed later synthesis.",
				],
				true,
				false,
			),
			runtime,
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
) -> CatalogDescriptor {
	let contract = table_tool_contract(name);
	let fallback_input_schema = input_schema
		.iter()
		.map(|value| (*value).to_string())
		.collect::<Vec<_>>();
	CatalogDescriptor {
		selector: roku_common_types::ResourceSelector::tool(name),
		kind: ResourceKind::Tool,
		name: name.to_string(),
		role: Some("core_table".to_string()),
		description: description.to_string(),
		selection_hint: selection_hint.to_string(),
		discoverable: true,
		tags: tags.iter().map(|value| (*value).to_string()).collect(),
		examples: examples.iter().map(|value| (*value).to_string()).collect(),
		input_schema: contract_input_schema(contract.as_ref(), &fallback_input_schema),
		risk: ResourceRisk::Low,
		cost: ResourceCost {
			estimated_tokens: 0,
			estimated_latency_ms: 1_500,
		},
		required_capabilities: required_capabilities
			.iter()
			.map(|value| (*value).to_string())
			.collect(),
		summary: selection_hint.to_string(),
		key_commands: Vec::new(),
		use_cases: Vec::new(),
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
	let contract = table_tool_contract(name);
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
			"no allowed read roots are configured for table tools",
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
	let canonical = candidate
		.canonicalize()
		.map_err(|error| ToolFailure::terminal(format!("failed to resolve `{raw}`: {error}")))?;
	if roots.iter().any(|root| canonical.starts_with(root)) {
		Ok(canonical)
	} else {
		Err(ToolFailure::terminal(format!(
			"path `{}` is outside the allowed read roots",
			canonical.display()
		)))
	}
}

fn table_kind(path: &Path) -> Result<TableKind, ToolFailure> {
	match path
		.extension()
		.and_then(|value| value.to_str())
		.unwrap_or_default()
		.to_ascii_lowercase()
		.as_str()
	{
		"csv" => Ok(TableKind::Csv),
		"tsv" => Ok(TableKind::Tsv),
		"xlsx" => Ok(TableKind::Xlsx),
		other => Err(ToolFailure::terminal(format!(
			"unsupported table format `{other}` for `{}`",
			path.display()
		))),
	}
}

fn inspect_table(
	path: &Path,
	kind: TableKind,
	default_preview_rows: usize,
) -> Result<TableInspectSummary, ToolFailure> {
	match kind {
		TableKind::Csv | TableKind::Tsv => {
			let preview = preview_delimited(path, kind, default_preview_rows)?;
			Ok(TableInspectSummary {
				sheet_names: Vec::new(),
				columns: preview.headers.len(),
			})
		}
		TableKind::Xlsx => {
			let mut workbook = open_workbook_auto(path).map_err(|error| {
				ToolFailure::terminal(format!(
					"failed to open workbook `{}`: {error}",
					path.display()
				))
			})?;
			let sheet_names = workbook.sheet_names().to_vec();
			let columns = sheet_names
				.first()
				.and_then(|sheet_name| workbook.worksheet_range(sheet_name).ok())
				.map(|range| range.width())
				.unwrap_or_default();
			Ok(TableInspectSummary {
				sheet_names,
				columns,
			})
		}
	}
}

fn preview_table(
	path: &Path,
	kind: TableKind,
	sheet: Option<&str>,
	rows: usize,
) -> Result<TablePreview, ToolFailure> {
	match kind {
		TableKind::Csv | TableKind::Tsv => preview_delimited(path, kind, rows),
		TableKind::Xlsx => preview_xlsx(path, sheet, rows),
	}
}

fn preview_delimited(
	path: &Path,
	kind: TableKind,
	rows: usize,
) -> Result<TablePreview, ToolFailure> {
	let delimiter = if kind == TableKind::Tsv { b'\t' } else { b',' };
	let mut reader = ReaderBuilder::new()
		.delimiter(delimiter)
		.from_path(path)
		.map_err(|error| {
			ToolFailure::terminal(format!("failed to read `{}`: {error}", path.display()))
		})?;
	let headers = reader
		.headers()
		.map_err(|error| {
			ToolFailure::terminal(format!(
				"failed to read headers from `{}`: {error}",
				path.display()
			))
		})?
		.iter()
		.map(str::to_string)
		.collect::<Vec<_>>();
	let mut records = Vec::new();
	let mut iter = reader.records();
	for _ in 0..rows {
		let Some(record) = iter.next() else {
			return Ok(TablePreview {
				sheet: None,
				headers,
				rows: records,
				truncated: false,
			});
		};
		let record = record.map_err(|error| {
			ToolFailure::terminal(format!(
				"failed to parse row from `{}`: {error}",
				path.display()
			))
		})?;
		records.push(record.iter().map(str::to_string).collect::<Vec<_>>());
	}
	let truncated = iter.next().is_some();
	Ok(TablePreview {
		sheet: None,
		headers,
		rows: records,
		truncated,
	})
}

fn preview_xlsx(
	path: &Path,
	sheet: Option<&str>,
	rows: usize,
) -> Result<TablePreview, ToolFailure> {
	let mut workbook = open_workbook_auto(path).map_err(|error| {
		ToolFailure::terminal(format!(
			"failed to open workbook `{}`: {error}",
			path.display()
		))
	})?;
	let sheet_name = sheet
		.map(str::to_string)
		.or_else(|| workbook.sheet_names().first().cloned())
		.ok_or_else(|| {
			ToolFailure::terminal(format!("workbook `{}` has no sheets", path.display()))
		})?;
	let range = workbook.worksheet_range(&sheet_name).map_err(|error| {
		ToolFailure::terminal(format!("failed to load sheet `{sheet_name}`: {error}"))
	})?;
	let mut iter = range.rows();
	let headers = iter
		.next()
		.map(|row| row.iter().map(cell_to_string).collect::<Vec<_>>())
		.unwrap_or_default();
	let mut preview_rows = Vec::new();
	for row in iter.by_ref().take(rows) {
		preview_rows.push(row.iter().map(cell_to_string).collect::<Vec<_>>());
	}
	let truncated = iter.next().is_some();
	Ok(TablePreview {
		sheet: Some(sheet_name),
		headers,
		rows: preview_rows,
		truncated,
	})
}

fn cell_to_string(cell: &impl ToString) -> String {
	cell.to_string()
}

fn infer_schema(headers: &[String], rows: &[Vec<String>]) -> Vec<Value> {
	headers
		.iter()
		.enumerate()
		.map(|(index, header)| {
			let values = rows
				.iter()
				.filter_map(|row| row.get(index))
				.map(|value| value.trim().to_string())
				.collect::<Vec<_>>();
			json!({
				"name": header,
				"index": index,
				"inferred_type": infer_type(&values),
			})
		})
		.collect()
}

fn infer_type(values: &[String]) -> &'static str {
	let non_empty = values
		.iter()
		.filter(|value| !value.is_empty())
		.collect::<Vec<_>>();
	if non_empty.is_empty() {
		return "empty";
	}
	if non_empty.iter().all(|value| value.parse::<i64>().is_ok()) {
		return "integer";
	}
	if non_empty.iter().all(|value| value.parse::<f64>().is_ok()) {
		return "number";
	}
	if non_empty.iter().all(|value| {
		matches!(
			value.to_ascii_lowercase().as_str(),
			"true" | "false" | "yes" | "no"
		)
	}) {
		return "boolean";
	}
	"string"
}

fn render_bullet_lines(values: &[String]) -> String {
	values
		.iter()
		.map(|value| format!("- {value}"))
		.collect::<Vec<_>>()
		.join("\n")
}

fn render_preview_message(path: &Path, preview: &TablePreview) -> String {
	let mut lines = Vec::new();
	lines.push(format!("Preview for `{}`:", path.display()));
	if !preview.headers.is_empty() {
		lines.push(format!("| {} |", preview.headers.join(" | ")));
	}
	for row in &preview.rows {
		lines.push(format!("| {} |", row.join(" | ")));
	}
	if preview.rows.is_empty() {
		lines.push("(no data rows)".to_string());
	}
	if preview.truncated {
		lines.push("... truncated preview".to_string());
	}
	lines.join("\n")
}

fn render_schema_message(path: &Path, columns: &[Value]) -> String {
	let mut lines = Vec::new();
	lines.push(format!("Schema for `{}`:", path.display()));
	lines.extend(columns.iter().filter_map(|column| {
		let name = column.get("name").and_then(Value::as_str)?;
		let kind = column.get("inferred_type").and_then(Value::as_str)?;
		Some(format!("- {name}: {kind}"))
	}));
	lines.join("\n")
}
