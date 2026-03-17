// Copyright 2025 The tgoskits Team
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

use anyhow::Result;
use axbuild::arceos::{AxBuild, config_path};
use clap::Parser;

/// Build command arguments
#[derive(Parser, Debug)]
pub struct BuildArgs {
    /// Target architecture (x86_64, aarch64, riscv64, loongarch64)
    #[arg(long)]
    pub arch: Option<String>,

    /// Workspace package name (for example: arceos-helloworld)
    #[arg(short = 'p', long = "package")]
    pub package: String,

    /// Platform name
    #[arg(long)]
    pub platform: Option<String>,

    /// Build in release mode
    #[arg(long)]
    pub release: bool,

    /// Comma-separated feature list
    #[arg(long)]
    pub features: Option<String>,

    /// Number of CPUs (must be >= 1)
    #[arg(long)]
    pub smp: Option<usize>,
}

impl BuildArgs {
    pub fn into_axbuild(self, manifest_dir: &std::path::Path) -> Result<AxBuild> {
        let Self {
            arch,
            package,
            platform,
            release,
            features,
            smp,
        } = self;

        let overrides = super::config::build_config_override(
            arch,
            package.clone(),
            platform,
            release,
            features,
            smp,
        )?;
        AxBuild::from_overrides(manifest_dir, overrides, Some(package), None)
    }
}

/// Run the build command
pub async fn run_build(args: BuildArgs) -> Result<()> {
    let manifest_dir = super::config::arceos_manifest_dir()?;
    let axbuild = args.into_axbuild(&manifest_dir)?;

    println!("Building ArceOS application:");
    println!("  Config: {}", config_path(&manifest_dir).display());

    let output = axbuild.build().await?;

    println!();
    println!("Build successful!");
    println!("  ELF: {}", output.elf.display());
    println!("  Binary: {}", output.bin.display());

    Ok(())
}
