use std::{fs, path::Path, process::Command};

use anyhow::{Context, ensure};
use serde::Deserialize;
use walkdir::WalkDir;

use super::QemuConfig;

#[derive(Deserialize)]
struct HostInitramfsFixture {
    source: Option<String>,
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
    let fixture: HostInitramfsFixture = toml::from_str(&fs::read_to_string(&manifest)?)
        .with_context(|| format!("failed to parse {}", manifest.display()))?;
    let Some(source_path) = &fixture.source else {
        ensure!(
            fixture.init_source.is_none(),
            "init_source requires a fixture source directory"
        );
        return Ok(());
    };
    ensure!(
        qemu.boot.initramfs.is_none(),
        "{} must not also set initramfs in qemu config",
        manifest.display()
    );
    let source = workspace_root.join(source_path);
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

/// Appends the map generated for the final target ELF to a prepared host
/// initramfs.  Newc archives may be concatenated and `ax-fs-ng` applies later
/// entries last, so this does not rebuild or mutate the fixture tree.
pub(crate) fn append_target_backtrace_map(
    target_dir: &Path,
    target: &str,
    debug: bool,
    qemu: &mut QemuConfig,
) -> anyhow::Result<()> {
    let profile = if debug { "debug" } else { "release" };
    let map = target_dir.join(target).join(profile).join("starryos.axbt");
    if !map.is_file() {
        return Ok(());
    }
    append_backtrace_map(qemu, &map)
}

/// Appends an AXBT sidecar to an already prepared initramfs.
pub(crate) fn append_backtrace_map(qemu: &mut QemuConfig, map: &Path) -> anyhow::Result<()> {
    append_backtrace_map_to_initramfs(&mut qemu.boot.initramfs, map)
}

/// Appends an AXBT sidecar to a boot payload that is not represented by QEMU.
/// U-Boot and board runners use the same initramfs contract as QEMU.
pub(crate) fn append_backtrace_map_to_initramfs(
    initramfs: &mut Option<String>,
    map: &Path,
) -> anyhow::Result<()> {
    if !map.is_file() {
        return Ok(());
    }
    if initramfs.is_none() {
        let stage = tempfile::tempdir_in(map.parent().context("AXBT map has no parent")?)?;
        let destination = stage.path().join("symbols/kernel.axbt");
        fs::create_dir_all(destination.parent().expect("map has a parent"))?;
        fs::copy(map, &destination)
            .with_context(|| format!("failed to stage target backtrace map {}", map.display()))?;
        let archive = map.with_extension("initramfs.cpio");
        crate::image::pack_initramfs_dir(stage.path(), &archive)?;
        *initramfs = Some(archive.to_string_lossy().into_owned());
        return Ok(());
    }
    let initramfs = initramfs.as_deref().expect("checked above");
    let initramfs = Path::new(initramfs);
    let stage = tempfile::tempdir_in(
        initramfs
            .parent()
            .context("host initramfs has no parent directory")?,
    )?;
    let destination = stage.path().join("symbols/kernel.axbt");
    fs::create_dir_all(destination.parent().expect("map has a parent"))?;
    fs::copy(map, &destination)
        .with_context(|| format!("failed to stage target backtrace map {}", map.display()))?;
    let map_archive = initramfs.with_extension("symbols.cpio");
    crate::image::pack_initramfs_dir(stage.path(), &map_archive)?;
    let mut output = fs::OpenOptions::new().append(true).open(initramfs)?;
    let bytes = fs::read(&map_archive)?;
    std::io::Write::write_all(&mut output, &bytes)?;
    output.sync_all()?;
    let _ = fs::remove_file(map_archive);
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
