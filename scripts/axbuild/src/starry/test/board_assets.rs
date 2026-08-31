use std::path::{Path, PathBuf};

use anyhow::{Context, ensure};

use crate::{
    starry::{
        board_assets::{BoardSessionAssetPlan, PreparedBoardSessionAssets},
        test::starry_case_asset_config,
    },
    test::{build::prepare_c_case_overlay_sync, case::reset_dir},
};

const C_SOURCE_DIR: &str = "c";
const CMAKE_PROJECT_FILE: &str = "CMakeLists.txt";

pub(crate) async fn prepare_board_session_assets(
    workspace_root: &Path,
    arch: &str,
    target: &str,
    case_name: &str,
    case_dir: &Path,
    board_config_path: &Path,
    declared_session_files: &[PathBuf],
) -> anyhow::Result<Option<PreparedBoardSessionAssets>> {
    let session_environment =
        crate::starry::session_env::SessionEnvConfig::load(case_dir, board_config_path)?
            .map(|config| config.capture_environment())
            .transpose()?
            .unwrap_or_default();
    let cmake_project = case_dir.join(C_SOURCE_DIR).join(CMAKE_PROJECT_FILE);
    if !cmake_project.is_file()
        && !session_environment.is_bootstrap()
        && declared_session_files.is_empty()
    {
        return Ok(None);
    }
    for unsupported_dir in ["sh", "python"] {
        ensure!(
            !case_dir.join(unsupported_dir).exists(),
            "board case `{case_name}` combines C assets with unsupported `{unsupported_dir}` \
             assets"
        );
    }

    let rootfs = if cmake_project.is_file() {
        Some(crate::starry::rootfs::ensure_rootfs_in_tmp_dir(workspace_root, arch, target).await?)
    } else {
        None
    };
    let workspace_root = workspace_root.to_path_buf();
    let arch = arch.to_string();
    let target = target.to_string();
    let case_name = case_name.to_string();
    let case_dir = case_dir.to_path_buf();
    let board_config_path = board_config_path.to_path_buf();
    let declared_session_files = declared_session_files.to_vec();
    let plan = BoardSessionAssetPlan {
        workspace_root,
        target,
        work_name: case_name.clone(),
        case_name,
        case_dir,
        board_config_path,
        declared_session_files,
        session_environment,
    };

    let assets = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        plan.prepare(|case, layout| {
            if let Some(rootfs) = &rootfs {
                prepare_c_case_overlay_sync(
                    &arch,
                    case,
                    rootfs,
                    layout,
                    &starry_case_asset_config(),
                )
            } else {
                reset_dir(&layout.overlay_dir)
            }
        })
    })
    .await
    .context("Starry board session asset task failed")??;
    Ok(Some(assets))
}
