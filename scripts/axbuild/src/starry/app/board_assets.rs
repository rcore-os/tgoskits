use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

use anyhow::{Context, ensure};
use ostool::board::config::BoardRunConfig;
use sha2::{Digest, Sha256};

use super::StarryAppBoardCase;
use crate::starry::test::{
    BoardSessionAssetRequest, PreparedBoardSessionAssets, prepare_board_session_assets,
};

/// Bind HTTP payloads to the host-prepared bytes through the serial command.
pub(in crate::starry) fn bind_board_session_checksums(
    board: &mut BoardRunConfig,
    assets: Option<&PreparedBoardSessionAssets>,
) -> anyhow::Result<()> {
    for step in &mut board.shell_check_steps {
        if let Some(command) = &step.shell_cmd {
            step.shell_cmd = Some(resolve_session_file_checksums(command, assets)?);
        }
    }
    Ok(())
}

fn resolve_session_file_checksums(
    command: &str,
    assets: Option<&PreparedBoardSessionAssets>,
) -> anyhow::Result<String> {
    let mut remaining = command;
    let mut resolved = String::new();
    while let Some((prefix, reference)) = remaining.split_once("${sessionFileSha256:") {
        resolved.push_str(prefix);
        let (relative, suffix) = reference
            .split_once('}')
            .context("unterminated session file checksum placeholder")?;
        let assets = assets.context("session file checksum requires prepared assets")?;
        ensure!(
            assets
                .relative_paths
                .iter()
                .any(|path| path == Path::new(relative)),
            "checksum references an undeclared session file: {relative}"
        );
        let path = assets.root.join(relative);
        let mut file = File::open(&path)
            .with_context(|| format!("failed to open checksum input {}", path.display()))?;
        let mut digest = Sha256::new();
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let count = file
                .read(&mut buffer)
                .with_context(|| format!("failed to hash {}", path.display()))?;
            if count == 0 {
                break;
            }
            digest.update(&buffer[..count]);
        }
        resolved.push_str(&format!("{:x}", digest.finalize()));
        remaining = suffix;
    }
    resolved.push_str(remaining);
    Ok(resolved)
}

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
