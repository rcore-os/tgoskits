//! Prepare the ivshmem guest programs and kernel-matched UIO module for QEMU.

use std::{
    fs,
    path::{Component, Path, PathBuf},
    process::Command,
};

use anyhow::{Context, ensure};

use crate::support::process::ProcessExt;

pub(super) const SMOKE_ENV: &str = "AXVISOR_TEST_IVSHMEM_SMOKE";
pub(super) const ARCEOS_ENV: &str = "AXVISOR_TEST_IVSHMEM_ARCEOS_SMOKE";
pub(super) const MODULE_ARCHIVE_PATH: &str = "lib/modules/axvisor.ko";
pub(super) const MODULE_ROOTFS_PATH: &str = "/root/axvisor.ko";
pub(super) const SMOKE_ARCHIVE_PATH: &str = "bin/ivshmem-bar2-smoke";
pub(super) const SUITE_ARCHIVE_PATH: &str = "bin/ivshmem-pci-suite";
pub(super) const SUBSCRIBER_ARCHIVE_PATH: &str = "bin/ivshmem_subscriber";

const ADAPTER_SOURCES: &[&str] = &["discovery.c", "backend_polling.c", "errors.c"];

pub(super) struct SmokeBinaries {
    pub(super) smoke: Vec<u8>,
    pub(super) suite: Vec<u8>,
    pub(super) subscriber: Vec<u8>,
}

fn workspace_path(root: &Path, configured: &str, variable: &str) -> anyhow::Result<PathBuf> {
    let path = Path::new(configured);
    ensure!(
        !path.as_os_str().is_empty()
            && !path.is_absolute()
            && path
                .components()
                .all(|component| matches!(component, Component::Normal(_))),
        "{variable} must be a nonempty workspace-relative path without parent traversal"
    );
    Ok(root.join(path))
}

pub(super) fn build_arceos_smoke(
    root: &Path,
    target_dir: &Path,
    arch: &str,
    configured: &str,
) -> anyhow::Result<()> {
    ensure!(arch == "aarch64", "ArceOS ivshmem peer requires aarch64");
    let config = workspace_path(root, configured, ARCEOS_ENV)?;
    ensure!(
        config.is_file(),
        "ArceOS ivshmem build config {} does not exist",
        config.display()
    );
    let xtask = std::env::current_exe().context("failed to locate the running xtask")?;
    let mut command = Command::new(xtask);
    command
        .current_dir(root)
        .args([
            "arceos",
            "build",
            "--package",
            "arceos-ivshmem-pci",
            "--config",
        ])
        .arg(&config);
    command
        .exec()
        .context("failed to build ArceOS ivshmem peer")?;
    ensure!(
        target_dir
            .join("aarch64-unknown-linux-musl/release/arceos-ivshmem-pci.bin")
            .is_file(),
        "ArceOS ivshmem build did not produce a raw guest image"
    );
    Ok(())
}

pub(super) fn build_smoke_binaries(
    root: &Path,
    target_dir: &Path,
    arch: &str,
) -> anyhow::Result<SmokeBinaries> {
    ensure!(arch == "aarch64", "ivshmem Linux smoke requires aarch64");
    let compiler = "aarch64-linux-musl-gcc";
    let archiver = "aarch64-linux-musl-ar";
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
            compiler,
            &flags,
            &source.join("lib"),
            &input,
            &out.join(format!("{name}.o")),
        )?);
    }
    let library = out.join("libivshmem.a");
    Command::new(archiver)
        .arg("rcs")
        .arg(&library)
        .args(&objects)
        .exec()
        .context("failed to archive ivshmem adapter")?;
    let build = |name: &str, input: &Path| -> anyhow::Result<Vec<u8>> {
        let object = compile(
            compiler,
            &flags,
            &source.join("lib"),
            input,
            &out.join(format!("{name}.o")),
        )?;
        let binary = out.join(name);
        Command::new(compiler)
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
        subscriber: build(
            "ivshmem_subscriber",
            &root.join("apps/linux/ivshmem_subscriber/main.c"),
        )?,
    })
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
