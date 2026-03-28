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

#[cfg(not(any(windows, unix)))]
mod lang;

#[cfg(any(windows, unix))]
#[derive(clap::Parser)]
struct Cli {
    #[command(subcommand)]
    command: axbuild::axvisor::Command,
}

#[cfg(any(windows, unix))]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    use clap::Parser;

    let cli = Cli::parse();
    axbuild::axvisor::Axvisor::new()?
        .execute(cli.command)
        .await?;
    Ok(())
}
