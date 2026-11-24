//! Prepare ivshmem guest programs and the kernel-matched UIO module for QEMU.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, ensure};

use crate::support::{process::ProcessExt, rustup};

pub(super) const MODULE_ARCHIVE_PATH: &str = "lib/modules/axvisor.ko";
pub(super) const MODULE_ROOTFS_PATH: &str = "/root/axvisor.ko";
pub(super) const SMOKE_ARCHIVE_PATH: &str = "bin/ivshmem-bar2-smoke";
pub(super) const SUITE_ARCHIVE_PATH: &str = "bin/ivshmem-pci-suite";
pub(super) const LINUX_SUBSCRIBER_ARCHIVE_PATH: &str = "bin/linux-ivshmem-subscriber";

const ADAPTER_SOURCES: &[&str] = &["discovery.c", "backend_polling.c", "errors.c"];
const LINUX_SUBSCRIBER_PACKAGE: &str = "linux-ivshmem-subscriber";
const LINUX_SUBSCRIBER_TARGET: &str = "aarch64-unknown-linux-musl";
const LINUX_MUSL_COMPILER: &str = "aarch64-linux-musl-gcc";
const LINUX_MUSL_ARCHIVER: &str = "aarch64-linux-musl-ar";

pub(super) struct SmokeBinaries {
    pub(super) smoke: Vec<u8>,
    pub(super) suite: Vec<u8>,
}

pub(super) struct LinuxSubscriberBinary {
    pub(super) binary: Vec<u8>,
}

pub(super) enum GuestBinaries {
    Smoke(SmokeBinaries),
    LinuxSubscriber(LinuxSubscriberBinary),
}

pub(super) fn build_smoke_binaries(
    root: &Path,
    target_dir: &Path,
    arch: &str,
) -> anyhow::Result<SmokeBinaries> {
    ensure!(arch == "aarch64", "ivshmem Linux smoke requires aarch64");
    let source = root.join("apps/linux/ivshmem");
    let out = target_dir.join("axbuild/ivshmem-smoke/aarch64");
    fs::create_dir_all(&out).with_context(|| format!("failed to create {}", out.display()))?;
    let flags = [
        "-std=c11",
        "-Os",
        "-g0",
        "-Wall",
        "-Wextra",
        "-Werror",
        "-ffunction-sections",
        "-fdata-sections",
    ];
    let mut objects = Vec::new();
    for name in ADAPTER_SOURCES {
        let input = source.join("lib").join(name);
        objects.push(compile(
            LINUX_MUSL_COMPILER,
            &flags,
            &source.join("lib"),
            &input,
            &out.join(format!("{name}.o")),
        )?);
    }
    let library = out.join("libivshmem.a");
    Command::new(LINUX_MUSL_ARCHIVER)
        .arg("rcs")
        .arg(&library)
        .args(&objects)
        .exec()
        .context("failed to archive ivshmem adapter")?;
    let build = |name: &str, input: &Path| -> anyhow::Result<Vec<u8>> {
        let object = compile(
            LINUX_MUSL_COMPILER,
            &flags,
            &source.join("lib"),
            input,
            &out.join(format!("{name}.o")),
        )?;
        let binary = out.join(name);
        Command::new(LINUX_MUSL_COMPILER)
            .args(["-static", "-Wl,--gc-sections"])
            .arg("-o")
            .arg(&binary)
            .arg(&object)
            .arg(&library)
            .exec()
            .with_context(|| format!("failed to link {}", binary.display()))?;
        fs::read(&binary).with_context(|| format!("failed to read {}", binary.display()))
    };
    Ok(SmokeBinaries {
        smoke: build("ivshmem-bar2-smoke", &source.join("bar2_smoke/main.c"))?,
        suite: build("ivshmem-pci-suite", &source.join("suite/main.c"))?,
    })
}

pub(super) fn build_linux_subscriber(
    workspace_root: &Path,
    target_dir: &Path,
    arch: &str,
) -> anyhow::Result<LinuxSubscriberBinary> {
    ensure!(
        arch == "aarch64",
        "linux ivshmem subscriber requires aarch64"
    );
    rustup::ensure_target(LINUX_SUBSCRIBER_TARGET)?;
    let cargo_target_dir = target_dir.join("axbuild/linux-ivshmem-subscriber/aarch64");
    fs::create_dir_all(&cargo_target_dir)
        .with_context(|| format!("failed to create {}", cargo_target_dir.display()))?;
    Command::new("cargo")
        .current_dir(workspace_root)
        .args([
            "build",
            "--release",
            "--package",
            LINUX_SUBSCRIBER_PACKAGE,
            "--bin",
            LINUX_SUBSCRIBER_PACKAGE,
            "--target",
            LINUX_SUBSCRIBER_TARGET,
        ])
        .arg("--target-dir")
        .arg(&cargo_target_dir)
        .env(
            "CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER",
            LINUX_MUSL_COMPILER,
        )
        .env("CC_aarch64_unknown_linux_musl", LINUX_MUSL_COMPILER)
        .env("AR_aarch64_unknown_linux_musl", LINUX_MUSL_ARCHIVER)
        .env("RUSTFLAGS", "-C target-feature=+crt-static")
        .exec()
        .context("failed to build linux-ivshmem-subscriber for aarch64-musl")?;

    let binary = cargo_target_dir
        .join(LINUX_SUBSCRIBER_TARGET)
        .join("release")
        .join(LINUX_SUBSCRIBER_PACKAGE);
    let binary =
        fs::read(&binary).with_context(|| format!("failed to read {}", binary.display()))?;
    ensure!(!binary.is_empty(), "linux-ivshmem-subscriber is empty");
    Ok(LinuxSubscriberBinary { binary })
}

fn compile(
    compiler: &str,
    flags: &[&str],
    includes: &Path,
    input: &Path,
    output: &Path,
) -> anyhow::Result<PathBuf> {
    Command::new(compiler)
        .args(flags)
        .arg(format!("-I{}", includes.display()))
        .arg("-c")
        .arg(input)
        .arg("-o")
        .arg(output)
        .exec()
        .with_context(|| format!("failed to compile {}", input.display()))?;
    Ok(output.to_path_buf())
}
