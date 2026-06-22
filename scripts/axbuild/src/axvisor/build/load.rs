use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, anyhow};

use super::{AxvisorBoardFile, AxvisorBuildInfo, LoadedAxvisorBuildConfig};
use crate::{
    axvisor::board,
    context::{ResolvedAxvisorRequest, arch_for_target_checked},
};

pub(crate) fn load_board_file(path: &Path) -> anyhow::Result<AxvisorBoardFile> {
    let content = fs::read_to_string(path).map_err(|e| {
        anyhow!(
            "failed to read Axvisor board config {}: {e}",
            path.display()
        )
    })?;
    reject_unsupported_axvisor_fields(path, &content)?;
    toml::from_str(&content).map_err(|e| {
        anyhow!(
            "failed to parse Axvisor board config {}: {e}",
            path.display()
        )
    })
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
        &super::workspace_root_from_axvisor_dir(axvisor_dir),
        super::AXVISOR_PACKAGE,
        target,
    )
}

pub(crate) fn load_target_from_build_config(path: &Path) -> anyhow::Result<Option<String>> {
    let content = fs::read_to_string(path).map_err(|e| {
        anyhow!(
            "failed to read Axvisor build config {}: {e}",
            path.display()
        )
    })?;
    reject_unsupported_axvisor_fields(path, &content)?;

    if let Ok(board_file) = toml::from_str::<AxvisorBoardFile>(&content) {
        return Ok(Some(board_file.target));
    }
    if toml::from_str::<AxvisorBuildInfo>(&content).is_ok() {
        return Ok(None);
    }

    Err(anyhow!("invalid Axvisor build config {}", path.display()))
}

pub(super) fn load_build_config(
    request: &ResolvedAxvisorRequest,
) -> anyhow::Result<LoadedAxvisorBuildConfig> {
    println!("Using build config: {}", request.build_info_path.display());

    if !request.build_info_path.exists() {
        return load_or_create_missing_build_config(request);
    }

    let content = fs::read_to_string(&request.build_info_path).map_err(|e| {
        anyhow!(
            "failed to read Axvisor build config {}: {e}",
            request.build_info_path.display()
        )
    })?;
    reject_unsupported_axvisor_fields(&request.build_info_path, &content)?;

    if let Ok(board_config) = toml::from_str::<AxvisorBoardFile>(&content) {
        let mut loaded = board_config.into_loaded();
        apply_request_smp(&mut loaded, request);
        return Ok(loaded);
    }

    toml::from_str::<AxvisorBuildInfo>(&content)
        .map(|build_info| {
            let mut loaded = LoadedAxvisorBuildConfig {
                build_info,
                target: request.target.clone(),
                vm_configs: Vec::new(),
            };
            apply_request_smp(&mut loaded, request);
            loaded
        })
        .map_err(|e| {
            anyhow!(
                "failed to parse build info {}: {e}",
                request.build_info_path.display()
            )
        })
}

fn load_or_create_missing_build_config(
    request: &ResolvedAxvisorRequest,
) -> anyhow::Result<LoadedAxvisorBuildConfig> {
    if let Some(parent) = request.build_info_path.parent() {
        fs::create_dir_all(parent)?;
    }
    if let Some(default_board) =
        board::default_board_for_target(&request.axvisor_dir, &request.target)?
    {
        fs::copy(&default_board.path, &request.build_info_path).map_err(|e| {
            anyhow!(
                "failed to copy default board config {} to {}: {e}",
                default_board.path.display(),
                request.build_info_path.display()
            )
        })?;
        let mut loaded = default_board
            .config
            .into_loaded(default_board.target.clone());
        apply_request_smp(&mut loaded, request);
        return Ok(loaded);
    }

    let default_build_info = super::default_axvisor_build_info();
    fs::write(
        &request.build_info_path,
        toml::to_string_pretty(&default_build_info)?,
    )
    .with_context(|| {
        format!(
            "failed to write default Axvisor build config {}",
            request.build_info_path.display()
        )
    })?;

    let mut loaded = LoadedAxvisorBuildConfig {
        build_info: default_build_info,
        target: request.target.clone(),
        vm_configs: Vec::new(),
    };
    apply_request_smp(&mut loaded, request);
    Ok(loaded)
}

fn apply_request_smp(loaded: &mut LoadedAxvisorBuildConfig, request: &ResolvedAxvisorRequest) {
    if let Some(smp) = request.smp {
        loaded.build_info.max_cpu_num = Some(smp);
    }
}

fn reject_unsupported_axvisor_fields(path: &Path, content: &str) -> anyhow::Result<()> {
    crate::build::reject_removed_std_field(path, content)?;
    crate::build::reject_arceos_app_c_field(path, content)?;
    Ok(())
}
