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
use axbuild::arceos::{AxBuild, context::AxContext};
use clap::Parser;

/// Run command arguments
#[derive(Parser, Debug)]
pub struct RunArgs {
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

    /// Enable block device
    #[arg(long)]
    pub blk: bool,

    /// Disk image path
    #[arg(long)]
    pub disk_img: Option<String>,

    /// Enable network
    #[arg(long)]
    pub net: bool,

    /// Network device type (user, tap, bridge)
    #[arg(long)]
    pub net_dev: Option<String>,

    /// Enable graphic output
    #[arg(long)]
    pub graphic: bool,

    /// Enable hardware acceleration (KVM/HVF)
    #[arg(long)]
    pub accel: bool,
}

impl RunArgs {
    pub fn into_axbuild(self, manifest_dir: &std::path::Path) -> Result<AxBuild> {
        let Self {
            arch,
            package,
            platform,
            release,
            features,
            smp,
            blk,
            disk_img,
            net,
            net_dev,
            graphic,
            accel,
        } = self;

        let overrides = super::config::run_config_override(
            arch,
            package.clone(),
            platform,
            release,
            features,
            smp,
            blk,
            disk_img,
            net,
            net_dev,
            graphic,
            accel,
        )?;

        AxBuild::from_overrides(manifest_dir, overrides, Some(package), None)
    }
}

/// Run the build and run command
pub async fn run_with_context(ctx: AxContext) -> Result<()> {
    println!("Running in QEMU...");
    AxBuild::new(ctx).run_qemu().await
}

pub async fn run_with_arg(arg: RunArgs) -> Result<()> {
    let manifest_dir = super::config::arceos_manifest_dir()?;
    let axbuild = arg.into_axbuild(&manifest_dir)?;
    println!("Running in QEMU...");
    axbuild.run_qemu().await
}
