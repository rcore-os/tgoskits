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
        let mut ax_args = ensure_image_prefix(raw_args);
        normalize_image_paths(&mut ax_args, &invocation_dir);
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

/// Resolve relative paths in image command arguments against the invocation
/// directory. This must happen *before* `set_current_dir(workspace_root)`,
/// because axbuild's image logic calls `to_absolute_path()` which resolves
/// relative paths against `std::env::current_dir()`.
///
/// Path-valued flags that are normalised:
/// - `-S` / `--local-storage`  (image global override)
/// - `-o` / `--output-dir`     (pull subcommand)
/// - `--output`                (resize subcommand)
#[cfg(any(windows, all(unix, not(target_env = "musl"))))]
fn normalize_image_paths(args: &mut [OsString], invocation_dir: &Path) {
    const PATH_FLAGS: &[&str] = &["-S", "--local-storage", "-o", "--output-dir", "--output"];

    let mut i = 1; // skip binary name
    while i < args.len() {
        let Some(current) = args[i].to_str().map(String::from) else {
            i += 1;
            continue;
        };

        // Handle --flag=<value> / -S=<value> form
        let mut matched = false;
        for flag in PATH_FLAGS {
            let prefix = format!("{flag}=");
            if let Some(rel) = current.strip_prefix(&prefix) {
                let path = Path::new(rel);
                if path.is_relative() {
                    let abs = invocation_dir.join(path);
                    args[i] = OsString::from(format!("{flag}={}", abs.display()));
                }
                matched = true;
                break;
            }
        }
        if matched {
            i += 1;
            continue;
        }

        // Handle -S <value> / --flag <value> form
        if PATH_FLAGS.contains(&current.as_str()) && i + 1 < args.len() {
            if let Some(val_str) = args[i + 1].to_str() {
                let path = Path::new(val_str);
                if path.is_relative() && !val_str.starts_with('-') {
                    args[i + 1] = OsString::from(invocation_dir.join(path).display().to_string());
                }
            }
            i += 2;
            continue;
        }

        i += 1;
    }
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

#[cfg(all(test, any(windows, all(unix, not(target_env = "musl")))))]
mod tests {
    use std::{ffi::OsString, path::Path};

    use super::normalize_image_paths;

    fn os(s: &str) -> OsString {
        OsString::from(s)
    }

    fn assert_path_eq(actual: &OsString, expected: &str) {
        assert_eq!(actual.to_str().unwrap(), expected, "path mismatch");
    }

    #[test]
    fn relative_output_dir_is_resolved() {
        let inv = Path::new("/home/user/project");
        let mut args = vec![
            os("xtask"),
            os("image"),
            os("pull"),
            os("--output-dir"),
            os("out/images"),
            os("qemu-aarch64"),
        ];
        normalize_image_paths(&mut args, inv);
        assert_path_eq(&args[4], "/home/user/project/out/images");
    }

    #[test]
    fn relative_output_dir_equals_form() {
        let inv = Path::new("/home/user/project");
        let mut args = vec![os("xtask"), os("pull"), os("--output-dir=../staging")];
        normalize_image_paths(&mut args, inv);
        // Path::join preserves `..` components (matching `to_absolute_path` in
        // axbuild); the filesystem resolves them equivalently.
        assert_path_eq(&args[2], "--output-dir=/home/user/project/../staging");
    }

    #[test]
    fn relative_local_storage_short_flag() {
        let inv = Path::new("/tmp/work");
        let mut args = vec![
            os("xtask"),
            os("image"),
            os("-S"),
            os("my-store"),
            os("pull"),
            os("qemu-aarch64"),
        ];
        normalize_image_paths(&mut args, inv);
        assert_path_eq(&args[3], "/tmp/work/my-store");
    }

    #[test]
    fn relative_local_storage_equals_form() {
        let inv = Path::new("/tmp/work");
        let mut args = vec![
            os("xtask"),
            os("image"),
            os("--local-storage=cache/img"),
            os("pull"),
            os("qemu-aarch64"),
        ];
        normalize_image_paths(&mut args, inv);
        assert_path_eq(&args[2], "--local-storage=/tmp/work/cache/img");
    }

    #[test]
    fn absolute_path_untouched() {
        let inv = Path::new("/home/user/project");
        let mut args = vec![
            os("xtask"),
            os("image"),
            os("pull"),
            os("--output-dir"),
            os("/absolute/path"),
            os("qemu-aarch64"),
        ];
        normalize_image_paths(&mut args, inv);
        // absolute path unchanged
        assert_path_eq(&args[4], "/absolute/path");
    }

    #[test]
    fn output_flag_for_resize() {
        let inv = Path::new("/home/user/project");
        let mut args = vec![
            os("xtask"),
            os("image"),
            os("resize"),
            os("rootfs.img"),
            os("--output"),
            os("resized.img"),
        ];
        normalize_image_paths(&mut args, inv);
        assert_path_eq(&args[5], "/home/user/project/resized.img");
    }

    #[test]
    fn short_o_flag_with_equals_is_normalised() {
        // `-o=<value>` is valid clap syntax (clap accepts `=` form for short flags too).
        let inv = Path::new("/home/user/project");
        let mut args = vec![os("xtask"), os("image"), os("pull"), os("-o=tmp/out")];
        normalize_image_paths(&mut args, inv);
        assert_path_eq(&args[3], "-o=/home/user/project/tmp/out");
    }

    #[test]
    fn non_path_value_not_starts_with_dash() {
        // -S followed by a flag-like value should not be treated as a path
        let inv = Path::new("/home/user/project");
        let mut args = vec![
            os("xtask"),
            os("image"),
            os("pull"),
            os("-S"),
            os("--another-flag"),
        ];
        let expected = args.clone();
        normalize_image_paths(&mut args, inv);
        assert_eq!(args, expected);
    }
}
