use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
};

use anyhow::Context;
use serde::Deserialize;

use super::build::StarryBuildInfo;
use crate::context::STARRY_PACKAGE;

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub(crate) struct StarryBoardFile {
    pub(crate) target: String,
    #[serde(flatten)]
    pub(crate) build_info: StarryBuildInfo,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Board {
    pub(crate) name: String,
    pub(crate) path: PathBuf,
    pub(crate) target: String,
    pub(crate) build_info: StarryBuildInfo,
}

pub(crate) fn starry_dir(workspace_root: &Path) -> anyhow::Result<PathBuf> {
    let path = workspace_root.join("os/StarryOS");
    let workspace_manifest = path.join("Cargo.toml");
    let package_manifest = path.join("starryos/Cargo.toml");
    if workspace_manifest.exists() || package_manifest.exists() {
        Ok(path)
    } else {
        Err(anyhow!(
            "failed to locate Starry workspace directory for package `{}` under {}",
            STARRY_PACKAGE,
            workspace_root.display()
        ))
    }
}

pub(crate) fn board_dir(workspace_root: &Path) -> anyhow::Result<PathBuf> {
    Ok(starry_dir(workspace_root)?.join("configs/board"))
}

pub(crate) fn load_board_file(path: &Path) -> anyhow::Result<StarryBoardFile> {
    toml::from_str::<StarryBoardFile>(&fs::read_to_string(path)?)
        .map_err(anyhow::Error::from)
        .with_context(|| format!("failed to parse Starry board config {}", path.display()))
}

pub(crate) fn board_default_list(workspace_root: &Path) -> anyhow::Result<Vec<Board>> {
    let board_dir = board_dir(workspace_root)?;
    let mut boards = Vec::new();
    for entry in fs::read_dir(&board_dir).map_err(|e| {
        anyhow!(
            "failed to read Starry board config directory {}: {e}",
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
            .ok_or_else(|| anyhow!("invalid Starry board filename {}", path.display()))?
            .to_string();
        let Ok(board_file) = load_board_file(&path) else {
            continue;
        };
        boards.push(Board {
            name,
            path,
            target: board_file.target,
            build_info: board_file.build_info,
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

pub(crate) fn default_board_for_target(
    workspace_root: &Path,
    target: &str,
) -> anyhow::Result<Option<Board>> {
    Ok(board_default_list(workspace_root)?
        .into_iter()
        .find(|board| board.name.starts_with("qemu-") && board.target == target))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    fn write_workspace(root: &Path) {
        let starry_workspace_dir = root.join("os/StarryOS");
        let starry_dir = root.join("os/StarryOS/starryos");
        let src_dir = starry_dir.join("src");
        fs::create_dir_all(&starry_workspace_dir).unwrap();
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(src_dir.join("lib.rs"), "").unwrap();
        fs::write(
            starry_dir.join("Cargo.toml"),
            "[package]\nname = \"starryos\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        fs::write(
            starry_workspace_dir.join("Cargo.toml"),
            "[workspace]\nmembers = [\"starryos\"]\n",
        )
        .unwrap();
        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"os/StarryOS/starryos\"]\n",
        )
        .unwrap();
    }

    fn write_board(root: &Path, name: &str, body: &str) -> PathBuf {
        let path = board_dir(root).unwrap().join(format!("{name}.toml"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn loads_board_names_in_filename_order_and_ignores_non_build_configs() {
        let root = tempdir().unwrap();
        write_workspace(root.path());
        write_board(
            root.path(),
            "z-board",
            r#"
target = "aarch64-unknown-none-softfloat"
env = { AX_IP = "10.0.2.15", AX_GW = "10.0.2.2" }
features = ["qemu"]
log = "Warn"
plat_dyn = false
"#,
        );
        write_board(
            root.path(),
            "a-board",
            r#"
target = "x86_64-unknown-none"
env = { AX_IP = "10.0.2.15", AX_GW = "10.0.2.2" }
features = ["qemu"]
log = "Warn"
plat_dyn = false
"#,
        );
        write_board(
            root.path(),
            "orangepi-5-plus-uboot",
            r#"
serial = "/dev/ttyUSB0"
baud_rate = "1500000"
"#,
        );

        assert_eq!(
            board_names(root.path()).unwrap(),
            vec!["a-board".to_string(), "z-board".to_string()]
        );
    }

    #[test]
    fn default_board_prefers_qemu_board_with_matching_target() {
        let root = tempdir().unwrap();
        write_workspace(root.path());
        write_board(
            root.path(),
            "orangepi-5-plus",
            r#"
target = "aarch64-unknown-none-softfloat"
env = {}
features = ["common"]
log = "Info"
plat_dyn = true
"#,
        );
        write_board(
            root.path(),
            "qemu-aarch64",
            r#"
target = "aarch64-unknown-none-softfloat"
env = { AX_IP = "10.0.2.15", AX_GW = "10.0.2.2" }
features = ["qemu"]
log = "Warn"
plat_dyn = false
"#,
        );
        write_board(
            root.path(),
            "qemu-riscv64",
            r#"
target = "riscv64gc-unknown-none-elf"
env = { AX_IP = "10.0.2.15", AX_GW = "10.0.2.2" }
features = ["qemu"]
log = "Warn"
plat_dyn = false
"#,
        );

        let board =
            default_board_for_target(root.path(), "aarch64-unknown-none-softfloat").unwrap();
        assert_eq!(board.unwrap().name, "qemu-aarch64");
    }
}
