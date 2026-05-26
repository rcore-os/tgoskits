//! Custom file logger for ostool.
//!
//! This module provides a file-based logger that writes all log output to
//! `{workspace_root}/target/ostool.ans`, keeping the terminal clean and
//! avoiding conflicts with the ratatui-based TUI.

use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
};

use anyhow::Context as _;
use chrono::Local;
use log::{Level, LevelFilter, Log, Metadata, Record};

/// File logger that writes log records to a file.
pub struct FileLogger {
    file: Mutex<File>,
}

impl FileLogger {
    /// Creates a new file logger that writes to the specified path.
    fn new(log_path: &Path) -> std::io::Result<Self> {
        if let Some(parent) = log_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)?;

        Ok(Self {
            file: Mutex::new(file),
        })
    }
}

impl Log for FileLogger {
    fn enabled(&self, _metadata: &Metadata) -> bool {
        true
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S");
            let level_str = match record.level() {
                Level::Error => "ERROR",
                Level::Warn => "WARN ",
                Level::Info => "INFO ",
                Level::Debug => "DEBUG",
                Level::Trace => "TRACE",
            };

            let message = format!("[{} {}] {}\n", level_str, timestamp, record.args());

            if let Ok(mut file) = self.file.lock() {
                let _ = file.write_all(message.as_bytes());
            }
        }
    }

    fn flush(&self) {
        if let Ok(mut file) = self.file.lock() {
            let _ = file.flush();
        }
    }
}

/// Returns the canonical log file path for a workspace root.
pub fn log_file_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join("target").join("ostool.ans")
}

/// Initializes the file logger to write logs to `{workspace_root}/target/ostool.ans`.
pub fn init_file_logger(workspace_root: &Path) -> anyhow::Result<PathBuf> {
    let log_path = log_file_path(workspace_root);
    let logger = FileLogger::new(&log_path)
        .with_context(|| format!("failed to create log file: {}", log_path.display()))?;

    log::set_boxed_logger(Box::new(logger)).context("failed to install ostool file logger")?;
    log::set_max_level(LevelFilter::Debug);

    Ok(log_path)
}
