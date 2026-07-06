use crate::types::LogFormat;
use serde::Serialize;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::sync::{Arc, Mutex};

const LOG_BUFFER_SIZE: usize = 8192;

#[derive(Debug, Clone, Copy)]
pub enum LogLevel {
    Info,
    Warning,
    Error,
    Success,
}

impl LogLevel {
    fn as_str(self) -> &'static str {
        match self {
            LogLevel::Info => "INFO",
            LogLevel::Warning => "WARN",
            LogLevel::Error => "ERROR",
            LogLevel::Success => "SUCCESS",
        }
    }
}

/// Optional fields attached to a single log event for structured (JSON) output.
#[derive(Default)]
pub struct LogContext<'a> {
    pub schema: Option<&'a str>,
    pub table_name: Option<&'a str>,
    pub operation: Option<&'a str>,
    pub status: Option<&'a str>,
    pub duration_secs: Option<f64>,
    pub xid_age: Option<i64>,
    pub error: Option<&'a str>,
}

#[derive(Serialize)]
struct LogEvent<'a> {
    timestamp: &'a str,
    level: &'a str,
    message: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    schema: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    table_name: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    operation: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    duration_secs: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    xid_age: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'a str>,
}

pub struct Logger {
    log_file: String,
    silence_mode: bool,
    json_mode: bool,
    file_handle: Arc<Mutex<Option<BufWriter<File>>>>,
}

impl Logger {
    pub fn new(log_file: String, silence_mode: bool, log_format: LogFormat) -> Self {
        Self {
            log_file,
            silence_mode,
            json_mode: log_format == LogFormat::Json,
            file_handle: Arc::new(Mutex::new(None)),
        }
    }

    fn ensure_file_handle(&self) -> bool {
        let mut guard = match self.file_handle.lock() {
            Ok(g) => g,
            Err(p) => {
                eprintln!("Logger lock was poisoned, attempting recovery");
                p.into_inner()
            }
        };

        if guard.is_some() {
            return true;
        }

        match OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_file)
        {
            Ok(file) => {
                *guard = Some(BufWriter::with_capacity(LOG_BUFFER_SIZE, file));
                true
            }
            Err(e) => {
                eprintln!(
                    "Failed to open log file '{}': {}. File logging disabled.",
                    self.log_file, e
                );
                false
            }
        }
    }

    fn timestamp(&self) -> String {
        chrono::Utc::now()
            .format("%Y-%m-%d %H:%M:%S%.3f")
            .to_string()
    }

    pub fn log(&self, level: LogLevel, message: &str) {
        self.log_with_context(level, message, LogContext::default());
    }

    pub fn log_with_context(&self, level: LogLevel, message: &str, ctx: LogContext<'_>) {
        let ts = self.timestamp();
        let level_str = level.as_str();

        let line = if self.json_mode {
            let event = LogEvent {
                timestamp: &ts,
                level: level_str,
                message,
                schema: ctx.schema,
                table_name: ctx.table_name,
                operation: ctx.operation,
                status: ctx.status,
                duration_secs: ctx.duration_secs,
                xid_age: ctx.xid_age,
                error: ctx.error,
            };
            format!(
                "{}\n",
                serde_json::to_string(&event).unwrap_or_else(|_| format!(
                    "{{\"timestamp\":\"{}\",\"level\":\"{}\",\"message\":\"{}\"}}",
                    ts, level_str, message
                ))
            )
        } else {
            format!("[{}] [{}] {}\n", ts, level_str, message)
        };

        if !self.silence_mode {
            print!("{}", line);
        }

        if self.ensure_file_handle() {
            let mut guard = match self.file_handle.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            if let Some(ref mut writer) = *guard {
                if let Err(e) = writer.write_all(line.as_bytes()) {
                    eprintln!("Failed to write to log file '{}': {}", self.log_file, e);
                    *guard = None;
                } else if matches!(level, LogLevel::Error) {
                    let _ = writer.flush();
                }
            }
        }
    }

    /// Always prints to stdout regardless of silence mode (used for completion summaries).
    pub fn log_always(&self, level: LogLevel, message: &str) {
        let ts = self.timestamp();
        let level_str = level.as_str();
        let line = if self.json_mode {
            let event = LogEvent {
                timestamp: &ts,
                level: level_str,
                message,
                schema: None,
                table_name: None,
                operation: None,
                status: None,
                duration_secs: None,
                xid_age: None,
                error: None,
            };
            format!(
                "{}\n",
                serde_json::to_string(&event)
                    .unwrap_or_else(|_| format!("{{\"message\":\"{}\"}}", message))
            )
        } else {
            format!("[{}] [{}] {}\n", ts, level_str, message)
        };

        print!("{}", line);

        if self.ensure_file_handle() {
            let mut guard = match self.file_handle.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            if let Some(ref mut writer) = *guard {
                if let Err(e) = writer.write_all(line.as_bytes()) {
                    eprintln!("Failed to write to log file '{}': {}", self.log_file, e);
                    *guard = None;
                }
            }
        }
    }

    pub fn log_table_start(&self, num: usize, total: usize, schema: &str, table: &str, op: &str) {
        let msg = format!(
            "[{}/{}] Starting {} on \"{}\".\"{}\"",
            num, total, op, schema, table
        );
        self.log_with_context(
            LogLevel::Info,
            &msg,
            LogContext {
                schema: Some(schema),
                table_name: Some(table),
                operation: Some(op),
                status: Some("starting"),
                ..Default::default()
            },
        );
    }

    pub fn log_table_success(
        &self,
        schema: &str,
        table: &str,
        op: &str,
        duration: std::time::Duration,
    ) {
        let msg = format!(
            "Completed {} on \"{}\".\"{}\" in {:.2?}",
            op, schema, table, duration
        );
        self.log_with_context(
            LogLevel::Success,
            &msg,
            LogContext {
                schema: Some(schema),
                table_name: Some(table),
                operation: Some(op),
                status: Some("success"),
                duration_secs: Some(duration.as_secs_f64()),
                ..Default::default()
            },
        );
    }

    pub fn log_table_failed(&self, schema: &str, table: &str, op: &str, reason: &str) {
        let msg = format!(
            "Failed {} on \"{}\".\"{}\" — {}",
            op, schema, table, reason
        );
        self.log_with_context(
            LogLevel::Error,
            &msg,
            LogContext {
                schema: Some(schema),
                table_name: Some(table),
                operation: Some(op),
                status: Some("failed"),
                error: Some(reason),
                ..Default::default()
            },
        );
    }
}

impl Drop for Logger {
    fn drop(&mut self) {
        let mut guard = match self.file_handle.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        if let Some(mut writer) = guard.take() {
            if let Err(e) = writer.flush() {
                eprintln!("Failed to flush log file '{}' on shutdown: {}", self.log_file, e);
            }
        }
    }
}
