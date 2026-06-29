use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::Context;

use super::{super::rootfs, types::StarryAppCase};
use crate::{rootfs::inject, support::process::ProcessExt};

pub(super) async fn prepare_qemu_app_rootfs(
    workspace_root: &Path,
    app: &StarryAppCase,
    arch: &str,
    target: &str,
    configured_rootfs: Option<&Path>,
) -> anyhow::Result<PathBuf> {
    let rootfs_path = match configured_rootfs {
        Some(path) => path.to_path_buf(),
        None => crate::image::storage::default_rootfs_path(workspace_root, arch)?,
    };
    if app.prebuild_path.is_none() {
        if let Some(configured) = configured_rootfs {
            crate::image::storage::ensure_optional_managed_rootfs(
                workspace_root,
                arch,
                Some(configured),
            )
            .await?;
            rootfs::ensure_apk_region_in_rootfs(configured)?;
            return Ok(configured.to_path_buf());
        }
        return rootfs::ensure_rootfs_in_tmp_dir(workspace_root, arch, target).await;
    }

    let default_rootfs = rootfs::ensure_rootfs_in_tmp_dir(workspace_root, arch, target).await?;
    if let Some(parent) = rootfs_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    if !rootfs_path.exists() {
        fs::copy(&default_rootfs, &rootfs_path).with_context(|| {
            format!(
                "failed to copy default rootfs {} to {}",
                default_rootfs.display(),
                rootfs_path.display()
            )
        })?;
    }

    let layout_root = workspace_root
        .join("tmp/axbuild/starry-app")
        .join(&app.name);
    let staging_root = layout_root.join("staging-root");
    let overlay_dir = layout_root.join("overlay");

    let prepare_result = (|| -> anyhow::Result<()> {
        reset_dir(&staging_root)?;
        reset_dir(&overlay_dir)?;

        if let Some(prebuild_path) = app.prebuild_path.as_deref() {
            let mut command = Command::new("bash");
            command
                .arg(prebuild_path)
                .current_dir(&app.case_dir)
                .env("STARRY_APP_NAME", &app.name)
                .env("STARRY_APP_DIR", &app.case_dir)
                .env("STARRY_WORKSPACE", workspace_root)
                .env("STARRY_ARCH", arch)
                .env("STARRY_ROOTFS", &rootfs_path)
                .env("STARRY_STAGING_ROOT", &staging_root)
                .env("STARRY_OVERLAY_DIR", &overlay_dir);
            command
                .exec()
                .with_context(|| format!("failed to run {}", prebuild_path.display()))?;
        }

        inject::inject_overlay(&rootfs_path, &overlay_dir)
    })();
    prepare_result?;
    Ok(rootfs_path)
}

fn reset_dir(path: &Path) -> anyhow::Result<()> {
    if path.exists() {
        fs::remove_dir_all(path).with_context(|| format!("failed to remove {}", path.display()))?;
    }
    fs::create_dir_all(path).with_context(|| format!("failed to create {}", path.display()))
}
