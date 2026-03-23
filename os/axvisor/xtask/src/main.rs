// Copyright 2025 The Axvisor Team
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

//! Build tool for Axvisor hypervisor.
//!
//! This crate provides the `xtask` binary for building, running, and managing
//! the Axvisor hypervisor project.

#![cfg_attr(not(any(windows, unix)), no_main)]
#![cfg_attr(not(any(windows, unix)), no_std)]
#![cfg(any(windows, unix))]

use std::path::PathBuf;

use anyhow::{Context, Result};
use axbuild::axvisor::Cli;
use clap::Parser;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("failed to determine axvisor repo root")?
        .to_path_buf();
    axbuild::axvisor::run(cli.command, repo_root).await
}
