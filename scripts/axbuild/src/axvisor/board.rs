use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
};

use anyhow::anyhow;

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
