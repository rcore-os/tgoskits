//! Prepare the default Alpine boot environment before QEMU starts.

use std::{collections::BTreeMap, fs, path::Path, process::Command};

use anyhow::{Context, ensure};
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use super::apk;
use crate::{
    rootfs::inject,
    support::process::{ProcessExt, find_host_binary_candidates},
    test::{build::qemu_user_binary_names, case::copy_file_fast},
};

const MARKER: &str = "/etc/starry-openrc-assets";
const DATABASE: &str = "lib/apk/db/installed";

/// The caller holds the managed image lock. Publish only a fully prepared copy.
pub(super) fn prepare(workspace: &Path, arch: &str, image: &Path) -> anyhow::Result<()> {
    let assets = workspace.join("os/StarryOS/starryos/rootfs");
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
    let after = prepare_packages(image, temporary.path(), &overlay, arch)?;
    copy_tree(&assets, &overlay)?;
    fs::create_dir_all(overlay.join("run"))?;
    for level in ["boot", "shutdown"] {
        fs::create_dir_all(overlay.join("etc/runlevels").join(level))?;
    }
    // Package post-install scripts are disabled on the host. Install the required
    // BusyBox applet links explicitly without executing guest init scripts.
    for name in ["init", "reboot", "poweroff", "halt"] {
        let link = overlay.join("sbin").join(name);
        fs::create_dir_all(link.parent().context("applet has no parent")?)?;
        if link.symlink_metadata().is_ok() {
            fs::remove_file(&link)?;
        }
        std::os::unix::fs::symlink("/bin/busybox", link)?;
    }
    fs::write(overlay.join("etc/starry-openrc-packages"), &after)?;
    fs::write(overlay.join(MARKER.trim_start_matches('/')), version)?;
    let candidate = temporary.path().join("rootfs.img");
    copy_file_fast(image, &candidate)?;
    inject::inject_overlay(&candidate, &overlay)?;
    fs::rename(candidate, image).context("failed to publish OpenRC rootfs")
}

fn prepare_packages(
    image: &Path,
    temporary: &Path,
    overlay: &Path,
    arch: &str,
) -> anyhow::Result<String> {
    let installed = inject::read_text_file(image, "/lib/apk/db/installed")?
        .context("Alpine image has no installed package database")?;
    if package_records(&installed)
        .get("openrc")
        .is_some_and(|record| record.lines().any(|line| line == "V:0.63-r1"))
    {
        return Ok(installed);
    }
    let staging = temporary.join("staging");
    fs::create_dir(&staging)?;
    inject::extract_rootfs(image, &staging)?;
    ensure!(
        staging.join("etc/alpine-release").is_file(),
        "OpenRC preparation requires Alpine"
    );
    super::resolver::write_host_resolver_config(&staging)?;
    apk::rewrite_apk_repositories_for_region(&staging, apk::apk_region_from_env()?)?;
    let before = fs::read_to_string(staging.join(DATABASE))?;
    let runner = find_host_binary_candidates(qemu_user_binary_names(arch)?)?;
    let cache = image
        .parent()
        .context("image has no parent")?
        .join(format!("openrc-apk-{arch}"));
    fs::create_dir_all(&cache)?;
    Command::new(runner)
        .arg("-L")
        .arg(&staging)
        .arg(staging.join("sbin/apk"))
        .arg("--root")
        .arg(&staging)
        .arg("--repositories-file")
        .arg(staging.join("etc/apk/repositories"))
        .arg("--keys-dir")
        .arg(staging.join("etc/apk/keys"))
        .arg("--cache-dir")
        .arg(&cache)
        .args([
            "--update-cache",
            "--timeout",
            "60",
            "--no-interactive",
            "--force-no-chroot",
            "--scripts=no",
            "add",
            "openrc=0.63-r1",
        ])
        .env("QEMU_LD_PREFIX", &staging)
        .env(
            "LD_LIBRARY_PATH",
            format!(
                "{}:{}",
                staging.join("lib").display(),
                staging.join("usr/lib").display()
            ),
        )
        .exec()
        .context("failed to install OpenRC in staging rootfs")?;
    let after = fs::read_to_string(staging.join(DATABASE))?;
    copy_changed_packages(&staging, overlay, &before, &after)?;
    for path in [DATABASE, "etc/apk/world", "etc/apk/repositories"] {
        copy_entry(&staging, overlay, Path::new(path))?;
    }
    Ok(after)
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

fn copy_changed_packages(
    staging: &Path,
    overlay: &Path,
    before: &str,
    after: &str,
) -> anyhow::Result<()> {
    let previous = package_records(before);
    for (name, record) in package_records(after) {
        if previous.get(name).copied() == Some(record) {
            continue;
        }
        let mut directory = Path::new("");
        for line in record.lines() {
            if let Some(path) = line.strip_prefix("F:") {
                directory = Path::new(path);
                copy_entry(staging, overlay, directory)?;
            } else if let Some(file) = line.strip_prefix("R:") {
                copy_entry(staging, overlay, &directory.join(file))?;
            }
        }
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_delta_copies_changed_payload_without_unrelated_files() {
        let tmp = tempfile::tempdir().unwrap();
        let staging = tmp.path().join("staging");
        let overlay = tmp.path().join("overlay");
        fs::create_dir_all(staging.join("sbin")).unwrap();
        fs::write(staging.join("sbin/openrc"), b"new executable").unwrap();
        fs::write(staging.join("sbin/unrelated"), b"keep base version").unwrap();
        std::os::unix::fs::symlink("openrc", staging.join("sbin/rc-status")).unwrap();
        let before = "P:base\nV:1\nF:sbin\nR:unrelated\n\n";
        let after = format!("{before}P:openrc\nV:1\nF:sbin\nR:openrc\nR:rc-status\n\n");
        copy_changed_packages(&staging, &overlay, before, &after).unwrap();
        assert_eq!(
            fs::read(overlay.join("sbin/openrc")).unwrap(),
            b"new executable"
        );
        assert_eq!(
            fs::read_link(overlay.join("sbin/rc-status")).unwrap(),
            Path::new("openrc")
        );
        assert!(!overlay.join("sbin/unrelated").exists());
        assert!(copy_entry(&staging, &overlay, Path::new("../escape")).is_err());
    }
}
