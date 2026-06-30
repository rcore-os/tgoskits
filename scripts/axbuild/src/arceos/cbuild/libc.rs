use std::{collections::HashMap, path::Path, process::Command};

use anyhow::Context;
use ostool::build::config::Cargo;

use crate::support::process::ProcessExt;

pub(super) const AX_LIBC_PACKAGE: &str = "ax-libc";
pub(super) const PIC_RUSTFLAG: &str = "-Crelocation-model=pic";

pub(super) fn build_axlibc_staticlib(
    workspace_root: &Path,
    cargo: &Cargo,
    target_dir: &Path,
    debug: bool,
    dynamic_pie: bool,
) -> anyhow::Result<()> {
    let mut command = Command::new("cargo");
    let mut env = cargo.env.clone();
    if dynamic_pie {
        append_pic_rustflag(&mut env);
    }
    command
        .current_dir(workspace_root)
        .arg("build")
        .arg("-p")
        .arg(&cargo.package)
        .arg("--target")
        .arg(&cargo.target)
        .arg("-Z")
        .arg("unstable-options")
        .arg("--target-dir")
        .arg(target_dir)
        .arg("--features")
        .arg(cargo.features.join(","));
    if !debug {
        command.arg("--release");
    }
    for arg in &cargo.args {
        command.arg(arg);
    }
    for (key, value) in &env {
        command.env(key, value);
    }
    command
        .exec()
        .context("failed to build ax-libc static library")
}

pub(super) fn append_pic_rustflag(env: &mut HashMap<String, String>) {
    const ENCODED_RUSTFLAGS: &str = "CARGO_ENCODED_RUSTFLAGS";
    const RUSTFLAGS: &str = "RUSTFLAGS";

    if let Some(flags) = env.get_mut(ENCODED_RUSTFLAGS) {
        if !flags.is_empty() {
            flags.push('\x1f');
        }
        flags.push_str(PIC_RUSTFLAG);
        return;
    }

    if let Some(flags) = env.get_mut(RUSTFLAGS) {
        if !flags.is_empty() {
            flags.push(' ');
        }
        flags.push_str(PIC_RUSTFLAG);
        return;
    }

    env.insert(ENCODED_RUSTFLAGS.to_string(), PIC_RUSTFLAG.to_string());
}
