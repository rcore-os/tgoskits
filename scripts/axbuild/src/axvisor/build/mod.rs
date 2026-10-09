mod config;
mod features;
mod load;
mod metadata;
mod vm_config;

#[cfg(test)]
mod tests;

pub type AxvisorBuildInfo = config::AxvisorBuildInfo;
use std::path::{Path, PathBuf};

pub(crate) use config::AxvisorBoardFile;
pub use config::{AXVISOR_PACKAGE, AxvisorBoardConfig};
pub(crate) use load::{
    default_build_info_path, load_board_file, load_target_from_build_config,
    resolve_build_info_path,
};
use ostool::build::config::Cargo;

use self::{
    config::LoadedAxvisorBuildConfig, features::reject_unsupported_nested_platform_features,
    load::load_build_config, metadata::platform_feature_names,
};
pub use crate::build::LogLevel;
use crate::context::{ResolvedAxvisorRequest, WorkspaceContext};

pub(crate) fn load_cargo_config(
    request: &ResolvedAxvisorRequest,
    workspace: &WorkspaceContext,
) -> anyhow::Result<Cargo> {
    let makefile_features = crate::build::makefile_features_from_env();
    load_cargo_config_with_makefile_features(request, workspace, &makefile_features)
}

fn load_cargo_config_with_makefile_features(
    request: &ResolvedAxvisorRequest,
    workspace: &WorkspaceContext,
    makefile_features: &[String],
) -> anyhow::Result<Cargo> {
    to_cargo_config(
        load_build_config(request)?,
        request,
        workspace,
        makefile_features,
    )
}

fn to_cargo_config(
    mut config: LoadedAxvisorBuildConfig,
    request: &ResolvedAxvisorRequest,
    workspace: &WorkspaceContext,
    makefile_features: &[String],
) -> anyhow::Result<Cargo> {
    config.target = request.target.clone();
    crate::build::apply_makefile_features(&mut config.build_info, makefile_features)?;
    let known_platforms = platform_feature_names(workspace.metadata());
    reject_unsupported_nested_platform_features(&config.build_info.features, &known_platforms)?;
    let mut cargo = config
        .build_info
        .into_prepared_std_cargo_config_with_metadata(
            &request.package,
            &config.target,
            workspace.metadata(),
            &workspace.axbuild_artifact_dir(),
        )?;
    patch_axvisor_cargo_config(&mut cargo, request);
    Ok(cargo)
}

fn patch_axvisor_cargo_config(cargo: &mut Cargo, request: &ResolvedAxvisorRequest) {
    cargo.package = request.package.clone();
    ensure_axvisor_bin_arg(&mut cargo.args);
    cargo
        .env
        .insert("AX_ARCH".to_string(), request.arch.clone());
    cargo
        .env
        .insert("AX_TARGET".to_string(), request.target.clone());
    cargo.features.sort();
    cargo.features.dedup();
}

/// Resolves bundle inputs independently of Cargo's compilation identity.
pub(crate) fn load_vmconfigs(
    request: &ResolvedAxvisorRequest,
    workspace: &WorkspaceContext,
) -> anyhow::Result<Vec<PathBuf>> {
    let config = load_build_config(request)?;
    let paths = if request.vmconfigs.is_empty() {
        config
            .vm_configs
            .iter()
            .map(|path| resolve_build_config_vmconfig_path(request, path))
            .collect()
    } else {
        request.vmconfigs.clone()
    };
    resolve_vmconfigs(request, &paths, workspace)
}

pub(crate) fn resolve_vmconfigs(
    request: &ResolvedAxvisorRequest,
    paths: &[PathBuf],
    workspace: &WorkspaceContext,
) -> anyhow::Result<Vec<PathBuf>> {
    vm_config::resolve_vmconfigs(request, paths, workspace)
}

fn resolve_build_config_vmconfig_path(request: &ResolvedAxvisorRequest, path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    let workspace_root = request
        .axvisor_dir
        .parent()
        .and_then(Path::parent)
        .unwrap_or(&request.axvisor_dir);
    workspace_root.join(path)
}

fn ensure_axvisor_bin_arg(args: &mut Vec<String>) {
    if args.iter().any(|arg| arg == "--bin") {
        return;
    }

    args.push("--bin".to_string());
    args.push(AXVISOR_PACKAGE.to_string());
}
