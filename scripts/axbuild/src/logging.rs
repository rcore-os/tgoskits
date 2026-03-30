// Copyright 2026 The tgoskits Team
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

use std::{
    fs::{self, OpenOptions},
    path::Path,
    sync::{Mutex, OnceLock},
};

use anyhow::{Context, Result};
use tracing_log::LogTracer;
use tracing_subscriber::{fmt, prelude::*, util::SubscriberInitExt};

static LOG_FILE: OnceLock<Mutex<std::fs::File>> = OnceLock::new();

pub(crate) fn init_logging(workspace_root: &Path) -> Result<()> {
    let log_dir = workspace_root.join("target");
    fs::create_dir_all(&log_dir)
        .with_context(|| format!("failed to create log directory {}", log_dir.display()))?;
    let log_path = log_dir.join("xtask.ans");
    let file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&log_path)
        .with_context(|| format!("failed to open log file {}", log_path.display()))?;

    let writer = LOG_FILE.get_or_init(|| Mutex::new(file));

    let _ = LogTracer::init();
    let _ = tracing_subscriber::registry()
        .with(
            fmt::layer()
                .with_ansi(false)
                .with_writer(move || LogWriter { file: writer })
                .with_target(false),
        )
        .try_init();

    Ok(())
}

#[derive(Clone, Copy)]
struct LogWriter<'a> {
    file: &'a Mutex<std::fs::File>,
}

impl std::io::Write for LogWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.file
            .lock()
            .expect("log file mutex poisoned")
            .write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.lock().expect("log file mutex poisoned").flush()
    }
}
