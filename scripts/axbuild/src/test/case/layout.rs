use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::Context;

use super::types::{
    BOARD_CASE_UPLOAD_DIR_NAME, BOARD_CASE_WORK_ROOT_NAME, CASE_APK_CACHE_DIR_NAME,
    CASE_BUILD_DIR_NAME, CASE_CACHE_DIR_NAME, CASE_CMAKE_TOOLCHAIN_FILE_NAME,
    CASE_COMMAND_WRAPPER_DIR_NAME, CASE_CROSS_BIN_DIR_NAME, CASE_OVERLAY_DIR_NAME,
    CASE_ROOTFS_CACHE_DIR_NAME, CASE_ROOTFS_COPY_NAME, CASE_RUNS_DIR_NAME, CASE_STAGING_DIR_NAME,
    CASE_WORK_ROOT_NAME, CaseAssetLayout,
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
    asset_layout(workspace_root, target, CASE_WORK_ROOT_NAME, case_name)
}

/// Builds the working directory layout used for a board case asset run.
pub(crate) fn board_case_asset_layout(
    workspace_root: &Path,
    target: &str,
    case_name: &str,
) -> anyhow::Result<CaseAssetLayout> {
    let mut layout = asset_layout(workspace_root, target, BOARD_CASE_WORK_ROOT_NAME, case_name)?;
    layout.overlay_dir = layout.run_dir.join(BOARD_CASE_UPLOAD_DIR_NAME);
    Ok(layout)
}

fn asset_layout(
    workspace_root: &Path,
    target: &str,
    work_root_name: &str,
    case_name: &str,
) -> anyhow::Result<CaseAssetLayout> {
    let target_dir = resolve_target_dir(workspace_root, target)?;
    let work_dir = target_dir.join(work_root_name).join(case_name);
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

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::board_case_asset_layout;

    #[test]
    fn board_layout_places_upload_root_under_target_board_cases() {
        let root = tempdir().unwrap();

        let layout =
            board_case_asset_layout(root.path(), "riscv64gc-unknown-none-elf", "usb/init").unwrap();

        assert!(
            layout.overlay_dir.starts_with(
                root.path()
                    .join("target/riscv64gc-unknown-none-elf/board-cases/usb/init/runs")
            )
        );
        assert_eq!(
            layout.overlay_dir.file_name().unwrap().to_string_lossy(),
            "upload"
        );
    }
}
