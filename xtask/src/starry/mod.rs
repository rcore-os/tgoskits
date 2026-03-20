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

use anyhow::Result;
use clap::Subcommand;

pub mod build;
pub mod config;
pub mod run;

pub use build::{BuildArgs, STARRY_TEST_PACKAGE, run_build};
pub use run::RunArgs;

use crate::starry::{
    config::{
        ensure_rootfs_in_target_dir, parse_starry_arch, parse_starry_target_for_test,
        starry_default_disk_image,
    },
    run::{default_test_fail_regex, default_test_success_regex, run_with_arg, run_with_qemu_regex},
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
            package: STARRY_TEST_PACKAGE.to_string(),
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
    run_with_qemu_regex(
        args,
        default_test_success_regex(),
        default_test_fail_regex(),
    )
    .await
}
