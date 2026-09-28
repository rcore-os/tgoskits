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
        let result = Command::new("clang")
            .arg("--target=aarch64-unknown-linux-musl")
            .args([
                "-fuse-ld=lld",
                "-ffreestanding",
                "-fno-builtin",
                "-fno-stack-protector",
                "-nostdlib",
                "-static",
                "-Wl,-e,_start",
            ])
            .arg(&entry_source)
            .arg(&init_source)
            .arg("-o")
            .arg(staging.path().join("init"))
            .output()
            .context("failed to start clang for test init")?;
        ensure!(
            result.status.success(),
            "failed to build test init: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }

    crate::image::pack_initramfs_dir(staging.path(), &output)?;
    qemu.boot.initramfs = Some(output.to_string_lossy().into_owned());
    Ok(())
}

pub(crate) fn diskless_host_initramfs(qemu: &QemuConfig) -> bool {
    qemu.boot.initramfs.is_some() && !qemu.args.iter().any(|arg| arg == "-drive")
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
