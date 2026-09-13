use std::path::{Path, PathBuf};

use anyhow::Context;

use super::StarryAppBoardCase;
use crate::{
    starry::test::{
        PreparedBoardSessionAssets, collect_upload_paths, copy_declared_session_files,
        starry_case_asset_config,
    },
    test::{
        build::prepare_rust_case_overlay_sync,
        case::{TestQemuCase, board_case_asset_layout},
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
    if !rust_manifest.is_file() {
        return Ok(None);
    }

    let rootfs =
        crate::starry::rootfs::ensure_rootfs_in_tmp_dir(workspace_root, arch, target).await?;
    let workspace_root = workspace_root.to_path_buf();
    let arch = arch.to_string();
    let target = target.to_string();
    let case_name = case.name.clone();
    let case_dir = case.case_dir.clone();
    let board_config_path = case.board_config_path.clone();
    let declared_session_files = declared_session_files.to_vec();

    let assets = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let layout =
            board_case_asset_layout(&workspace_root, &target, &format!("app/{case_name}"))?;
        let build_case = TestQemuCase {
            name: case_name.clone(),
            display_name: case_name,
            case_dir: case_dir.clone(),
            qemu_config_path: board_config_path,
            test_commands: Vec::new(),
            grouped_command_selection: Default::default(),
            host_symbolize_success_regex: Vec::new(),
            host_http_server: None,
            subcases: Vec::new(),
            grouped_subcase_filter: None,
        };
        prepare_rust_case_overlay_sync(
            &arch,
            &build_case,
            &rootfs,
            &layout,
            &starry_case_asset_config(),
        )?;
        copy_declared_session_files(&case_dir, &layout.overlay_dir, &declared_session_files)?;
        let relative_paths = collect_upload_paths(&layout.overlay_dir)?;
        Ok(PreparedBoardSessionAssets {
            root: layout.overlay_dir,
            relative_paths,
        })
    })
    .await
    .context("Starry app board session asset task failed")??;

    Ok(Some(assets))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[tokio::test]
    async fn board_apps_reject_ambiguous_assets_before_preparing_a_rootfs() {
        let root = tempfile::tempdir().unwrap();
        let case_dir = root.path().join("case");
        fs::create_dir_all(case_dir.join("c")).unwrap();
        fs::create_dir_all(case_dir.join("sh")).unwrap();
        fs::write(case_dir.join("c/CMakeLists.txt"), "project(probe C)").unwrap();
        let case = StarryAppBoardCase {
            name: "probe".into(),
            init_path: case_dir.join("init.sh"),
            init_cmd: "echo probe".into(),
            build_config_path: case_dir.join("build.toml"),
            board_config_path: case_dir.join("board.toml"),
            target: "aarch64-unknown-none-softfloat".into(),
            case_dir,
        };
        let result =
            prepare_app_board_session_assets(root.path(), "aarch64", &case.target, &case, &[])
                .await;
        let error = result.expect_err("a board app must not silently ignore its C assets");
        assert!(error.to_string().contains("multiple asset pipelines"));
    }
}
