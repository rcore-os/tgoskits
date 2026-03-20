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

use anyhow::Result;
use axbuild::{
    QemuOptions,
    arceos::{AxBuild, NetDev, RunScope},
};
use clap::Args;

use super::build::BuildArgs;

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
        let mut overrides = self.build.as_override()?;
        let qemu = QemuOptions {
            blk: self.blk,
            disk_image: self.disk_img.clone().map(String::into),
            net: self.net,
            net_dev: self
                .net_dev
                .as_deref()
                .and_then(|dev| NetDev::from_str(dev).ok())
                .unwrap_or(NetDev::User),
            graphic: self.graphic,
            accel: self.accel,
            extra_args: vec![],
            success_regex: vec![],
            fail_regex: vec![],
        };

        overrides.qemu = Some(qemu);
        Ok(overrides)
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
    let mut overrides = arg.as_override()?;
    if let Some(qemu) = overrides.qemu.as_mut() {
        qemu.success_regex = success_regex;
        qemu.fail_regex = fail_regex;
    }
    let axbuild = AxBuild::from_overrides(overrides, Some(package), None, run_scope)?;
    if as_test {
        println!("Running test in QEMU...");
        axbuild.test().await
    } else {
        println!("Running in QEMU...");
        axbuild.run_qemu().await
    }
}
