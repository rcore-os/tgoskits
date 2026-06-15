//! Compatibility wrappers for managed rootfs image helpers.
//!
//! The implementation lives in `crate::image::storage`; this module keeps the
//! existing ArceOS/Starry/Axvisor call sites on the shared image manager.

use std::path::{Path, PathBuf};

/// Returns the local storage directory used for image-managed files.
pub(crate) fn rootfs_dir(workspace_root: &Path) -> PathBuf {
    crate::image::storage::rootfs_dir(workspace_root)
        .unwrap_or_else(|_| crate::context::axbuild_tmp_dir(workspace_root).join("rootfs"))
}

/// Resolves a user-facing rootfs argument into the image storage path.
pub(crate) fn resolve_rootfs_path(workspace_root: &Path, arch: &str, rootfs: PathBuf) -> PathBuf {
    crate::image::storage::resolve_rootfs_path(workspace_root, arch, rootfs.clone())
        .unwrap_or(rootfs)
}

/// Resolves an explicit `--rootfs` CLI value into a concrete image path.
pub(crate) fn resolve_explicit_rootfs(
    workspace_root: &Path,
    arch: &str,
    rootfs: PathBuf,
) -> PathBuf {
    crate::image::storage::resolve_explicit_rootfs(workspace_root, arch, rootfs.clone())
        .unwrap_or(rootfs)
}

/// Returns the default managed rootfs path for an architecture.
pub(crate) fn default_rootfs_path(workspace_root: &Path, arch: &str) -> anyhow::Result<PathBuf> {
    crate::image::storage::default_rootfs_path(workspace_root, arch)
}

/// Ensures a managed rootfs path exists locally before it is used.
pub(crate) async fn ensure_managed_rootfs(
    workspace_root: &Path,
    arch: &str,
    path: &Path,
) -> anyhow::Result<()> {
    crate::image::storage::ensure_managed_rootfs(workspace_root, arch, path).await
}

/// Ensures an optional managed rootfs path exists locally before it is used.
pub(crate) async fn ensure_optional_managed_rootfs(
    workspace_root: &Path,
    arch: &str,
    path: Option<&Path>,
) -> anyhow::Result<()> {
    crate::image::storage::ensure_optional_managed_rootfs(workspace_root, arch, path).await
}

/// Ensures the default managed rootfs image for an architecture is available.
pub(crate) async fn ensure_rootfs_for_arch(
    workspace_root: &Path,
    arch: &str,
) -> anyhow::Result<PathBuf> {
    crate::image::storage::ensure_rootfs_for_arch(workspace_root, arch).await
}
