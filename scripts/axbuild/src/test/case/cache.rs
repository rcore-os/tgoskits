use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::Context;

use super::{
    hash::case_asset_cache_key,
    layout::next_case_run_id,
    types::{CaseAssetConfig, CaseAssetLayout, CasePipeline, TestQemuCase},
};

pub(super) fn rootfs_cache_image_path(
    layout: &CaseAssetLayout,
    arch: &str,
    target: &str,
    pipeline: CasePipeline,
    case: &TestQemuCase,
    shared_rootfs: &Path,
    config: &CaseAssetConfig,
) -> anyhow::Result<PathBuf> {
    let key = case_asset_cache_key(arch, target, pipeline, case, shared_rootfs, config)?;
    Ok(layout.rootfs_cache_dir.join(format!("{key}.img")))
}

fn rootfs_cache_write_enabled() -> bool {
    if std::env::var_os("AXBUILD_DISABLE_ROOTFS_CACHE").is_some() {
        return false;
    }
    std::env::var_os("CI").is_none()
}

fn is_no_space_left(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .and_then(|e| e.raw_os_error())
            == Some(28)
    })
}

pub(super) fn save_rootfs_cache_image(src: &Path, dst: &Path) -> anyhow::Result<()> {
    if !rootfs_cache_write_enabled() {
        return Ok(());
    }
    let parent = dst
        .parent()
        .ok_or_else(|| anyhow::anyhow!("rootfs cache path has no parent: {}", dst.display()))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("failed to create rootfs cache dir {}", parent.display()))?;
    let temp = parent.join(format!(
        ".{}.{}.tmp",
        dst.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("rootfs-cache"),
        next_case_run_id()
    ));
    if let Err(err) = copy_file_fast(src, &temp) {
        let _ = fs::remove_file(&temp);
        if is_no_space_left(&err) {
            return Ok(());
        }
        return Err(err);
    }
    match fs::rename(&temp, dst) {
        Ok(()) => Ok(()),
        Err(err) if dst.is_file() => {
            let _ = fs::remove_file(&temp);
            if is_valid_rootfs_cache_image(dst) {
                Ok(())
            } else {
                Err(err).with_context(|| {
                    format!("failed to install rootfs cache image {}", dst.display())
                })
            }
        }
        Err(err) => {
            let _ = fs::remove_file(&temp);
            Err(err)
                .with_context(|| format!("failed to install rootfs cache image {}", dst.display()))
        }
    }
}

/// Returns `true` when a rootfs cache image file exists and has a plausible
/// size. Files smaller than 1 MiB are treated as corrupt/incomplete and will
/// trigger a cache-miss rebuild.
pub(super) fn is_valid_rootfs_cache_image(path: &Path) -> bool {
    const MIN_SIZE: u64 = 1024 * 1024;
    path.is_file()
        && path
            .metadata()
            .map(|m| m.len() >= MIN_SIZE)
            .unwrap_or(false)
}

/// Copies `src` to `dst`, preferring a copy-on-write reflink when the
/// filesystem supports it so that ~1 GiB rootfs images are duplicated in
/// near-zero time on btrfs / XFS.
///
/// On Linux this delegates to `cp --reflink=auto` and falls back to a regular
/// `fs::copy` if that fails (e.g. on ext4 or when `cp` is too old). On other
/// platforms only `fs::copy` is used.
pub(super) fn copy_file_fast(src: &Path, dst: &Path) -> anyhow::Result<()> {
    #[cfg(target_os = "linux")]
    {
        // `cp --reflink=auto` tries FICLONE ioctl first; if the filesystem
        // does not support it the command falls back to a regular copy, so
        // this is always safe to try.
        let status = std::process::Command::new("cp")
            .arg("--reflink=auto")
            .arg(src)
            .arg(dst)
            .status();
        if let Ok(status) = status {
            if status.success() {
                return Ok(());
            }
            // cp reported an error -- remove any partial destination file so
            // the fs::copy fallback below starts with a clean slate.
            let _ = fs::remove_file(dst);
        }
        // Fall through to regular copy on any failure (cp not available,
        // unsupported flag, or a non-CoW error we cannot distinguish).
    }
    fs::copy(src, dst)
        .with_context(|| format!("failed to copy {} to {}", src.display(), dst.display()))?;
    Ok(())
}
