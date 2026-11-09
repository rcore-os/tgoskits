use std::path::{Path, PathBuf};

use anyhow::Context;

use super::AxvisorConfig;
use crate::{axvisor::board, context::arch_for_target_checked};

pub(crate) fn load_board_file(path: &Path) -> anyhow::Result<AxvisorConfig> {
    let config: AxvisorConfig = crate::build::load_build_config(path)?;
    config
        .validate()
        .with_context(|| format!("invalid Axvisor board config {}", path.display()))?;
    Ok(config)
}

pub(crate) fn resolve_build_info_path(
    axvisor_dir: &Path,
    target: &str,
    explicit_path: Option<PathBuf>,
) -> anyhow::Result<PathBuf> {
    if let Some(path) = explicit_path {
        return Ok(path);
    }
    let _ = arch_for_target_checked(target)?;
    Ok(default_build_info_path(axvisor_dir, target))
}

pub(crate) fn workspace_root_from_axvisor_dir(axvisor_dir: &Path) -> PathBuf {
    axvisor_dir
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| axvisor_dir.to_path_buf())
}

pub(crate) fn default_build_info_path(axvisor_dir: &Path, target: &str) -> PathBuf {
    crate::build::default_build_info_path_in_workspace(
        &workspace_root_from_axvisor_dir(axvisor_dir),
        super::AXVISOR_PACKAGE,
        target,
    )
}

pub(crate) fn load_or_create_default_config(
    axvisor_dir: &Path,
    target: &str,
    path: &Path,
) -> anyhow::Result<AxvisorConfig> {
    if path.exists() {
        return crate::build::load_build_config(path);
    }
    if board::board_dir(axvisor_dir).exists()
        && let Some(board) = board::default_board_for_target(axvisor_dir, target)?
    {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(&board.path, path).with_context(|| {
            format!(
                "failed to copy default board {} to {}",
                board.path.display(),
                path.display()
            )
        })?;
        return Ok(board.config);
    }
    let config = AxvisorConfig::default();
    crate::build::ensure_build_info(path, || config.clone())?;
    Ok(config)
}
