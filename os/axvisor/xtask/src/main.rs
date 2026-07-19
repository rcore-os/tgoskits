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
//! This xtask delegates all logic to the shared [`axbuild`] library, keeping
//! this binary as a thin CLI shim. Two kinds of commands are supported:
//!
//! * **Axvisor commands** (`build`, `qemu`, `board`, `test`, `uboot`,
//!   `defconfig`, `config`) — parsed directly into [`axbuild::axvisor::Command`]
//!   and executed via [`axbuild::axvisor::Axvisor`].
//! * **Image commands** (`ls`, `pull`, `resize`, `check`, optionally prefixed
//!   with `image`) — an `image` prefix is inserted if missing, then forwarded
//!   to [`axbuild::run_from`] so axbuild's own CLI dispatches them as
//!   `axbuild::Commands::Image(...)`.
//!
//! # Release / standalone distribution
//!
//! Inside the tgoskits workspace the dependency `axbuild = { workspace = true }`
//! resolves via the workspace root. When `os/axvisor` is published or synced
//! outside this workspace (e.g. as `arceos-hypervisor/axvisor`), one of the
//! following must be done before `cargo run --bin xtask` will work:
//!
//! 1. Publish the `axbuild` crate to crates.io and change this dependency to
//!    `axbuild = { version = "..." }` — keeps sharing a single implementation.
//! 2. Extract an `axvisor-build` crate from `axbuild` and depend on that
//!    instead — narrower dependency, more extraction work.
//! 3. Vendor the needed build logic directly into this xtask (not recommended;
//!    duplicates code and drifts over time).

#![cfg_attr(not(any(windows, all(unix, not(target_env = "musl")))), no_main)]
#![cfg_attr(not(any(windows, all(unix, not(target_env = "musl")))), no_std)]

#[cfg(not(any(windows, all(unix, not(target_env = "musl")))))]
mod lang;

#[cfg(any(windows, all(unix, not(target_env = "musl"))))]
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

#[cfg(any(windows, all(unix, not(target_env = "musl"))))]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    use clap::Parser;

    let raw_args = normalize_legacy_args(std::env::args_os());
    let invocation_dir = std::env::current_dir()?;
    let workspace_root = workspace_root();

    if is_image_subcommand(&raw_args) {
        let ax_args = ensure_image_prefix(raw_args);
        std::env::set_current_dir(&workspace_root)?;
        axbuild::run_from(ax_args).await?;
    } else {
        let mut cli = AxvisorOnlyCli::parse_from(raw_args);
        normalize_command_paths(&mut cli.command, &invocation_dir, &workspace_root);
        std::env::set_current_dir(&workspace_root)?;
        axbuild::axvisor::Axvisor::new()?
            .execute(cli.command)
            .await?;
    }

    Ok(())
}

/// Detect whether the first positional argument after the binary name is an
/// image subcommand or the `image` keyword itself.
#[cfg(any(windows, all(unix, not(target_env = "musl"))))]
fn is_image_subcommand(args: &[OsString]) -> bool {
    const IMAGE_SUBCOMMANDS: &[&str] = &["ls", "pull", "resize", "check"];
    let first = args.iter().skip(1).find_map(|a| a.to_str());
    match first {
        Some("image") => true,
        Some(cmd) => IMAGE_SUBCOMMANDS.contains(&cmd),
        None => false,
    }
}

/// Ensure the argument list contains `image` as the first subcommand so
/// axbuild's own `Cli` dispatches it as `Commands::Image(...)`.
#[cfg(any(windows, all(unix, not(target_env = "musl"))))]
fn ensure_image_prefix(mut args: Vec<OsString>) -> Vec<OsString> {
    if args.get(1).and_then(|a| a.to_str()) == Some("image") {
        return args;
    }
    args.insert(1, OsString::from("image"));
    args
}

/// Parser that only recognises Axvisor subcommands.
#[cfg(any(windows, all(unix, not(target_env = "musl"))))]
#[derive(clap::Parser)]
struct AxvisorOnlyCli {
    #[command(subcommand)]
    command: axbuild::axvisor::Command,
}

#[cfg(not(any(windows, all(unix, not(target_env = "musl")))))]
#[unsafe(no_mangle)]
fn main() -> ! {
    loop {
        core::hint::spin_loop();
    }
}

#[cfg(not(any(windows, all(unix, not(target_env = "musl")))))]
#[rustfmt::skip]
#[unsafe(no_mangle)]
extern "C" fn _head() -> ! {
    main()
}

#[cfg(not(any(windows, all(unix, not(target_env = "musl")))))]
#[rustfmt::skip]
#[unsafe(no_mangle)]
extern "C" fn kernel_entry() -> ! {
    main()
}

#[cfg(any(windows, all(unix, not(target_env = "musl"))))]
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

#[cfg(any(windows, all(unix, not(target_env = "musl"))))]
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .expect("failed to locate workspace root from os/axvisor")
}

#[cfg(any(windows, all(unix, not(target_env = "musl"))))]
fn normalize_command_paths(
    command: &mut axbuild::axvisor::Command,
    invocation_dir: &Path,
    workspace_root: &Path,
) {
    use axbuild::axvisor::{Command, TestCommand};

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
        Command::Test(args) => match &mut args.command {
            TestCommand::Uboot(args) => {
                normalize_existing_path(&mut args.uboot_config, invocation_dir, workspace_root);
            }
            TestCommand::Qemu(_) | TestCommand::Board(_) => {}
        },
        Command::Defconfig(_) | Command::Config(_) => {}
    }
}

#[cfg(any(windows, all(unix, not(target_env = "musl"))))]
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

#[cfg(any(windows, all(unix, not(target_env = "musl"))))]
fn normalize_existing_path(
    path: &mut Option<PathBuf>,
    invocation_dir: &Path,
    workspace_root: &Path,
) {
    if let Some(path) = path {
        *path = resolve_existing_path(path, invocation_dir, workspace_root);
    }
}

#[cfg(any(windows, all(unix, not(target_env = "musl"))))]
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
