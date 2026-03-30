use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
};

use crate::axvisor::build::{AxvisorBoardConfig, load_board_file};

#[derive(Debug, Clone, PartialEq)]
pub struct Board {
    pub name: String,
    pub path: PathBuf,
    pub target: String,
    pub config: AxvisorBoardConfig,
}

pub(crate) fn board_dir(axvisor_dir: &Path) -> PathBuf {
    axvisor_dir.join("configs/board")
}

pub(crate) fn board_default_list(axvisor_dir: &Path) -> anyhow::Result<Vec<Board>> {
    let mut boards = Vec::new();
    for entry in fs::read_dir(board_dir(axvisor_dir)).map_err(|e| {
        anyhow!(
            "failed to read Axvisor board config directory {}: {e}",
            board_dir(axvisor_dir).display()
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
            .ok_or_else(|| anyhow!("invalid Axvisor board filename {}", path.display()))?
            .to_string();
        let board_file = load_board_file(&path)?;
        let target = board_file.target.clone();
        boards.push(Board {
            name,
            path,
            target,
            config: board_file.into_board_config(),
        });
    }
    boards.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(boards)
}

pub(crate) fn find_board(axvisor_dir: &Path, name: &str) -> anyhow::Result<Option<Board>> {
    Ok(board_default_list(axvisor_dir)?
        .into_iter()
        .find(|board| board.name == name))
}

pub(crate) fn board_names(axvisor_dir: &Path) -> anyhow::Result<Vec<String>> {
    Ok(board_default_list(axvisor_dir)?
        .into_iter()
        .map(|board| board.name)
        .collect())
}

pub(crate) fn default_board_for_target(
    axvisor_dir: &Path,
    target: &str,
) -> anyhow::Result<Option<Board>> {
    Ok(board_default_list(axvisor_dir)?
        .into_iter()
        .find(|board| board.name.starts_with("qemu-") && board.target == target))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    fn write_board(root: &Path, name: &str, body: &str) -> PathBuf {
        let path = board_dir(root).join(format!("{name}.toml"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn loads_board_names_in_filename_order() {
        let root = tempdir().unwrap();
        write_board(
            root.path(),
            "z-board",
            r#"
env = { AX_IP = "10.0.2.15", AX_GW = "10.0.2.2" }
target = "aarch64-unknown-none-softfloat"
features = ["fs"]
log = "Info"
"#,
        );
        write_board(
            root.path(),
            "a-board",
            r#"
env = { AX_IP = "10.0.2.15", AX_GW = "10.0.2.2" }
target = "aarch64-unknown-none-softfloat"
features = ["ept-level-4"]
log = "Info"
"#,
        );

        assert_eq!(
            board_names(root.path()).unwrap(),
            vec!["a-board".to_string(), "z-board".to_string()]
        );
    }

    #[test]
    fn default_board_prefers_qemu_boards_with_matching_target() {
        let root = tempdir().unwrap();
        write_board(
            root.path(),
            "phytiumpi",
            r#"
env = { AX_IP = "10.0.2.15", AX_GW = "10.0.2.2" }
target = "aarch64-unknown-none-softfloat"
features = ["phytium-blk"]
log = "Info"
plat_dyn = true
"#,
        );
        write_board(
            root.path(),
            "qemu-aarch64",
            r#"
env = { AX_IP = "10.0.2.15", AX_GW = "10.0.2.2" }
target = "aarch64-unknown-none-softfloat"
features = ["ept-level-4"]
log = "Info"
plat_dyn = true
"#,
        );
        write_board(
            root.path(),
            "qemu-riscv64",
            r#"
env = { AX_IP = "10.0.2.15", AX_GW = "10.0.2.2" }
target = "riscv64gc-unknown-none-elf"
features = ["ept-level-4"]
log = "Info"
"#,
        );

        let board =
            default_board_for_target(root.path(), "aarch64-unknown-none-softfloat").unwrap();
        assert_eq!(board.unwrap().name, "qemu-aarch64");
    }

    #[test]
    fn find_board_returns_none_for_unknown_name() {
        let root = tempdir().unwrap();
        write_board(
            root.path(),
            "orangepi-5-plus",
            r#"
env = { AX_IP = "10.0.2.15", AX_GW = "10.0.2.2" }
target = "aarch64-unknown-none-softfloat"
features = ["rk3588-clk"]
log = "Info"
plat_dyn = true
"#,
        );

        assert!(find_board(root.path(), "orangepi").unwrap().is_none());
    }
}
