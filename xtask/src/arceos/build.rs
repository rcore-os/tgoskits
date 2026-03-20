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

use std::str::FromStr;

use anyhow::{Context, Result};
use axbuild::{
    Arch, BuildMode, FeatureResolver,
    arceos::{ArceosConfigOverride, AxBuild, RunScope},
};
use clap::Args;

/// Build command arguments
#[derive(Args, Debug)]
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

    /// Enable dynamic platform (plat-dyn)
    #[arg(long, action = clap::ArgAction::Set)]
    pub plat_dyn: Option<bool>,
}

impl BuildArgs {
    pub fn into_axbuild(self) -> Result<AxBuild> {
        let overrides = self.as_override()?;
        AxBuild::from_overrides(overrides, Some(self.package), None, RunScope::Default)
    }

    pub fn as_override(&self) -> Result<ArceosConfigOverride> {
        if matches!(self.smp, Some(0)) {
            bail!("invalid SMP value `0`: SMP must be >= 1");
        }
        let parsed_arch = self
            .arch
            .as_deref()
            .map(Arch::from_str)
            .transpose()
            .context("failed to parse arch override")?;
        let effective_plat_dyn = self.plat_dyn.unwrap_or(match parsed_arch {
            Some(Arch::AArch64) | None => true,
            Some(_) => false,
        });

        Ok(ArceosConfigOverride {
            arch: parsed_arch,
            platform: self.platform.clone(),
            mode: self.release.then_some(BuildMode::Release),
            plat_dyn: Some(effective_plat_dyn),
            smp: self.smp,
            features: self
                .features
                .as_deref()
                .map(FeatureResolver::parse_features)
                .map(Some)
                .unwrap_or(None),
            ..Default::default()
        })
    }
}

/// Run the build command
pub async fn run_build(args: BuildArgs) -> Result<()> {
    let axbuild = args.into_axbuild()?;

    println!("Building ArceOS application:");
    let output = axbuild.build().await?;

    println!();
    println!("Build successful!");
    println!("  ELF: {}", output.elf.display());
    println!("  Binary: {}", output.bin.display());

    Ok(())
}
