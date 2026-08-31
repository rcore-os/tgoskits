use std::path::{Path, PathBuf};

use anyhow::Context;

use super::StarryAppBoardCase;
use crate::{
    starry::{
        board_assets::{BoardSessionAssetPlan, PreparedBoardSessionAssets},
        test::starry_case_asset_config,
    },
    test::{
        build::{prepare_c_case_overlay_sync, prepare_rust_case_overlay_sync},
        case::reset_dir,
    },
};

pub(in crate::starry) async fn prepare_app_board_session_assets(
    workspace_root: &Path,
    arch: &str,
    target: &str,
    case: &StarryAppBoardCase,
    declared_session_files: &[PathBuf],
) -> anyhow::Result<Option<PreparedBoardSessionAssets>> {
    let rust_manifest = case.case_dir.join("rust/Cargo.toml");
    let c_manifest = case.case_dir.join("c/CMakeLists.txt");
    let session_environment = crate::starry::session_env::SessionEnvConfig::load(
        &case.case_dir,
        &case.board_config_path,
    )?
    .map(|config| config.capture_environment())
    .transpose()?
    .unwrap_or_default();
    if !rust_manifest.is_file()
        && !c_manifest.is_file()
        && !session_environment.is_bootstrap()
        && declared_session_files.is_empty()
    {
        return Ok(None);
    }
    anyhow::ensure!(
        !(rust_manifest.is_file() && c_manifest.is_file()),
        "Starry app board assets cannot combine Rust and C pipelines"
    );

    let rootfs = if rust_manifest.is_file() || c_manifest.is_file() {
        Some(crate::starry::rootfs::ensure_rootfs_in_tmp_dir(workspace_root, arch, target).await?)
    } else {
        None
    };
    let workspace_root = workspace_root.to_path_buf();
    let arch = arch.to_string();
    let target = target.to_string();
    let case_name = case.name.clone();
    let case_dir = case.case_dir.clone();
    let board_config_path = case.board_config_path.clone();
    let declared_session_files = declared_session_files.to_vec();
    let plan = BoardSessionAssetPlan {
        workspace_root,
        target,
        work_name: format!("app/{case_name}"),
        case_name,
        case_dir,
        board_config_path,
        declared_session_files,
        session_environment,
    };

    let assets = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        plan.prepare(|build_case, layout| {
            if rust_manifest.is_file() {
                prepare_rust_case_overlay_sync(
                    &arch,
                    build_case,
                    rootfs.as_ref().expect("Rust pipeline requested a rootfs"),
                    layout,
                    &starry_case_asset_config(),
                )
            } else if c_manifest.is_file() {
                prepare_c_case_overlay_sync(
                    &arch,
                    build_case,
                    rootfs.as_ref().expect("C pipeline requested a rootfs"),
                    layout,
                    &starry_case_asset_config(),
                )
            } else {
                reset_dir(&layout.overlay_dir)
            }
        })
    })
    .await
    .context("Starry app board session asset task failed")??;

    Ok(Some(assets))
}
