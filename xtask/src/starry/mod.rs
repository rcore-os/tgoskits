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

//! StarryOS build commands for xtask

use anyhow::{Context, Result};
use clap::Subcommand;

pub mod build;
pub mod config;
pub mod run;

pub use build::{BuildArgs, STARRY_PACKAGE, run_build};
pub use run::RunArgs;

use crate::starry::{
    config::{
        ensure_rootfs_in_target_dir, parse_starry_arch, parse_starry_target_for_test,
        starry_default_disk_image,
    },
    run::run_with_arg,
};

/// StarryOS subcommands
#[derive(Subcommand, Debug)]
pub enum StarryCommand {
    /// Build StarryOS application
    Build {
        #[command(flatten)]
        args: BuildArgs,
    },
    /// Build and run StarryOS application
    Run {
        #[command(flatten)]
        args: RunArgs,
    },
    /// Download rootfs image and place it under target artifact directory
    Rootfs {
        /// Target architecture (default: riscv64)
        #[arg(long)]
        arch: Option<String>,
    },
    /// Deprecated alias for `rootfs`
    Img {
        /// Target architecture (default: riscv64)
        #[arg(long)]
        arch: Option<String>,
    },
}

impl StarryCommand {
    pub async fn run(self) -> Result<()> {
        match self {
            StarryCommand::Build { args } => run_build(args).await,
            StarryCommand::Run { args } => run_with_arg(args).await,
            StarryCommand::Rootfs { arch } => run_rootfs_command(arch),
            StarryCommand::Img { arch } => run_img_command(arch),
        }
    }
}

fn run_rootfs_command(arch: Option<String>) -> Result<()> {
    let arch = parse_starry_arch(arch.as_deref())?;
    let disk_img = starry_default_disk_image(arch)?;
    println!("Preparing rootfs for {} at {}...", arch, disk_img.display());
    ensure_rootfs_in_target_dir(arch, &disk_img)?;
    println!("rootfs ready at {}", disk_img.display());
    Ok(())
}

fn run_img_command(arch: Option<String>) -> Result<()> {
    eprintln!(
        "\u{1b}[33mWARN: The 'img' command is deprecated. Please use 'rootfs' instead.\u{1b}[0m"
    );
    run_rootfs_command(arch)
}

pub async fn run_test(target: &str) -> Result<()> {
    let arch = parse_starry_target_for_test(target)?;
    let args = RunArgs {
        build: BuildArgs {
            arch: Some(arch.to_string()),
            package: STARRY_PACKAGE.to_string(),
            platform: None,
            release: true,
            features: None,
            smp: None,
            plat_dyn: false,
        },
        blk: true,
        disk_img: None,
        net: false,
        net_dev: None,
        graphic: false,
        accel: false,
    };

    let run_result = run_with_arg(args).await;
    let cleanup_result = cleanup_generated_qemu_config();

    match (run_result, cleanup_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(run_err), Ok(())) => Err(run_err),
        (Ok(()), Err(cleanup_err)) => Err(cleanup_err),
        (Err(run_err), Err(cleanup_err)) => {
            Err(run_err.context(format!("also failed to cleanup qemu config: {cleanup_err}")))
        }
    }
}

fn cleanup_generated_qemu_config() -> Result<()> {
    use axbuild::arceos::QEMU_CONFIG_FILE_NAME;

    let manifest_dir =
        std::env::current_dir().context("failed to get current working directory")?;
    let app_dir = axbuild::arceos::resolve_package_app_dir(&manifest_dir, STARRY_PACKAGE)?;
    let qemu_config_path = manifest_dir.join(app_dir).join(QEMU_CONFIG_FILE_NAME);
    if qemu_config_path.exists() {
        std::fs::remove_file(&qemu_config_path)
            .with_context(|| format!("failed to remove {}", qemu_config_path.display()))?;
    }
    Ok(())
}
