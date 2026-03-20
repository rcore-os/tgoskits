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

use std::str::FromStr;

use anyhow::{Context, Result, bail};
use axbuild::{
    Arch, BuildMode, FeatureResolver, PlatformResolver,
    arceos::{ArceosConfigOverride, RunScope},
};
use clap::Args;

pub const STARRY_PACKAGE: &str = "starryos";
pub const STARRY_TEST_PACKAGE: &str = "starryos-test";

/// Build command arguments
#[derive(Args, Debug)]
pub struct BuildArgs {
    /// Target architecture (x86_64, aarch64, riscv64, loongarch64)
    #[arg(long)]
    pub arch: Option<String>,

    /// Workspace package name
    #[arg(short = 'p', long = "package", default_value = STARRY_PACKAGE)]
    pub package: String,

    /// Platform name
    #[arg(long)]
    pub platform: Option<String>,

    /// Build in release mode (default: true)
    #[arg(long, default_value_t = true)]
    pub release: bool,

    /// Comma-separated feature list
    #[arg(long)]
    pub features: Option<String>,

    /// Number of CPUs (must be >= 1)
    #[arg(long)]
    pub smp: Option<usize>,

    /// Enable dynamic platform (plat-dyn)
    #[arg(long, action = clap::ArgAction::Set, default_value_t = false)]
    pub plat_dyn: bool,
}

impl BuildArgs {
    pub fn into_config_override(self) -> Result<ArceosConfigOverride> {
        let arch = parse_starry_arch(self.arch.as_deref())?;
        if matches!(self.smp, Some(0)) {
            bail!("invalid SMP value `0`: SMP must be >= 1");
        }

        Ok(ArceosConfigOverride {
            arch: Some(arch),
            platform: self
                .platform
                .or_else(|| Some(PlatformResolver::resolve_default_platform_name(&arch))),
            mode: self.release.then_some(BuildMode::Release),
            plat_dyn: Some(self.plat_dyn),
            smp: self.smp,
            features: self
                .features
                .as_deref()
                .map(FeatureResolver::parse_features)
                .map(Some)
                .unwrap_or(None),
            app_features: Some(vec!["qemu".to_string()]),
            ..Default::default()
        })
    }
}

/// Run the build command
pub async fn run_build(args: BuildArgs) -> Result<()> {
    let package = args.package.clone();
    let overrides = args.into_config_override()?;
    let axbuild = axbuild::arceos::AxBuild::from_overrides(
        overrides,
        Some(package),
        None,
        RunScope::Default,
    )?;

    println!("Building StarryOS application:");
    let output = axbuild.build().await?;
    println!();
    println!("Build successful!");
    println!("  ELF: {}", output.elf.display());
    println!("  Binary: {}", output.bin.display());
    Ok(())
}

fn parse_starry_arch(arch: Option<&str>) -> Result<Arch> {
    match arch {
        Some(value) => Arch::from_str(value).context("failed to parse arch override"),
        None => Ok(Arch::RiscV64),
    }
}
