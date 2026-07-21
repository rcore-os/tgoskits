use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, anyhow};
use serde::Deserialize;

use super::build::{ArceosBuildConfig, ArceosBuildFile};

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub(crate) struct ArceosBoardFile {
    pub(crate) package: String,
    pub(crate) target: String,
    #[serde(flatten)]
    pub(crate) build_config: ArceosBuildConfig,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Board {
    pub(crate) name: String,
    pub(crate) path: PathBuf,
    pub(crate) package: String,
    pub(crate) target: String,
    pub(crate) build_config: ArceosBuildConfig,
}

pub(crate) fn arceos_dir(workspace_root: &Path) -> anyhow::Result<PathBuf> {
    let path = workspace_root.join("os/arceos");
    if path.exists() {
        Ok(path)
    } else {
        Err(anyhow!(
            "failed to locate ArceOS directory under {}",
            workspace_root.display()
        ))
    }
}

pub(crate) fn board_dir(workspace_root: &Path) -> anyhow::Result<PathBuf> {
    Ok(arceos_dir(workspace_root)?.join("configs/board"))
}

pub(crate) fn load_build_file(path: &Path) -> anyhow::Result<ArceosBuildFile> {
    super::build::load_arceos_build_file(path)
}

pub(crate) fn load_board_file(path: &Path) -> anyhow::Result<ArceosBoardFile> {
    let contents = fs::read_to_string(path)
        .with_context(|| format!("failed to read ArceOS board config {}", path.display()))?;
    crate::build::reject_removed_std_field(path, &contents)?;
    let board_file: ArceosBoardFile = toml::from_str(&contents)
        .with_context(|| format!("failed to parse ArceOS board config {}", path.display()))?;
    Ok(board_file)
}

pub(crate) fn board_default_list(workspace_root: &Path) -> anyhow::Result<Vec<Board>> {
    let board_dir = board_dir(workspace_root)?;
    let mut boards = Vec::new();
    for entry in fs::read_dir(&board_dir).map_err(|e| {
        anyhow!(
            "failed to read ArceOS board config directory {}: {e}",
            board_dir.display()
        )
    })? {
        let entry = entry?;
        let path = entry.path();
        if path.extension() != Some(OsStr::new("toml")) {
            continue;
        }

        let name = path
            .file_stem()
            .and_then(OsStr::to_str)
            .ok_or_else(|| anyhow!("invalid ArceOS board filename {}", path.display()))?
            .to_string();
        let board_file = load_board_file(&path)?;
        boards.push(Board {
            name,
            path,
            package: board_file.package,
            target: board_file.target,
            build_config: board_file.build_config,
        });
    }
    boards.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(boards)
}

pub(crate) fn find_board(workspace_root: &Path, name: &str) -> anyhow::Result<Option<Board>> {
    Ok(board_default_list(workspace_root)?
        .into_iter()
        .find(|board| board.name == name))
}

pub(crate) fn board_names(workspace_root: &Path) -> anyhow::Result<Vec<String>> {
    Ok(board_default_list(workspace_root)?
        .into_iter()
        .map(|board| board.name)
        .collect())
}

pub(crate) fn default_qemu_board(
    workspace_root: &Path,
    target: &str,
    package: Option<&str>,
) -> anyhow::Result<Option<Board>> {
    Ok(board_default_list(workspace_root)?
        .into_iter()
        .find(|board| {
            board.name.starts_with("qemu-")
                && board.target == target
                && package.is_none_or(|package| board.package == package)
        }))
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use tempfile::tempdir;

    use super::*;

    fn write_workspace(root: &Path) {
        fs::create_dir_all(root.join("os/arceos")).unwrap();
        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"apps/arceos/helloworld\"]\n",
        )
        .unwrap();
    }

    fn write_board(root: &Path, name: &str, body: &str) {
        let path = board_dir(root).unwrap().join(format!("{name}.toml"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    #[test]
    fn loads_dynamic_board_names_in_filename_order() {
        let root = tempdir().unwrap();
        write_workspace(root.path());
        write_board(
            root.path(),
            "z-board",
            r#"
package = "arceos-helloworld"
target = "aarch64-unknown-none-softfloat"
features = []
log = "Info"
"#,
        );
        write_board(
            root.path(),
            "a-board",
            r#"
package = "arceos-helloworld"
target = "x86_64-unknown-none"
features = []
log = "Info"
"#,
        );

        assert_eq!(
            board_names(root.path()).unwrap(),
            vec!["a-board".to_string(), "z-board".to_string()]
        );
    }

    #[test]
    fn load_board_accepts_dynamic_platform_template_without_plat_dyn_field() {
        let root = tempdir().unwrap();
        write_workspace(root.path());
        let path = board_dir(root.path()).unwrap().join("static.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"
package = "arceos-helloworld"
target = "aarch64-unknown-none-softfloat"
features = []
log = "Info"
"#,
        )
        .unwrap();

        let board = load_board_file(&path).unwrap();
        assert_eq!(board.target, "aarch64-unknown-none-softfloat");
    }

    #[test]
    fn default_qemu_board_matches_package_and_target() {
        let root = tempdir().unwrap();
        write_workspace(root.path());
        write_board(
            root.path(),
            "qemu-aarch64",
            r#"
package = "arceos-helloworld"
target = "aarch64-unknown-none-softfloat"
features = []
log = "Info"
"#,
        );

        let board = default_qemu_board(
            root.path(),
            "aarch64-unknown-none-softfloat",
            Some("arceos-helloworld"),
        )
        .unwrap();
        assert_eq!(board.unwrap().name, "qemu-aarch64");
        assert_eq!(
            default_qemu_board(root.path(), "aarch64-unknown-none-softfloat", None)
                .unwrap()
                .unwrap()
                .name,
            "qemu-aarch64"
        );
        assert!(
            default_qemu_board(
                root.path(),
                "aarch64-unknown-none-softfloat",
                Some("another-package"),
            )
            .unwrap()
            .is_none()
        );
    }
}
