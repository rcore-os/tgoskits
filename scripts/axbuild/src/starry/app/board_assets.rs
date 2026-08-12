use std::{
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::Context;

use super::StarryAppBoardCase;
use crate::{
    starry::test::{
        PreparedBoardSessionAssets, collect_upload_paths, copy_declared_session_files,
        starry_case_asset_config,
    },
    support::process::ProcessExt,
    test::{
        build::prepare_rust_case_overlay_sync,
        case::{TestQemuCase, board_case_asset_layout, reset_dir},
    },
};

struct BoardPrebuildContext<'a> {
    workspace_root: &'a Path,
    arch: &'a str,
    target: &'a str,
    case_name: &'a str,
    case_dir: &'a Path,
    rootfs: &'a Path,
    staging_root: &'a Path,
    overlay_dir: &'a Path,
}

pub(in crate::starry) async fn prepare_app_board_session_assets(
    workspace_root: &Path,
    arch: &str,
    target: &str,
    case: &StarryAppBoardCase,
    declared_session_files: &[PathBuf],
) -> anyhow::Result<Option<PreparedBoardSessionAssets>> {
    let rust_manifest = case.case_dir.join("rust/Cargo.toml");
    if !board_case_needs_session_assets(&case.case_dir, declared_session_files) {
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
        if rust_manifest.is_file() {
            let build_case = TestQemuCase {
                name: case_name.clone(),
                display_name: case_name.clone(),
                case_dir: case_dir.clone(),
                qemu_config_path: board_config_path,
                test_commands: Vec::new(),
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
        } else {
            reset_dir(&layout.staging_root)?;
            reset_dir(&layout.overlay_dir)?;
        }
        run_board_prebuild(BoardPrebuildContext {
            workspace_root: &workspace_root,
            arch: &arch,
            target: &target,
            case_name: &case_name,
            case_dir: &case_dir,
            rootfs: &rootfs,
            staging_root: &layout.staging_root,
            overlay_dir: &layout.overlay_dir,
        })?;
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

fn board_case_needs_session_assets(case_dir: &Path, declared_session_files: &[PathBuf]) -> bool {
    case_dir.join("rust/Cargo.toml").is_file()
        || case_dir.join("prebuild.sh").is_file()
        || !declared_session_files.is_empty()
}

fn run_board_prebuild(context: BoardPrebuildContext<'_>) -> anyhow::Result<()> {
    let prebuild_path = context.case_dir.join("prebuild.sh");
    if !prebuild_path.is_file() {
        return Ok(());
    }

    Command::new("bash")
        .arg(&prebuild_path)
        .current_dir(context.case_dir)
        .env("STARRY_APP_NAME", context.case_name)
        .env("STARRY_APP_DIR", context.case_dir)
        .env("STARRY_WORKSPACE", context.workspace_root)
        .env("STARRY_ARCH", context.arch)
        .env("STARRY_TARGET", context.target)
        .env("STARRY_ROOTFS", context.rootfs)
        .env("STARRY_STAGING_ROOT", context.staging_root)
        .env("STARRY_OVERLAY_DIR", context.overlay_dir)
        .exec()
        .with_context(|| {
            format!(
                "failed to run board app prebuild {}",
                prebuild_path.display()
            )
        })
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use tempfile::tempdir;

    use super::{BoardPrebuildContext, board_case_needs_session_assets, run_board_prebuild};

    #[test]
    fn top_level_prebuild_is_a_board_session_asset_source() {
        let root = tempdir().unwrap();
        fs::write(root.path().join("prebuild.sh"), "#!/usr/bin/env bash\n").unwrap();

        assert!(board_case_needs_session_assets(root.path(), &[]));
    }

    #[test]
    fn top_level_prebuild_populates_the_upload_overlay() {
        let root = tempdir().unwrap();
        let case_dir = root.path().join("app");
        let staging_root = root.path().join("staging");
        let overlay_dir = root.path().join("upload");
        fs::create_dir_all(&case_dir).unwrap();
        fs::create_dir_all(&staging_root).unwrap();
        fs::create_dir_all(&overlay_dir).unwrap();
        fs::write(
            case_dir.join("prebuild.sh"),
            "#!/usr/bin/env bash\nset -eu\nprintf '%s\\n' \"$STARRY_ARCH/$STARRY_TARGET\" > \
             \"$STARRY_OVERLAY_DIR/result\"\n",
        )
        .unwrap();

        run_board_prebuild(BoardPrebuildContext {
            workspace_root: root.path(),
            arch: "aarch64",
            target: "aarch64-unknown-none-softfloat",
            case_name: "demo",
            case_dir: &case_dir,
            rootfs: Path::new("/rootfs.img"),
            staging_root: &staging_root,
            overlay_dir: &overlay_dir,
        })
        .unwrap();

        assert_eq!(
            fs::read_to_string(overlay_dir.join("result")).unwrap(),
            "aarch64/aarch64-unknown-none-softfloat\n"
        );
    }
}
