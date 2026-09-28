use std::{fs, path::Path, process::Command};

use anyhow::{Context, ensure};
use serde::Deserialize;
use walkdir::WalkDir;

use super::QemuConfig;

#[derive(Deserialize)]
struct HostInitramfsFixture {
    source: String,
    #[serde(default)]
    init_source: Option<String>,
}

/// Builds a test archive before its QEMU configuration is handed to ostool.
pub(crate) fn prepare_host_initramfs(
    workspace_root: &Path,
    target_dir: &Path,
    case_dir: &Path,
    arch: &str,
    qemu: &mut QemuConfig,
) -> anyhow::Result<()> {
    let manifest = case_dir.join("host-initramfs.toml");
    if !manifest.is_file() {
        return Ok(());
    }
    ensure!(
        qemu.boot.initramfs.is_none(),
        "{} must not also set initramfs in qemu config",
        manifest.display()
    );
    let fixture: HostInitramfsFixture = toml::from_str(&fs::read_to_string(&manifest)?)
        .with_context(|| format!("failed to parse {}", manifest.display()))?;
    let source = workspace_root.join(&fixture.source);
    ensure!(
        source.is_dir(),
        "missing initramfs source {}",
        source.display()
    );
    let case_path = case_dir
        .strip_prefix(workspace_root)
        .context("initramfs case is outside the workspace")?;
    let output = target_dir
        .join("axbuild/host-initramfs")
        .join(case_path)
        .join(format!("{arch}.cpio"));
    fs::create_dir_all(output.parent().expect("archive has a parent"))?;
    let staging = tempfile::tempdir_in(output.parent().expect("archive has a parent"))?;
    copy_fixture(&source, staging.path())?;

    if let Some(init_source) = fixture.init_source {
        ensure!(arch == "aarch64", "test init source supports only aarch64");
        let init_source = workspace_root.join(init_source);
        ensure!(
            init_source.is_file(),
            "missing init source {}",
            init_source.display()
        );
        let entry_source = init_source.with_file_name("entry-aarch64.S");
        ensure!(
            entry_source.is_file(),
            "missing init entry {}",
            entry_source.display()
        );
        build_test_init(&entry_source, &init_source, &staging.path().join("init"))?;
    }

    crate::image::pack_initramfs_dir(staging.path(), &output)?;
    qemu.boot.initramfs = Some(output.to_string_lossy().into_owned());
    Ok(())
}

fn build_test_init(entry_source: &Path, init_source: &Path, output: &Path) -> anyhow::Result<()> {
    let objects = tempfile::tempdir_in(output.parent().expect("test init has a parent"))?;
    let entry_object = objects.path().join("entry.o");
    let init_object = objects.path().join("init.o");
    for (source, object) in [(entry_source, &entry_object), (init_source, &init_object)] {
        let result = Command::new("clang")
            .arg("--target=aarch64-unknown-linux-musl")
            .args([
                "-ffreestanding",
                "-fno-builtin",
                "-fno-stack-protector",
                "-nostdlib",
                "-c",
            ])
            .arg(source)
            .arg("-o")
            .arg(object)
            .output()
            .context("failed to start clang for test init")?;
        ensure!(
            result.status.success(),
            "failed to compile test init {}: {}",
            source.display(),
            String::from_utf8_lossy(&result.stderr)
        );
    }
    let result = Command::new("rust-lld")
        .args(["-flavor", "gnu", "-static", "-e", "_start"])
        .arg(&entry_object)
        .arg(&init_object)
        .arg("-o")
        .arg(output)
        .output()
        .context("failed to start rust-lld for test init")?;
    ensure!(
        result.status.success(),
        "failed to link test init: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    Ok(())
}

pub(crate) fn host_initramfs_without_rootfs_drive(qemu: &QemuConfig) -> bool {
    crate::rootfs::qemu::host_initramfs_without_rootfs_drive(qemu)
}

fn copy_fixture(source: &Path, destination: &Path) -> anyhow::Result<()> {
    for entry in WalkDir::new(source).min_depth(1) {
        let entry = entry?;
        let relative = entry.path().strip_prefix(source)?;
        let target = destination.join(relative);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&target)?;
        } else if entry.file_type().is_symlink() {
            #[cfg(unix)]
            std::os::unix::fs::symlink(fs::read_link(entry.path())?, &target)?;
            #[cfg(not(unix))]
            anyhow::bail!("test initramfs symlinks require a Unix host");
        } else {
            fs::copy(entry.path(), &target)?;
            fs::set_permissions(&target, fs::metadata(entry.path())?.permissions())?;
        }
    }
    Ok(())
}
