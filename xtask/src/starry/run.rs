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

use std::path::PathBuf;

use anyhow::Result;
use axbuild::arceos::{ArceosConfigOverride, RunScope, parse_qemu_options};
use clap::Args;

use super::{
    build::{BuildArgs, STARRY_TEST_PACKAGE},
    config::{ensure_rootfs_in_target_dir, parse_starry_arch, starry_default_disk_image},
};

const STARRY_TEST_SUCCESS_REGEX: &[&str] = &["starry:~#"];
const STARRY_TEST_FAIL_REGEX: &[&str] = &["(?i)\\bpanic(?:ked)?\\b"];

/// Run command arguments
#[derive(Args, Debug)]
pub struct RunArgs {
    #[command(flatten)]
    pub build: BuildArgs,

    /// Enable block device
    #[arg(long, default_value_t = true)]
    pub blk: bool,

    /// Disk image path
    #[arg(long)]
    pub disk_img: Option<String>,

    /// Enable network
    #[arg(long, default_value_t = true)]
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
    pub fn into_config_override(self) -> Result<ArceosConfigOverride> {
        let arch = parse_starry_arch(self.build.arch.as_deref())?;
        let mut overrides = self.build.into_config_override()?;

        // Handle disk image
        let disk_img = if self.blk {
            let default_disk_img = starry_default_disk_image(arch)?;
            let disk_img_path = self
                .disk_img
                .as_ref()
                .map(PathBuf::from)
                .unwrap_or_else(|| default_disk_img.clone());

            if !disk_img_path.exists() {
                println!(
                    "disk image missing at {}, preparing rootfs...",
                    disk_img_path.display()
                );
                ensure_rootfs_in_target_dir(arch, &disk_img_path)?;
            }
            Some(disk_img_path.display().to_string())
        } else {
            None
        };

        overrides.qemu = Some(parse_qemu_options(
            self.blk,
            disk_img,
            self.net,
            self.net_dev,
            self.graphic,
            self.accel,
            vec![],
            vec![],
        ));
        Ok(overrides)
    }
}

/// Run the build and run command
pub async fn run_with_arg(args: RunArgs) -> Result<()> {
    run_with_qemu_regex(args, vec![], vec![]).await
}

pub async fn run_with_qemu_regex(
    args: RunArgs,
    success_regex: Vec<String>,
    fail_regex: Vec<String>,
) -> Result<()> {
    let as_test = !success_regex.is_empty() || !fail_regex.is_empty();
    let package = args.build.package.clone();
    let run_scope = if package == STARRY_TEST_PACKAGE {
        RunScope::PackageRoot
    } else {
        RunScope::StarryOsRoot
    };
    let mut overrides = args.into_config_override()?;
    if let Some(qemu) = overrides.qemu.as_mut() {
        qemu.success_regex = success_regex;
        qemu.fail_regex = fail_regex;
    }
    let axbuild =
        axbuild::arceos::AxBuild::from_overrides(overrides, Some(package), None, run_scope)?;
    if as_test {
        println!("Running test in QEMU...");
        axbuild.test().await
    } else {
        println!("Running in QEMU...");
        axbuild.run_qemu().await
    }
}

pub fn default_test_success_regex() -> Vec<String> {
    STARRY_TEST_SUCCESS_REGEX
        .iter()
        .map(|pattern| (*pattern).to_string())
        .collect()
}

pub fn default_test_fail_regex() -> Vec<String> {
    STARRY_TEST_FAIL_REGEX
        .iter()
        .map(|pattern| (*pattern).to_string())
        .collect()
}
