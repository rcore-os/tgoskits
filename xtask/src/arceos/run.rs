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
use axbuild::arceos::{ArceosConfig, QemuRunner, config_path};
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
    pub fn into_config(self, workspace_root: &std::path::Path) -> Result<ArceosConfig> {
        let Self {
            arch,
            package,
            platform,
            release,
            features,
            blk,
            disk_img,
            net,
            net_dev,
            graphic,
            accel,
        } = self;

        let overrides = super::config::run_config_override(
            workspace_root,
            arch,
            package,
            platform,
            release,
            features,
            blk,
            disk_img,
            net,
            net_dev,
            graphic,
            accel,
        )?;

        axbuild::arceos::load_config(workspace_root, overrides)
    }
}

/// Run the build and run command
pub async fn run_run(args: RunArgs) -> Result<()> {
    let manifest_dir = super::config::arceos_manifest_dir()?;
    let config = args.into_config(&manifest_dir)?;
    run_with_config(manifest_dir, config).await
}

pub async fn run_with_config(manifest_dir: std::path::PathBuf, config: ArceosConfig) -> Result<()> {
    println!("Building ArceOS application:");
    println!("  Architecture: {}", config.arch);
    println!("  Platform: {}", config.platform);
    println!("  App: {}", config.app.display());
    println!("  Config: {}", config_path(&manifest_dir).display());
    println!(
        "  Mode: {}",
        axbuild::arceos::config::BuildMode::to_string(config.mode)
    );
    println!();

    println!("Running in QEMU...");
    let runner = QemuRunner::new(config, manifest_dir);
    println!("  QEMU config: {}", runner.qemu_config_path().display());
    runner.run().await?;

    Ok(())
}
