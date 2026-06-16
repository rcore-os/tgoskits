// Copyright 2025 The Axvisor Team
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

//! Build tool for Axvisor hypervisor.
//!
//! This crate provides the `xtask` binary for building, running, and managing
//! the Axvisor hypervisor project.

#![cfg_attr(not(any(windows, unix)), no_main)]
#![cfg_attr(not(any(windows, unix)), no_std)]

#[cfg(not(any(windows, unix)))]
mod lang;

#[cfg(any(windows, unix))]
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

#[cfg(any(windows, unix))]
#[derive(clap::Parser)]
struct Cli {
    #[command(subcommand)]
    command: axbuild::axvisor::Command,
}

#[cfg(any(windows, unix))]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    use clap::Parser;

    let mut cli = Cli::parse_from(normalize_legacy_args(std::env::args_os()));
    let invocation_dir = std::env::current_dir()?;
    let workspace_root = workspace_root();
    normalize_command_paths(&mut cli.command, &invocation_dir, &workspace_root);
    std::env::set_current_dir(&workspace_root)?;
    axbuild::axvisor::Axvisor::new()?
        .execute(cli.command)
        .await?;
    Ok(())
}

#[cfg(any(windows, unix))]
fn normalize_legacy_args(args: impl IntoIterator<Item = OsString>) -> Vec<OsString> {
    args.into_iter()
        .map(|arg| match arg.to_str() {
            Some("--build-config") => OsString::from("--config"),
            Some(value) if value.starts_with("--build-config=") => {
                OsString::from(value.replacen("--build-config=", "--config=", 1))
            }
            _ => arg,
        })
        .collect()
}

#[cfg(any(windows, unix))]
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .expect("failed to locate workspace root from os/axvisor")
}

#[cfg(any(windows, unix))]
fn normalize_command_paths(
    command: &mut axbuild::axvisor::Command,
    invocation_dir: &Path,
    workspace_root: &Path,
) {
    use axbuild::axvisor::{Command, TestCommand, image};

    match command {
        Command::Build(args) => normalize_build_paths(args, invocation_dir, workspace_root),
        Command::Qemu(args) => {
            normalize_build_paths(&mut args.build, invocation_dir, workspace_root);
            normalize_existing_path(&mut args.qemu_config, invocation_dir, workspace_root);
            normalize_existing_path(&mut args.rootfs, invocation_dir, workspace_root);
        }
        Command::Board(args) => {
            normalize_build_paths(&mut args.build, invocation_dir, workspace_root);
            normalize_existing_path(&mut args.board_config, invocation_dir, workspace_root);
        }
        Command::Uboot(args) => {
            normalize_build_paths(&mut args.build, invocation_dir, workspace_root);
            normalize_existing_path(&mut args.uboot_config, invocation_dir, workspace_root);
        }
        Command::Image(args) => {
            normalize_output_path(&mut args.overrides.local_storage, invocation_dir);
            if let image::Command::Pull(args) = &mut args.command {
                normalize_output_path(&mut args.output_dir, invocation_dir);
            }
        }
        Command::Test(args) => match &mut args.command {
            TestCommand::Uboot(args) => {
                normalize_existing_path(&mut args.uboot_config, invocation_dir, workspace_root);
            }
            TestCommand::Qemu(_) | TestCommand::Board(_) => {}
        },
        Command::Defconfig(_) | Command::Config(_) => {}
    }
}

#[cfg(any(windows, unix))]
fn normalize_build_paths(
    args: &mut axbuild::axvisor::ArgsBuild,
    invocation_dir: &Path,
    workspace_root: &Path,
) {
    normalize_existing_path(&mut args.config, invocation_dir, workspace_root);
    for path in &mut args.vmconfigs {
        *path = resolve_existing_path(path, invocation_dir, workspace_root);
    }
}

#[cfg(any(windows, unix))]
fn normalize_existing_path(
    path: &mut Option<PathBuf>,
    invocation_dir: &Path,
    workspace_root: &Path,
) {
    if let Some(path) = path {
        *path = resolve_existing_path(path, invocation_dir, workspace_root);
    }
}

#[cfg(any(windows, unix))]
fn resolve_existing_path(path: &Path, invocation_dir: &Path, workspace_root: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }

    let cwd_path = invocation_dir.join(path);
    let workspace_path = workspace_root.join(path);
    if workspace_path.exists() && !cwd_path.exists() {
        workspace_path
    } else {
        cwd_path
    }
}

#[cfg(any(windows, unix))]
fn normalize_output_path(path: &mut Option<PathBuf>, invocation_dir: &Path) {
    if let Some(path) = path
        && path.is_relative()
    {
        *path = invocation_dir.join(&path);
    }
}
