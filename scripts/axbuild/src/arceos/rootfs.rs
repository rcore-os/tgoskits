//! ArceOS-specific rootfs helpers for QEMU runs.
//!
//! Main responsibilities:
//! - Resolve explicit ArceOS rootfs CLI values into concrete image paths
//! - Ensure managed rootfs images are available before launch
//! - Patch QEMU configs so an explicit rootfs image is attached correctly

use std::path::{Path, PathBuf};

use ostool::run::qemu::QemuConfig;

/// Resolves an explicit ArceOS rootfs CLI value into a concrete path.
pub(crate) fn resolve_explicit_rootfs(
    workspace_root: &Path,
    arch: &str,
    rootfs: PathBuf,
) -> PathBuf {
    crate::rootfs::store::resolve_rootfs_path(workspace_root, arch, rootfs)
}

/// Ensures a managed ArceOS rootfs image is available before launch.
pub(crate) async fn ensure_rootfs_ready(
    workspace_root: &Path,
    arch: &str,
    rootfs: &Path,
) -> anyhow::Result<()> {
    crate::rootfs::store::ensure_managed_rootfs(workspace_root, arch, rootfs).await
}

/// Patches a QEMU config so it boots with the selected ArceOS rootfs image.
pub(crate) fn patch_qemu_rootfs(qemu: &mut QemuConfig, rootfs: &Path) {
    crate::rootfs::qemu::patch_rootfs(
        qemu,
        rootfs,
        crate::rootfs::qemu::RootfsPatchMode::EnsureDiskBootNet,
    );
}
