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

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::macros::format_description;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LogLevel {
	Trace,
	Debug,
	Info,
	Warn,
	Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogField {
	pub key: String,
	pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogRecord {
	pub timestamp_unix_ms: u64,
	pub component: String,
	pub level: LogLevel,
	pub message: String,
	#[serde(default)]
	pub fields: Vec<LogField>,
}

impl LogRecord {
	pub fn new(component: impl Into<String>, level: LogLevel, message: impl Into<String>) -> Self {
		Self {
			timestamp_unix_ms: now_unix_ms(),
			component: component.into(),
			level,
			message: message.into(),
			fields: Vec::new(),
		}
	}

	pub fn with_field(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
		self.fields.push(LogField {
			key: key.into(),
			value: value.into(),
		});
		self
	}
}

pub trait LogSink: Send + Sync {
	fn write(&self, record: LogRecord) -> std::io::Result<()>;
}

#[derive(Debug, Default)]
pub struct NoopLogSink;

impl LogSink for NoopLogSink {
	fn write(&self, _record: LogRecord) -> std::io::Result<()> {
		Ok(())
	}
}

#[derive(Debug, Default)]
pub struct StderrLogSink;

impl LogSink for StderrLogSink {
	fn write(&self, record: LogRecord) -> std::io::Result<()> {
		eprintln!("{}", format_stderr_record(&record));
		Ok(())
	}
}

pub struct FanoutLogSink {
	sinks: Vec<Arc<dyn LogSink>>,
}

impl FanoutLogSink {
	pub fn new(sinks: Vec<Arc<dyn LogSink>>) -> Self {
		Self { sinks }
	}
}

impl LogSink for FanoutLogSink {
	fn write(&self, record: LogRecord) -> std::io::Result<()> {
		let mut first_error = None;
		for sink in &self.sinks {
			if let Err(error) = sink.write(record.clone())
				&& first_error.is_none()
			{
				first_error = Some(error);
			}
		}

		if let Some(error) = first_error {
			return Err(error);
		}

		Ok(())
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileLogConfig {
	pub base_dir: PathBuf,
	pub max_file_bytes: u64,
	pub max_backup_files: usize,
}

impl Default for FileLogConfig {
	fn default() -> Self {
		Self {
			base_dir: PathBuf::from("logs"),
			max_file_bytes: 8 * 1024 * 1024,
			max_backup_files: 5,
		}
	}
}

pub struct AsyncRotatingFileLogSink {
	sender: Mutex<Sender<LogRecord>>,
}

impl AsyncRotatingFileLogSink {
	pub fn new(config: FileLogConfig) -> Self {
		let (sender, receiver) = mpsc::channel::<LogRecord>();
		thread::spawn(move || {
			let mut writers = HashMap::<String, ComponentWriter>::new();
			for record in receiver {
				let line = match serde_json::to_string(&record) {
					Ok(encoded) => encoded,
					Err(error) => {
						eprintln!(
							"[roku-observability] failed to encode log record: {}",
							error
						);
						continue;
					}
				};
				if let Err(error) = write_record(&config, &mut writers, &record.component, &line) {
					eprintln!(
						"[roku-observability] failed to persist log record for component {}: {}",
						record.component, error
					);
				}
			}
		});
		Self {
			sender: Mutex::new(sender),
		}
	}
}

impl LogSink for AsyncRotatingFileLogSink {
	fn write(&self, record: LogRecord) -> std::io::Result<()> {
		let sender = self
			.sender
			.lock()
			.map_err(|_| std::io::Error::other("log sender is poisoned"))?;
		sender
			.send(record)
			.map_err(|_| std::io::Error::other("log writer thread is unavailable"))
	}
}

struct ComponentWriter {
	writer: BufWriter<File>,
	current_bytes: u64,
}

fn write_record(
	config: &FileLogConfig,
	writers: &mut HashMap<String, ComponentWriter>,
	component: &str,
	line: &str,
) -> std::io::Result<()> {
	let required_bytes = u64::try_from(line.len() + 1).unwrap_or(u64::MAX);
	let needs_rotation = writers
		.get(component)
		.map(|state| state.current_bytes.saturating_add(required_bytes) > config.max_file_bytes)
		.unwrap_or(false);
	if needs_rotation {
		writers.insert(
			component.to_string(),
			open_component_writer(&config.base_dir, component)?,
		);
		prune_component_logs(&config.base_dir, component, config.max_backup_files)?;
	}
	if !writers.contains_key(component) {
		writers.insert(
			component.to_string(),
			open_component_writer(&config.base_dir, component)?,
		);
		prune_component_logs(&config.base_dir, component, config.max_backup_files)?;
	}

	let state = writers
		.get_mut(component)
		.expect("component writer should exist after initialization");
	state.writer.write_all(line.as_bytes())?;
	state.writer.write_all(b"\n")?;
	state.writer.flush()?;
	state.current_bytes = state.current_bytes.saturating_add(required_bytes);
	Ok(())
}

fn open_component_writer(base_dir: &Path, component: &str) -> std::io::Result<ComponentWriter> {
	let log_path = next_component_log_path(base_dir, component);
	if let Some(parent) = log_path.parent() {
		fs::create_dir_all(parent)?;
	}
	let file = OpenOptions::new()
		.create(true)
		.append(true)
		.open(&log_path)?;
	let current_bytes = file.metadata()?.len();
	Ok(ComponentWriter {
		writer: BufWriter::new(file),
		current_bytes,
	})
}

fn prune_component_logs(
	base_dir: &Path,
	component: &str,
	max_backup_files: usize,
) -> std::io::Result<()> {
	let component_dir = base_dir.join(component);
	fs::create_dir_all(&component_dir)?;
	let mut log_paths = component_log_paths(&component_dir)?;
	let max_total_files = max_backup_files.saturating_add(1).max(1);
	while log_paths.len() > max_total_files {
		if let Some(path) = log_paths.first().cloned() {
			let _ = fs::remove_file(path);
		}
		log_paths.remove(0);
	}
	Ok(())
}

fn component_log_paths(component_dir: &Path) -> std::io::Result<Vec<PathBuf>> {
	let mut paths = fs::read_dir(component_dir)?
		.filter_map(|entry| entry.ok().map(|entry| entry.path()))
		.filter(|path| path.extension().is_some_and(|extension| extension == "log"))
		.collect::<Vec<_>>();
	paths.sort();
	Ok(paths)
}

fn next_component_log_path(base_dir: &Path, component: &str) -> PathBuf {
	let component_dir = base_dir.join(component);
	let timestamp = timestamped_log_name();
	let candidate = component_dir.join(format!("{timestamp}.log"));
	if !candidate.exists() {
		return candidate;
	}

	let epoch_millis = now_unix_ms();
	component_dir.join(format!("{timestamp}-{epoch_millis}.log"))
}

fn timestamped_log_name() -> String {
	let format =
		format_description!("[year][month][day]T[hour][minute][second][subsecond digits:3]Z");
	OffsetDateTime::now_utc()
		.format(&format)
		.unwrap_or_else(|_| format!("epoch-{}", now_unix_ms()))
}

fn format_stderr_record(record: &LogRecord) -> String {
	let fields = if record.fields.is_empty() {
		String::new()
	} else {
		format!(
			" {}",
			record
				.fields
				.iter()
				.map(|field| format!("{}={}", field.key, field.value))
				.collect::<Vec<_>>()
				.join(" ")
		)
	};
	format!(
		"[{}] {} {}{}",
		level_label(record.level),
		record.component,
		record.message,
		fields
	)
}

fn level_label(level: LogLevel) -> &'static str {
	match level {
		LogLevel::Trace => "TRACE",
		LogLevel::Debug => "DEBUG",
		LogLevel::Info => "INFO",
		LogLevel::Warn => "WARN",
		LogLevel::Error => "ERROR",
	}
}

fn global_log_sink() -> &'static Mutex<Arc<dyn LogSink>> {
	static GLOBAL_LOG_SINK: OnceLock<Mutex<Arc<dyn LogSink>>> = OnceLock::new();
	GLOBAL_LOG_SINK.get_or_init(|| Mutex::new(Arc::new(NoopLogSink)))
}

pub fn install_global_log_sink(sink: Arc<dyn LogSink>) {
	let mut slot = global_log_sink()
		.lock()
		.expect("global log sink mutex should not be poisoned");
	*slot = sink;
}

pub fn emit_global_log(record: LogRecord) -> std::io::Result<()> {
	let sink = global_log_sink()
		.lock()
		.expect("global log sink mutex should not be poisoned")
		.clone();
	sink.write(record)
}

fn now_unix_ms() -> u64 {
	SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.unwrap_or_default()
		.as_millis()
		.try_into()
		.unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
	use super::*;

	fn unique_dir(suffix: &str) -> PathBuf {
		let nanos = SystemTime::now()
			.duration_since(UNIX_EPOCH)
			.expect("clock should be after epoch")
			.as_nanos();
		std::env::temp_dir().join(format!("roku-log-{suffix}-{nanos}"))
	}

	#[test]
	fn async_rotating_sink_writes_component_log_files() {
		let dir = unique_dir("write");
		let sink = AsyncRotatingFileLogSink::new(FileLogConfig {
			base_dir: dir.clone(),
			max_file_bytes: 1024,
			max_backup_files: 2,
		});
		sink.write(LogRecord::new("roku-cmd", LogLevel::Info, "started"))
			.expect("log write should succeed");
		thread::sleep(std::time::Duration::from_millis(50));

		let log_path = component_log_paths(&dir.join("roku-cmd"))
			.expect("log directory should be readable")
			.into_iter()
			.last()
			.expect("one log file should be present");
		let content = fs::read_to_string(&log_path).expect("log file should be readable");
		assert!(content.contains("\"component\":\"roku-cmd\""));
		assert!(content.contains("\"message\":\"started\""));
		assert!(
			log_path
				.file_name()
				.and_then(|value| value.to_str())
				.is_some_and(|value| value.contains('T'))
		);
		let _ = fs::remove_dir_all(dir);
	}

	#[test]
	fn async_rotating_sink_rotates_when_size_limit_is_exceeded() {
		let dir = unique_dir("rotate");
		let sink = AsyncRotatingFileLogSink::new(FileLogConfig {
			base_dir: dir.clone(),
			max_file_bytes: 120,
			max_backup_files: 2,
		});
		for index in 0..8 {
			sink.write(
				LogRecord::new(
					"roku-runtime-service",
					LogLevel::Info,
					format!("event-{index}"),
				)
				.with_field("payload", "x".repeat(32)),
			)
			.expect("log write should succeed");
		}
		thread::sleep(std::time::Duration::from_millis(100));

		let log_paths = component_log_paths(&dir.join("roku-runtime-service"))
			.expect("runtime log directory should be readable");
		assert!(log_paths.len() >= 2);
		assert!(log_paths.len() <= 3);
		assert!(log_paths.iter().all(|path| {
			path.file_name()
				.and_then(|value| value.to_str())
				.is_some_and(|value| value.ends_with(".log"))
		}));
		let _ = fs::remove_dir_all(dir);
	}
}
