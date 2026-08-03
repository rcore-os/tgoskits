use std::{
    collections::BTreeSet,
    fs,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, bail, ensure};

use crate::{
    starry::test::starry_case_asset_config,
    test::{
        build::prepare_c_case_overlay_sync,
        case::{TestQemuCase, board_case_asset_layout},
    },
};

const C_SOURCE_DIR: &str = "c";
const CMAKE_PROJECT_FILE: &str = "CMakeLists.txt";

#[derive(Debug)]
pub(in crate::starry) struct PreparedBoardSessionAssets {
    pub(crate) root: PathBuf,
    pub(crate) relative_paths: Vec<PathBuf>,
}

pub(crate) async fn prepare_board_session_assets(
    workspace_root: &Path,
    arch: &str,
    target: &str,
    case_name: &str,
    case_dir: &Path,
    board_config_path: &Path,
    declared_session_files: &[PathBuf],
) -> anyhow::Result<Option<PreparedBoardSessionAssets>> {
    let cmake_project = case_dir.join(C_SOURCE_DIR).join(CMAKE_PROJECT_FILE);
    if !cmake_project.is_file() {
        return Ok(None);
    }
    for unsupported_dir in ["sh", "python"] {
        ensure!(
            !case_dir.join(unsupported_dir).exists(),
            "board case `{case_name}` combines C assets with unsupported `{unsupported_dir}` \
             assets"
        );
    }

    let rootfs =
        crate::starry::rootfs::ensure_rootfs_in_tmp_dir(workspace_root, arch, target).await?;
    let workspace_root = workspace_root.to_path_buf();
    let arch = arch.to_string();
    let target = target.to_string();
    let case_name = case_name.to_string();
    let case_dir = case_dir.to_path_buf();
    let board_config_path = board_config_path.to_path_buf();
    let declared_session_files = declared_session_files.to_vec();

    let assets = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let layout = board_case_asset_layout(&workspace_root, &target, &case_name)?;
        let case = TestQemuCase {
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
        prepare_c_case_overlay_sync(&arch, &case, &rootfs, &layout, &starry_case_asset_config())?;
        copy_declared_session_files(&case_dir, &layout.overlay_dir, &declared_session_files)?;
        let relative_paths = collect_upload_paths(&layout.overlay_dir)?;
        Ok(PreparedBoardSessionAssets {
            root: layout.overlay_dir,
            relative_paths,
        })
    })
    .await
    .context("Starry board session asset task failed")??;
    Ok(Some(assets))
}

pub(in crate::starry) fn copy_declared_session_files(
    case_dir: &Path,
    upload_root: &Path,
    relative_paths: &[PathBuf],
) -> anyhow::Result<()> {
    let canonical_case_dir = case_dir.canonicalize().with_context(|| {
        format!(
            "failed to resolve board case directory {}",
            case_dir.display()
        )
    })?;
    let mut copied = BTreeSet::new();

    for relative_path in relative_paths {
        validate_relative_path(relative_path)?;
        ensure!(
            copied.insert(relative_path.clone()),
            "duplicate session file path `{}`",
            relative_path.display()
        );
        let source = case_dir.join(relative_path);
        let metadata = fs::symlink_metadata(&source).with_context(|| {
            format!(
                "failed to inspect declared session file `{}`",
                source.display()
            )
        })?;
        ensure!(
            !metadata.file_type().is_symlink(),
            "declared session file `{}` must not be a symbolic link",
            relative_path.display()
        );
        ensure!(
            metadata.is_file(),
            "declared session file `{}` is not a regular file",
            relative_path.display()
        );
        let canonical_source = source.canonicalize().with_context(|| {
            format!(
                "failed to resolve declared session file `{}`",
                source.display()
            )
        })?;
        ensure!(
            canonical_source.starts_with(&canonical_case_dir),
            "declared session file `{}` escapes the board case directory",
            relative_path.display()
        );

        let destination = upload_root.join(relative_path);
        match fs::symlink_metadata(&destination) {
            Ok(_) => bail!(
                "declared session file `{}` conflicts with a CMake install product",
                relative_path.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "failed to inspect upload destination `{}`",
                        destination.display()
                    )
                });
            }
        }
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        fs::copy(&source, &destination).with_context(|| {
            format!(
                "failed to copy declared session file `{}` to `{}`",
                source.display(),
                destination.display()
            )
        })?;
    }
    Ok(())
}

pub(in crate::starry) fn collect_upload_paths(upload_root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut pending = vec![upload_root.to_path_buf()];
    let mut relative_paths = Vec::new();

    while let Some(directory) = pending.pop() {
        let mut entries = fs::read_dir(&directory)
            .with_context(|| format!("failed to read upload directory {}", directory.display()))?
            .collect::<Result<Vec<_>, _>>()
            .with_context(|| format!("failed to read upload directory {}", directory.display()))?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)
                .with_context(|| format!("failed to inspect upload entry {}", path.display()))?;
            if metadata.file_type().is_symlink() {
                bail!(
                    "board session upload entry `{}` must not be a symbolic link",
                    path.display()
                );
            }
            if metadata.is_dir() {
                pending.push(path);
                continue;
            }
            ensure!(
                metadata.is_file(),
                "board session upload entry `{}` is not a regular file",
                path.display()
            );
            relative_paths.push(
                path.strip_prefix(upload_root)
                    .expect("upload entry is below upload root")
                    .to_path_buf(),
            );
        }
    }

    relative_paths.sort();
    ensure!(
        !relative_paths.is_empty(),
        "board CMake install produced no files in upload root `{}`",
        upload_root.display()
    );
    Ok(relative_paths)
}

fn validate_relative_path(path: &Path) -> anyhow::Result<()> {
    ensure!(
        !path.as_os_str().is_empty() && !path.is_absolute(),
        "session file path `{}` must be a non-empty relative path",
        path.display()
    );
    ensure!(
        path.components()
            .all(|component| matches!(component, Component::Normal(_))),
        "session file path `{}` must be a normalized relative path",
        path.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use tempfile::tempdir;

    use super::{collect_upload_paths, copy_declared_session_files};

    #[test]
    fn upload_paths_are_sorted_and_keep_nested_relative_paths() {
        let root = tempdir().unwrap();
        fs::create_dir_all(root.path().join("tools/network")).unwrap();
        fs::write(root.path().join("tools/network/probe"), b"probe").unwrap();
        fs::create_dir_all(root.path().join("bin")).unwrap();
        fs::write(root.path().join("bin/app"), b"app").unwrap();

        assert_eq!(
            collect_upload_paths(root.path()).unwrap(),
            [
                PathBuf::from("bin/app"),
                PathBuf::from("tools/network/probe")
            ]
        );
    }

    #[test]
    fn upload_root_must_contain_at_least_one_regular_file() {
        let root = tempdir().unwrap();

        let error = collect_upload_paths(root.path()).unwrap_err();

        assert!(error.to_string().contains("no files"));
    }

    #[cfg(unix)]
    #[test]
    fn upload_root_rejects_symbolic_links() {
        use std::os::unix::fs::symlink;

        let root = tempdir().unwrap();
        fs::write(root.path().join("app"), b"app").unwrap();
        symlink("app", root.path().join("alias")).unwrap();

        let error = collect_upload_paths(root.path()).unwrap_err();

        assert!(error.to_string().contains("symbolic link"));
    }

    #[test]
    fn declared_session_files_are_copied_without_renaming() {
        let root = tempdir().unwrap();
        let case_dir = root.path().join("case");
        let upload_root = root.path().join("upload");
        fs::create_dir_all(case_dir.join("fixtures")).unwrap();
        fs::create_dir_all(&upload_root).unwrap();
        fs::write(case_dir.join("fixtures/input.json"), b"input").unwrap();

        copy_declared_session_files(
            &case_dir,
            &upload_root,
            &[PathBuf::from("fixtures/input.json")],
        )
        .unwrap();

        assert_eq!(
            fs::read(upload_root.join("fixtures/input.json")).unwrap(),
            b"input"
        );
    }

    #[test]
    fn declared_session_files_reject_collisions_and_invalid_paths() {
        let root = tempdir().unwrap();
        let case_dir = root.path().join("case");
        let upload_root = root.path().join("upload");
        fs::create_dir_all(case_dir.join("bin")).unwrap();
        fs::create_dir_all(upload_root.join("bin")).unwrap();
        fs::write(case_dir.join("bin/app"), b"source").unwrap();
        fs::write(upload_root.join("bin/app"), b"built").unwrap();

        let collision =
            copy_declared_session_files(&case_dir, &upload_root, &[PathBuf::from("bin/app")])
                .unwrap_err();
        assert!(collision.to_string().contains("conflicts"));

        let escape =
            copy_declared_session_files(&case_dir, &upload_root, &[PathBuf::from("../escape")])
                .unwrap_err();
        assert!(escape.to_string().contains("relative"));
    }
}
