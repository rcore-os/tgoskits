use std::{fs, io::Write, path::Path, process::Command};

use anyhow::{Context, ensure};
use ostool::BootPayloadConfig;
use serde::Deserialize;
use tempfile::NamedTempFile;
use walkdir::WalkDir;

use super::QemuConfig;
use crate::image::{InitramfsBuilder, InitramfsCompression};

#[derive(Deserialize)]
struct HostInitramfsFixture {
    source: Option<String>,
    #[serde(default)]
    init_source: Option<String>,
}

/// Builds a test archive before its boot payload is handed to a runner.
///
/// QEMU direct boot, U-Boot FIT and the board runners all carry the host
/// archive through ostool's [`BootPayloadConfig`], so the case fixture is
/// packed against that payload instead of one runner's configuration type.
pub(crate) fn prepare_host_initramfs_payload(
    workspace_root: &Path,
    target_dir: &Path,
    case_dir: &Path,
    arch: &str,
    boot: &mut BootPayloadConfig,
) -> anyhow::Result<bool> {
    let manifest = case_dir.join("host-initramfs.toml");
    if !manifest.is_file() {
        return Ok(false);
    }
    let fixture: HostInitramfsFixture = toml::from_str(&fs::read_to_string(&manifest)?)
        .with_context(|| format!("failed to parse {}", manifest.display()))?;
    let Some(source_path) = &fixture.source else {
        ensure!(
            fixture.init_source.is_none(),
            "init_source requires a fixture source directory"
        );
        return Ok(false);
    };
    ensure!(
        boot.initramfs.is_none(),
        "{} must not also set an initramfs in the run configuration",
        manifest.display()
    );
    // Fixtures may live in the build target directory, which a caller can move
    // with `--target-dir`; the shared resolution keeps the declared
    // `${workspace}/target/axbuild` paths pointing at the real target dir.
    let source = crate::context::resolve_axbuild_artifact_path(
        workspace_root,
        target_dir,
        &workspace_root.join(source_path),
    );
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
    boot.initramfs = Some(output.to_string_lossy().into_owned());
    Ok(true)
}

/// Builds the host archive fixture of one QEMU case.
///
/// Kept as the QEMU entry for the existing QEMU and StarryOS test runners, so
/// they keep mutating the QEMU configuration they already own.
pub(crate) fn prepare_host_initramfs(
    workspace_root: &Path,
    target_dir: &Path,
    case_dir: &Path,
    arch: &str,
    qemu: &mut QemuConfig,
) -> anyhow::Result<bool> {
    prepare_host_initramfs_payload(workspace_root, target_dir, case_dir, arch, &mut qemu.boot)
}

/// Prepares an initramfs with the symbol maps generated for the final target ELF.
/// The caller must retain the returned file until the runner finishes.
pub(crate) fn prepare_target_initramfs_with_symbols(
    target_dir: &Path,
    target: &str,
    debug: bool,
    qemu: &QemuConfig,
) -> anyhow::Result<Option<NamedTempFile>> {
    let profile = if debug { "debug" } else { "release" };
    let map = target_dir.join(target).join(profile).join("starryos.axbt");
    prepare_qemu_initramfs_with_symbols(qemu, &map, target)
}

/// Prepares symbol maps for QEMU loaders that support a host initramfs.
/// The caller must retain the returned file until QEMU finishes.
pub(crate) fn prepare_qemu_initramfs_with_symbols(
    qemu: &QemuConfig,
    map: &Path,
    target: &str,
) -> anyhow::Result<Option<NamedTempFile>> {
    if target.starts_with("x86_64") && !qemu.uefi {
        // ostool cannot hand a host initramfs to the direct x86_64 loader.
        log::debug!("skip target symbol maps for direct x86_64 QEMU; host initramfs requires UEFI");
        return Ok(None);
    }
    prepare_initramfs_with_symbols(qemu.boot.initramfs.as_deref().map(Path::new), map)
}

/// Builds a temporary boot payload from a read-only input and one symbol archive.
/// Returns `None` when neither AXBT nor AXKS exists. With no input, the payload
/// contains only symbols. The caller must set the boot path and retain the file
/// until the runner finishes; dropping the file removes the generated payload.
pub(crate) fn prepare_initramfs_with_symbols(
    input: Option<&Path>,
    map: &Path,
) -> anyhow::Result<Option<NamedTempFile>> {
    let kernel_symbols = map.with_extension("axks");
    if !map.is_file() && !kernel_symbols.is_file() {
        return Ok(None);
    }
    let mut symbols = InitramfsBuilder::new();
    symbols.add_directory("symbols", 0o755)?;
    for (source, guest_path) in [
        (map, "symbols/kernel.axbt"),
        (kernel_symbols.as_path(), "symbols/kernel.axks"),
    ] {
        if source.is_file() {
            let contents = fs::read(source).with_context(|| {
                format!("failed to read target symbol map {}", source.display())
            })?;
            symbols.add_file(guest_path, &contents, 0o644)?;
        }
    }
    let mut output = tempfile::Builder::new()
        .prefix("boot-initramfs-")
        .suffix(".cpio")
        .tempfile_in(map.parent().context("map has no parent")?)?;
    let input_len = match input {
        Some(input) => {
            let mut source = fs::File::open(input)
                .with_context(|| format!("failed to open input initramfs {}", input.display()))?;
            std::io::copy(&mut source, &mut output)
                .with_context(|| format!("failed to copy input initramfs {}", input.display()))?
        }
        None => 0,
    };
    // A preceding compressed member need not end on a newc alignment boundary.
    let padding = ((4 - input_len % 4) % 4) as usize;
    output.write_all(&[0; 3][..padding])?;
    output.write_all(&symbols.build(InitramfsCompression::None)?)?;
    output.as_file().sync_all()?;
    println!("host initramfs: {}", output.path().display());
    Ok(Some(output))
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

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn appending_map_aligns_after_compressed_bundle_member() {
        let root = tempdir().unwrap();
        let initramfs = root.path().join("host.cpio");
        fs::write(&initramfs, b"gzip-member").unwrap();
        let map = root.path().join("kernel.axbt");
        fs::write(&map, b"AXBT").unwrap();
        let archive = prepare_initramfs_with_symbols(Some(&initramfs), &map)
            .unwrap()
            .unwrap();

        let bytes = fs::read(archive.path()).unwrap();
        let map_offset = bytes
            .windows(6)
            .position(|window| window == b"070701")
            .unwrap();
        assert_eq!(map_offset % 4, 0);
        assert_eq!(&bytes["gzip-member".len()..map_offset], b"\0");
    }
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
