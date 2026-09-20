use std::path::{Path, PathBuf};

use super::StarryAppBoardCase;
use crate::starry::test::{
    BoardSessionAssetRequest, PreparedBoardSessionAssets, prepare_board_session_assets,
};

pub(in crate::starry) async fn prepare_app_board_session_assets(
    workspace_root: &Path,
    target_dir: &Path,
    arch: &str,
    target: &str,
    case: &StarryAppBoardCase,
    declared_session_files: &[PathBuf],
) -> anyhow::Result<Option<PreparedBoardSessionAssets>> {
    let case_name = format!("app/{}", case.name);
    prepare_board_session_assets(BoardSessionAssetRequest {
        workspace_root,
        target_dir,
        arch,
        target,
        case_name: &case_name,
        case_dir: &case.case_dir,
        board_config_path: &case.board_config_path,
        declared_session_files,
    })
    .await
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
        let result = prepare_app_board_session_assets(
            root.path(),
            &root.path().join("target"),
            "aarch64",
            &case.target,
            &case,
            &[],
        )
        .await;
        let error = result.expect_err("a board app must not silently ignore its C assets");
        assert!(error.to_string().contains("multiple asset pipelines"));
    }
}
