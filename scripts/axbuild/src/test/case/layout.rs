use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::Context;

use super::types::{
    CASE_APK_CACHE_DIR_NAME, CASE_BUILD_DIR_NAME, CASE_CACHE_DIR_NAME,
    CASE_CMAKE_TOOLCHAIN_FILE_NAME, CASE_COMMAND_WRAPPER_DIR_NAME, CASE_CROSS_BIN_DIR_NAME,
    CASE_OVERLAY_DIR_NAME, CASE_ROOTFS_CACHE_DIR_NAME, CASE_ROOTFS_COPY_NAME, CASE_RUNS_DIR_NAME,
    CASE_STAGING_DIR_NAME, CASE_WORK_ROOT_NAME, CaseAssetLayout,
};

static CASE_RUN_ID: AtomicU64 = AtomicU64::new(0);

/// Resolves the workspace target directory used for a test build target.
pub(crate) fn resolve_target_dir(workspace_root: &Path, target: &str) -> anyhow::Result<PathBuf> {
    Ok(workspace_root.join("target").join(target))
}

/// Builds the working directory layout used for a QEMU case asset run.
pub(crate) fn case_asset_layout(
    workspace_root: &Path,
    target: &str,
    case_name: &str,
) -> anyhow::Result<CaseAssetLayout> {
    let target_dir = resolve_target_dir(workspace_root, target)?;
    let work_dir = target_dir.join(CASE_WORK_ROOT_NAME).join(case_name);
    let run_dir = work_dir.join(CASE_RUNS_DIR_NAME).join(next_case_run_id());
    let cache_dir = work_dir.join(CASE_CACHE_DIR_NAME);

    Ok(CaseAssetLayout {
        staging_root: run_dir.join(CASE_STAGING_DIR_NAME),
        build_dir: run_dir.join(CASE_BUILD_DIR_NAME),
        overlay_dir: run_dir.join(CASE_OVERLAY_DIR_NAME),
        command_wrapper_dir: run_dir.join(CASE_COMMAND_WRAPPER_DIR_NAME),
        cross_bin_dir: run_dir.join(CASE_CROSS_BIN_DIR_NAME),
        cmake_toolchain_file: run_dir.join(CASE_CMAKE_TOOLCHAIN_FILE_NAME),
        apk_cache_dir: cache_dir.join(CASE_APK_CACHE_DIR_NAME),
        rootfs_cache_dir: cache_dir.join(CASE_ROOTFS_CACHE_DIR_NAME),
        case_rootfs_copy: run_dir.join(CASE_ROOTFS_COPY_NAME),
        cache_dir,
        run_dir,
        work_dir,
    })
}

pub(super) fn next_case_run_id() -> String {
    let sequence = CASE_RUN_ID.fetch_add(1, Ordering::Relaxed);
    format!("{}-{sequence}", std::process::id())
}

/// Resets a directory to an empty existing state.
pub(crate) fn reset_dir(path: &Path) -> anyhow::Result<()> {
    if path.exists() {
        fs::remove_dir_all(path).with_context(|| format!("failed to remove {}", path.display()))?;
    }
    fs::create_dir_all(path).with_context(|| format!("failed to create {}", path.display()))
}
