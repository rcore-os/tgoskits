use std::path::{Path, PathBuf};

use anyhow::Context;

use super::StarryAppBoardCase;
use crate::{
    starry::test::{
        PreparedBoardSessionAssets, SessionAssetDelivery, SessionRunDirectoryGuard,
        collect_upload_paths, copy_declared_session_files, starry_case_asset_config,
    },
    test::{
        build::{prepare_c_case_overlay_sync, prepare_rust_case_overlay_sync},
        case::{TestQemuCase, board_case_asset_layout, reset_dir},
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
    let session_env = crate::starry::session_env::SessionEnvConfig::load(
        &case.case_dir,
        &case.board_config_path,
    )?;
    if let Some(session_env) = &session_env {
        session_env.validate_environment()?;
    }
    if !rust_manifest.is_file()
        && !c_manifest.is_file()
        && session_env.is_none()
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
    let delivery = SessionAssetDelivery::for_session_env(session_env.as_ref());
    let session_env = session_env.unwrap_or_default();

    let assets = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let layout =
            board_case_asset_layout(&workspace_root, &target, &format!("app/{case_name}"))?;
        let cleanup = SessionRunDirectoryGuard::new(layout.run_dir.clone());
        let build_case = TestQemuCase {
            name: case_name.clone(),
            display_name: case_name,
            case_dir: case_dir.clone(),
            qemu_config_path: board_config_path,
            test_commands: Vec::new(),
            host_symbolize_success_regex: Vec::new(),
            host_http_server: None,
            subcases: Vec::new(),
            grouped_subcase_filter: None,
        };
        if rust_manifest.is_file() {
            prepare_rust_case_overlay_sync(
                &arch,
                &build_case,
                rootfs.as_ref().expect("Rust pipeline requested a rootfs"),
                &layout,
                &starry_case_asset_config(),
            )?;
        } else if c_manifest.is_file() {
            prepare_c_case_overlay_sync(
                &arch,
                &build_case,
                rootfs.as_ref().expect("C pipeline requested a rootfs"),
                &layout,
                &starry_case_asset_config(),
            )?;
        } else {
            reset_dir(&layout.overlay_dir)?;
        }
        copy_declared_session_files(&case_dir, &layout.overlay_dir, &declared_session_files)?;
        session_env.materialize(&layout.overlay_dir)?;
        let relative_paths = collect_upload_paths(&layout.overlay_dir)?;
        Ok(PreparedBoardSessionAssets::new(
            layout.overlay_dir,
            relative_paths,
            cleanup.preserve(),
            delivery,
        ))
    })
    .await
    .context("Starry app board session asset task failed")??;

    Ok(Some(assets))
}
