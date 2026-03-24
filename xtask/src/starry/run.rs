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

use std::{
    env::current_dir,
    path::PathBuf,
};

use anyhow::{Context, Result};
use axbuild::{
    Arch,
    arceos::{ArceosConfigOverride, RunScope},
};
use clap::Args;
use tracing::info;

use crate::qemu_override::{
    RuntimeQemuOverride, resolve_external_qemu_config_path, resolve_qemu_search_dir,
    write_qemu_override_file,
};

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
    pub async fn into_parts(self) -> Result<(ArceosConfigOverride, RuntimeQemuOverride, Arch)> {
        let arch = parse_starry_arch(self.build.arch.as_deref())?;
        let overrides = self.build.into_config_override()?;

        // Handle disk image
        let disk_img = if self.blk {
            let default_disk_img = starry_default_disk_image(arch)?;
            let disk_img_path = self
                .disk_img
                .as_ref()
                .map(PathBuf::from)
                .unwrap_or_else(|| default_disk_img.clone());

            if !disk_img_path.exists() {
                info!(
                    "disk image missing at {}, preparing rootfs...",
                    disk_img_path.display()
                );
                ensure_rootfs_in_target_dir(arch, &disk_img_path).await?;
            }
            Some(disk_img_path.display().to_string())
        } else {
            None
        };
        Ok((
            overrides,
            RuntimeQemuOverride {
                blk: self.blk,
                disk_img,
                net: self.net,
                net_dev: self.net_dev,
                graphic: self.graphic,
                accel: self.accel,
            },
            arch,
        ))
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
    let (overrides, runtime, arch) = args.into_parts().await?;
    let manifest_dir = current_dir().context("failed to get current directory")?;
    let search_dir = resolve_qemu_search_dir(&manifest_dir, &package, run_scope)?;
    let base_qemu = resolve_external_qemu_config_path(&manifest_dir, &search_dir, arch)?;
    let qemu_config_path = write_qemu_override_file(
        &base_qemu,
        &runtime,
        &success_regex,
        &fail_regex,
        arch,
    )?;
    if as_test {
        info!(
            "preparing to wait for QEMU test output; success_regex={:?}, fail_regex={:?}",
            success_regex, fail_regex
        );
    } else {
        info!("preparing to wait for interactive QEMU run");
    }
    let axbuild =
        axbuild::arceos::AxBuild::from_overrides(overrides, Some(package), None, run_scope)?;
    if as_test {
        info!("==> running test in QEMU");
        axbuild.test_with_config_path(qemu_config_path).await
    } else {
        info!("==> running in QEMU");
        axbuild.run_qemu_with_config_path(qemu_config_path).await
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
