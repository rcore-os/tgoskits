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

use std::env::current_dir;

use anyhow::{Context, Result};
use axbuild::{Arch, arceos::{AxBuild, RunScope}};
use clap::Args;
use tracing::info;

use super::build::BuildArgs;
use crate::qemu_override::{
    RuntimeQemuOverride, resolve_external_qemu_config_path, resolve_qemu_search_dir,
    write_qemu_override_file,
};

/// Run command arguments
#[derive(Args, Debug)]
pub struct RunArgs {
    #[command(flatten)]
    pub build: BuildArgs,

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
    pub fn as_override(&self) -> Result<axbuild::arceos::ArceosConfigOverride> {
        self.build.as_override()
    }
}

/// Run the build and run command
pub async fn run_with_arg(arg: RunArgs) -> Result<()> {
    run_with_arg_in_scope(arg, RunScope::Default).await
}

pub async fn run_with_arg_in_scope(arg: RunArgs, run_scope: RunScope) -> Result<()> {
    run_with_mode_in_scope(arg, run_scope, vec![], vec![], false).await
}

pub async fn test_with_arg_in_scope(arg: RunArgs, run_scope: RunScope) -> Result<()> {
    run_with_mode_in_scope(arg, run_scope, vec![], vec![], true).await
}

async fn run_with_mode_in_scope(
    arg: RunArgs,
    run_scope: RunScope,
    success_regex: Vec<String>,
    fail_regex: Vec<String>,
    as_test: bool,
) -> Result<()> {
    let package = arg.build.package.clone();
    let overrides = arg.as_override()?;
    let arch = overrides.arch.unwrap_or(Arch::AArch64);
    let manifest_dir = current_dir().context("failed to get current directory")?;
    let search_dir = resolve_qemu_search_dir(&manifest_dir, &package, run_scope)?;
    let base_qemu = resolve_external_qemu_config_path(&manifest_dir, &search_dir, arch)?;
    let runtime = RuntimeQemuOverride {
        blk: arg.blk,
        disk_img: arg.disk_img.clone(),
        net: arg.net,
        net_dev: arg.net_dev.clone(),
        graphic: arg.graphic,
        accel: arg.accel,
    };
    let qemu_config_path =
        write_qemu_override_file(&base_qemu, &runtime, &success_regex, &fail_regex, arch)?;
    let axbuild = AxBuild::from_overrides(overrides, Some(package), None, run_scope)?;
    if as_test {
        info!("==> running test in QEMU");
        axbuild.test_with_config_path(qemu_config_path).await
    } else {
        info!("==> running in QEMU");
        axbuild.run_qemu_with_config_path(qemu_config_path).await
    }
}
