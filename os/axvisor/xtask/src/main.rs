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
    ffi::{OsStr, OsString},
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
/// - `-D` / `--download-dir` (image global override)
/// - `-E` / `--extract-dir`  (image global override)
/// - `--output`              (resize subcommand)
#[cfg(any(windows, all(unix, not(target_env = "musl"))))]
fn normalize_image_paths(args: &mut [OsString], invocation_dir: &Path) {
    const PATH_FLAGS: &[&str] = &["-D", "--download-dir", "-E", "--extract-dir", "--output"];

    let mut i = 1; // skip binary name
    while i < args.len() {
        // Handle --flag=<value> / -D=<value> form.
        // Match the ASCII flag prefix at the byte level via try_strip_prefix so
        // that non-UTF-8 path values (valid on POSIX) are not silently dropped.
        let mut matched = false;
        for flag in PATH_FLAGS {
            let prefix = format!("{flag}=");
            if let Some(rest) = try_strip_prefix(&args[i], &prefix) {
                let path = Path::new(rest);
                if path.is_relative() {
                    let abs = invocation_dir.join(path);
                    args[i] = prepend_flag(flag, &abs);
                }
                matched = true;
                break;
            }

            // Handle attached short options: -Dvalue / -Evalue (no =, no space).
            // Must come AFTER the =<value> check so -D=cache is still handled
            // by that branch.
            if flag.len() == 2
                && let Some(rest) = try_strip_prefix(&args[i], flag)
            {
                // Skip empty (bare -D falls through to space form) and
                // = prefixed (already handled by the = form above).
                if !rest.is_empty() && !starts_with_equals(rest) {
                    let path = Path::new(rest);
                    if path.is_relative() {
                        let abs = invocation_dir.join(path);
                        // Reconstruct as -D/path (no =, preserving attached form)
                        let mut new_arg = OsString::from(flag);
                        new_arg.push(abs);
                        args[i] = new_arg;
                    }
                    matched = true;
                    break;
                }
            }
        }
        if matched {
            i += 1;
            continue;
        }

        // Handle -D <value> / --flag <value> form.
        // Flag tokens are always ASCII so to_str() is safe for the flag check;
        // the path value is handled as OsStr to preserve non-UTF-8 bytes.
        if let Some(current_str) = args[i].to_str()
            && PATH_FLAGS.contains(&current_str)
            && i + 1 < args.len()
        {
            let val = &args[i + 1];
            if !looks_like_flag(val) {
                let path = Path::new(val);
                if path.is_relative() {
                    args[i + 1] = invocation_dir.join(path).into_os_string();
                }
            }
            i += 2;
            continue;
        }

        i += 1;
    }
}

/// Strip `prefix` (which must be ASCII) from an `OsStr`.
///
/// On Unix this operates at the byte level so non-UTF-8 path content is
/// preserved; on Windows it falls back to string matching.
#[cfg(unix)]
fn try_strip_prefix<'a>(os: &'a OsStr, prefix: &str) -> Option<&'a OsStr> {
    use std::os::unix::ffi::OsStrExt;
    os.as_bytes()
        .strip_prefix(prefix.as_bytes())
        .map(OsStr::from_bytes)
}

#[cfg(not(unix))]
fn try_strip_prefix<'a>(os: &'a OsStr, prefix: &str) -> Option<&'a OsStr> {
    os.to_str()
        .and_then(|s| s.strip_prefix(prefix))
        .map(OsStr::new)
}

/// Returns `true` when `os` starts with a `-` byte (i.e. looks like a flag).
#[cfg(unix)]
fn looks_like_flag(os: &OsStr) -> bool {
    use std::os::unix::ffi::OsStrExt;
    os.as_bytes().first() == Some(&b'-')
}

#[cfg(not(unix))]
fn looks_like_flag(os: &OsStr) -> bool {
    os.to_str().map_or(false, |s| s.starts_with('-'))
}

/// Returns `true` when `os` starts with an `=` byte.
#[cfg(unix)]
fn starts_with_equals(os: &OsStr) -> bool {
    use std::os::unix::ffi::OsStrExt;
    os.as_bytes().first() == Some(&b'=')
}

#[cfg(not(unix))]
fn starts_with_equals(os: &OsStr) -> bool {
    os.to_str().map_or(false, |s| s.starts_with('='))
}

/// Reconstruct `flag=absolute_path` as an `OsString`, preserving non-UTF-8
/// bytes in `abs` on Unix.
#[cfg(unix)]
fn prepend_flag(flag: &str, abs: &Path) -> OsString {
    let mut os = OsString::from(flag);
    os.push("=");
    os.push(abs);
    os
}

#[cfg(not(unix))]
fn prepend_flag(flag: &str, abs: &Path) -> OsString {
    OsString::from(format!("{flag}={}", abs.display()))
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
    use std::{
        ffi::{OsStr, OsString},
        path::Path,
    };

    use super::normalize_image_paths;

    fn os(s: &str) -> OsString {
        OsString::from(s)
    }

    fn assert_path_eq(actual: &OsString, expected: &str) {
        assert_eq!(actual.to_str().unwrap(), expected, "path mismatch");
    }

    #[test]
    fn relative_extract_dir_is_resolved() {
        let inv = Path::new("/home/user/project");
        let mut args = vec![
            os("xtask"),
            os("image"),
            os("pull"),
            os("--extract-dir"),
            os("out/images"),
            os("qemu-aarch64"),
        ];
        normalize_image_paths(&mut args, inv);
        assert_path_eq(&args[4], "/home/user/project/out/images");
    }

    #[test]
    fn relative_extract_dir_equals_form() {
        let inv = Path::new("/home/user/project");
        let mut args = vec![os("xtask"), os("pull"), os("--extract-dir=../staging")];
        normalize_image_paths(&mut args, inv);
        // Path::join preserves `..` components (matching `to_absolute_path` in
        // axbuild); the filesystem resolves them equivalently.
        assert_path_eq(&args[2], "--extract-dir=/home/user/project/../staging");
    }

    #[test]
    fn relative_download_dir_short_flag() {
        let inv = Path::new("/tmp/work");
        let mut args = vec![
            os("xtask"),
            os("image"),
            os("-D"),
            os("my-store"),
            os("pull"),
            os("qemu-aarch64"),
        ];
        normalize_image_paths(&mut args, inv);
        assert_path_eq(&args[3], "/tmp/work/my-store");
    }

    #[test]
    fn relative_download_dir_equals_form() {
        let inv = Path::new("/tmp/work");
        let mut args = vec![
            os("xtask"),
            os("image"),
            os("--download-dir=cache/img"),
            os("pull"),
            os("qemu-aarch64"),
        ];
        normalize_image_paths(&mut args, inv);
        assert_path_eq(&args[2], "--download-dir=/tmp/work/cache/img");
    }

    #[test]
    fn absolute_path_untouched() {
        let inv = Path::new("/home/user/project");
        let mut args = vec![
            os("xtask"),
            os("image"),
            os("pull"),
            os("--extract-dir"),
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
    fn short_e_flag_with_equals_is_normalised() {
        // `-E=<value>` is valid clap syntax (clap accepts `=` form for short flags too).
        let inv = Path::new("/home/user/project");
        let mut args = vec![os("xtask"), os("image"), os("pull"), os("-E=tmp/out")];
        normalize_image_paths(&mut args, inv);
        assert_path_eq(&args[3], "-E=/home/user/project/tmp/out");
    }

    #[test]
    fn non_path_value_not_starts_with_dash() {
        // -D followed by a flag-like value should not be treated as a path
        let inv = Path::new("/home/user/project");
        let mut args = vec![
            os("xtask"),
            os("image"),
            os("pull"),
            os("-D"),
            os("--another-flag"),
        ];
        let expected = args.clone();
        normalize_image_paths(&mut args, inv);
        assert_eq!(args, expected);
    }

    #[test]
    #[cfg(unix)]
    fn non_utf8_download_dir_short_flag() {
        use std::os::unix::ffi::OsStrExt;
        let inv = Path::new("/tmp/work");
        let val = OsString::from(OsStr::from_bytes(b"cache-\xff"));
        let mut args = vec![
            OsString::from("xtask"),
            OsString::from("image"),
            OsString::from("-D"),
            val,
            OsString::from("pull"),
            OsString::from("qemu-aarch64"),
        ];
        normalize_image_paths(&mut args, inv);
        let mut expected = Vec::from(b"/tmp/work/cache-");
        expected.push(0xff);
        assert_eq!(args[3].as_bytes(), expected.as_slice());
    }

    #[test]
    #[cfg(unix)]
    fn non_utf8_download_dir_equals_form() {
        use std::os::unix::ffi::OsStrExt;
        let inv = Path::new("/tmp/work");
        let arg = OsString::from(OsStr::from_bytes(b"--download-dir=cache-\xff"));
        let mut args = vec![
            OsString::from("xtask"),
            OsString::from("image"),
            arg,
            OsString::from("pull"),
            OsString::from("qemu-aarch64"),
        ];
        normalize_image_paths(&mut args, inv);
        let mut expected = Vec::from(b"--download-dir=/tmp/work/cache-");
        expected.push(0xff);
        assert_eq!(args[2].as_bytes(), expected.as_slice());
    }

    #[test]
    fn attached_short_d_form_is_normalised() {
        // -Dcache → -D/tmp/work/cache (no = sign, attached form preserved)
        let inv = Path::new("/tmp/work");
        let mut args = vec![
            os("xtask"),
            os("image"),
            os("-Dcache"),
            os("pull"),
            os("qemu-aarch64"),
        ];
        normalize_image_paths(&mut args, inv);
        assert_path_eq(&args[2], "-D/tmp/work/cache");
    }

    #[test]
    fn attached_short_e_form_is_normalised() {
        // -Eout → -E/home/user/project/out
        let inv = Path::new("/home/user/project");
        let mut args = vec![
            os("xtask"),
            os("image"),
            os("pull"),
            os("-Eout"),
            os("qemu-aarch64"),
        ];
        normalize_image_paths(&mut args, inv);
        assert_path_eq(&args[3], "-E/home/user/project/out");
    }

    #[test]
    #[cfg(unix)]
    fn attached_short_d_form_non_utf8() {
        use std::os::unix::ffi::OsStrExt;
        let inv = Path::new("/tmp/work");
        let mut arg_bytes = Vec::from(b"-Dcache-");
        arg_bytes.push(0xff);
        let arg = OsString::from(OsStr::from_bytes(&arg_bytes));
        let mut args = vec![
            OsString::from("xtask"),
            OsString::from("image"),
            arg,
            OsString::from("pull"),
            OsString::from("qemu-aarch64"),
        ];
        normalize_image_paths(&mut args, inv);
        let mut expected = Vec::from(b"-D/tmp/work/cache-");
        expected.push(0xff);
        assert_eq!(args[2].as_bytes(), expected.as_slice());
    }

    #[test]
    #[cfg(unix)]
    fn attached_short_e_form_non_utf8() {
        use std::os::unix::ffi::OsStrExt;
        let inv = Path::new("/home/user/project");
        let mut arg_bytes = Vec::from(b"-Eout-");
        arg_bytes.push(0xff);
        let arg = OsString::from(OsStr::from_bytes(&arg_bytes));
        let mut args = vec![
            OsString::from("xtask"),
            OsString::from("image"),
            OsString::from("pull"),
            arg,
            OsString::from("qemu-aarch64"),
        ];
        normalize_image_paths(&mut args, inv);
        let mut expected = Vec::from(b"-E/home/user/project/out-");
        expected.push(0xff);
        assert_eq!(args[3].as_bytes(), expected.as_slice());
    }

    #[test]
    fn attached_short_absolute_path_unchanged() {
        // -D/absolute should not be touched
        let inv = Path::new("/tmp/work");
        let mut args = vec![
            os("xtask"),
            os("image"),
            os("-D/usr/local"),
            os("pull"),
            os("qemu-aarch64"),
        ];
        let expected = args.clone();
        normalize_image_paths(&mut args, inv);
        assert_eq!(args, expected);
    }

    #[test]
    fn attached_short_equals_form_still_works() {
        // -D=rel must still be handled by the = form (regression guard)
        let inv = Path::new("/tmp/work");
        let mut args = vec![
            os("xtask"),
            os("image"),
            os("-D=rel"),
            os("pull"),
            os("qemu-aarch64"),
        ];
        normalize_image_paths(&mut args, inv);
        assert_path_eq(&args[2], "-D=/tmp/work/rel");
    }

    #[test]
    fn attached_short_combined_d_e() {
        // Clap treats -De as -D with value "e", not -D + -E
        let inv = Path::new("/tmp/work");
        let mut args = vec![
            os("xtask"),
            os("image"),
            os("-De"),
            os("pull"),
            os("qemu-aarch64"),
        ];
        normalize_image_paths(&mut args, inv);
        assert_path_eq(&args[2], "-D/tmp/work/e");
    }

    #[test]
    fn attached_short_alone_falls_through() {
        // Bare -D with no attached value: the fallthrough to space form
        // must still work (regression guard).
        let inv = Path::new("/tmp/work");
        let mut args = vec![
            os("xtask"),
            os("image"),
            os("-D"),
            os("store"),
            os("pull"),
            os("qemu-aarch64"),
        ];
        normalize_image_paths(&mut args, inv);
        assert_path_eq(&args[3], "/tmp/work/store");
    }
}
