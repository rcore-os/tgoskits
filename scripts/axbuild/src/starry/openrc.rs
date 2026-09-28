//! Prepare the default Alpine boot environment before QEMU starts.

use std::{collections::BTreeMap, fs, path::Path};

use anyhow::{Context, ensure};
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use crate::{rootfs::inject, test::case::copy_file_fast};

const MARKER: &str = "/etc/starry-openrc-assets";
const REQUIRED_PACKAGES: &[(&str, &str)] = &[
    ("busybox", "1.37.0-r30"),
    ("openrc", "0.63-r1"),
    ("openrc-user", "0.63-r1"),
    ("bridge", "1.5-r5"),
    ("ifupdown-ng", "0.12.1-r7"),
    ("libcap2", "2.78-r0"),
];

/// The caller holds the managed image lock. Publish only a fully prepared copy.
pub(super) fn prepare(workspace: &Path, image: &Path) -> anyhow::Result<()> {
    let assets = workspace.join("os/StarryOS/starryos/rootfs");
    let installed = inject::read_text_file(image, "/lib/apk/db/installed")?
        .context("prebuilt Alpine image has no installed package database")?;
    check_preinstalled_packages(&installed)?;
    let mut digest = Sha256::new();
    digest.update(include_bytes!("openrc.rs"));
    for entry in WalkDir::new(&assets).sort_by_file_name() {
        let entry = entry?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            digest.update(entry.metadata()?.permissions().mode().to_le_bytes());
        }
        digest.update(
            entry
                .path()
                .strip_prefix(&assets)?
                .as_os_str()
                .as_encoded_bytes(),
        );
        if entry.file_type().is_file() {
            digest.update(fs::read(entry.path())?);
        } else if entry.file_type().is_symlink() {
            digest.update(fs::read_link(entry.path())?.as_os_str().as_encoded_bytes());
        }
    }
    let version = format!("{:x}\n", digest.finalize());
    if inject::read_text_file(image, MARKER)?.as_deref() == Some(&version) {
        return Ok(());
    }
    let parent = image.parent().context("managed image has no parent")?;
    let temporary = tempfile::tempdir_in(parent)?;
    let overlay = temporary.path().join("overlay");
    fs::create_dir(&overlay)?;
    copy_tree(&assets, &overlay)?;
    fs::create_dir_all(overlay.join("run"))?;
    for level in ["boot", "shutdown"] {
        fs::create_dir_all(overlay.join("etc/runlevels").join(level))?;
    }
    // The prebuilt image installs packages without guest scripts. Supply the
    // BusyBox applet links needed by init and the power-control commands.
    for name in ["init", "reboot", "poweroff", "halt"] {
        let link = overlay.join("sbin").join(name);
        fs::create_dir_all(link.parent().context("applet has no parent")?)?;
        if link.symlink_metadata().is_ok() {
            fs::remove_file(&link)?;
        }
        std::os::unix::fs::symlink("/bin/busybox", link)?;
    }
    fs::write(overlay.join("etc/starry-openrc-packages"), installed)?;
    fs::write(overlay.join(MARKER.trim_start_matches('/')), version)?;
    let candidate = temporary.path().join("rootfs.img");
    copy_file_fast(image, &candidate)?;
    inject::inject_overlay(&candidate, &overlay)?;
    fs::rename(candidate, image).context("failed to publish OpenRC rootfs")
}

fn check_preinstalled_packages(database: &str) -> anyhow::Result<()> {
    let installed = package_records(database);
    for (name, version) in REQUIRED_PACKAGES {
        ensure!(
            installed
                .get(name)
                .is_some_and(|record| record.lines().any(|line| line == format!("V:{version}"))),
            "prebuilt Alpine rootfs must contain {name}={version}; use tgosimages v0.0.14"
        );
    }
    Ok(())
}

fn package_records(database: &str) -> BTreeMap<&str, &str> {
    database
        .split("\n\n")
        .filter_map(|record| {
            let name = record.lines().find_map(|line| line.strip_prefix("P:"))?;
            Some((name, record))
        })
        .collect()
}

fn copy_tree(source: &Path, target: &Path) -> anyhow::Result<()> {
    for entry in WalkDir::new(source).min_depth(1) {
        let entry = entry?;
        copy_entry(source, target, entry.path().strip_prefix(source)?)?;
    }
    Ok(())
}

fn copy_entry(source: &Path, target: &Path, relative: &Path) -> anyhow::Result<()> {
    ensure!(
        !relative.is_absolute()
            && !relative
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir)),
        "invalid package path: {}",
        relative.display()
    );
    let src = source.join(relative);
    let dst = target.join(relative);
    let metadata = fs::symlink_metadata(&src)
        .with_context(|| format!("missing package file {}", src.display()))?;
    if metadata.is_dir() {
        fs::create_dir_all(&dst)?;
    } else {
        fs::create_dir_all(dst.parent().context("overlay file has no parent")?)?;
        if dst.symlink_metadata().is_ok() {
            fs::remove_file(&dst)?;
        }
        if metadata.is_symlink() {
            std::os::unix::fs::symlink(fs::read_link(&src)?, &dst)?;
        } else {
            fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}
