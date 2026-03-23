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

use std::{fs, fs::File, path::Path};

use anyhow::{Context, Result};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

pub fn init_tracing() -> Result<WorkerGuard> {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("failed to locate workspace root")?;
    let log_path = workspace_root.join("target/xtask.ans");
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let logfile = File::create(&log_path)
        .with_context(|| format!("failed to create {}", log_path.display()))?;
    let (file_writer, file_guard) = tracing_appender::non_blocking(logfile);

    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let console_layer = fmt::layer()
        .compact()
        .with_target(false)
        .with_line_number(false)
        .with_file(false)
        .without_time();
    let file_layer = fmt::layer()
        .with_writer(file_writer)
        .with_ansi(true)
        .with_target(true)
        .with_line_number(true)
        .with_file(true);

    tracing_subscriber::registry()
        .with(env_filter)
        .with(console_layer)
        .with(file_layer)
        .try_init()
        .context("failed to initialize tracing subscriber")?;

    tracing::info!("xtask logging initialized: {}", log_path.display());
    Ok(file_guard)
}
