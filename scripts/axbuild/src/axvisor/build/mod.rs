mod config;
mod features;
mod load;
mod metadata;

#[cfg(test)]
mod tests;

pub type AxvisorBuildInfo = config::AxvisorBuildInfo;
use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow, bail};
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
use crate::context::ResolvedAxvisorRequest;

pub(crate) fn default_axvisor_build_info() -> AxvisorBuildInfo {
    config::default_axvisor_build_info()
}

pub(crate) fn workspace_root_from_axvisor_dir(axvisor_dir: &Path) -> PathBuf {
    load::workspace_root_from_axvisor_dir(axvisor_dir)
}

pub(crate) fn load_cargo_config(request: &ResolvedAxvisorRequest) -> anyhow::Result<Cargo> {
    let metadata =
        crate::build::cached_workspace_metadata().context("failed to load workspace metadata")?;
    to_cargo_config(load_build_config(request)?, request, metadata)
}

fn to_cargo_config(
    mut config: LoadedAxvisorBuildConfig,
    request: &ResolvedAxvisorRequest,
    metadata: &cargo_metadata::Metadata,
) -> anyhow::Result<Cargo> {
    config.target = request.target.clone();
    let makefile_features = crate::build::makefile_features_from_env();
    crate::build::apply_makefile_features_with_metadata(
        &mut config.build_info,
        &request.package,
        &makefile_features,
        metadata,
    )?;
    let known_platforms = platform_feature_names(metadata);
    reject_unsupported_nested_platform_features(&config.build_info.features, &known_platforms)?;
    let mut cargo = config
        .build_info
        .into_prepared_base_cargo_config_with_metadata(
            &request.package,
            &config.target,
            metadata,
        )?;
    patch_axvisor_cargo_config(&mut cargo, request, &config.vm_configs)?;
    Ok(cargo)
}

fn patch_axvisor_cargo_config(
    cargo: &mut Cargo,
    request: &ResolvedAxvisorRequest,
    config_vmconfigs: &[PathBuf],
) -> anyhow::Result<()> {
    cargo.package = request.package.clone();
    cargo.to_bin = default_axvisor_to_bin(&request.arch);
    ensure_axvisor_bin_arg(&mut cargo.args);
    cargo
        .env
        .insert("AX_ARCH".to_string(), request.arch.clone());
    cargo
        .env
        .insert("AX_TARGET".to_string(), request.target.clone());
    let vmconfigs = if request.vmconfigs.is_empty() {
        config_vmconfigs
            .iter()
            .map(|path| resolve_build_config_vmconfig_path(request, path))
            .collect::<Vec<_>>()
    } else {
        request.vmconfigs.clone()
    };
    if !vmconfigs.is_empty() {
        let joined = std::env::join_paths(&vmconfigs)
            .map_err(|e| anyhow!("failed to join vmconfig paths: {e}"))?;
        cargo.env.insert(
            "AXVISOR_VM_CONFIGS".to_string(),
            joined.to_string_lossy().into_owned(),
        );
    }

    if request.arch == "x86_64" {
        let has_vmx = cargo
            .features
            .iter()
            .any(|feature| matches!(feature.as_str(), "vmx" | "axvm/vmx"));
        let has_svm = cargo
            .features
            .iter()
            .any(|feature| matches!(feature.as_str(), "svm" | "axvm/svm"));
        match (has_vmx, has_svm) {
            (true, true) => bail!("x86_64 Axvisor features `vmx` and `svm` are mutually exclusive"),
            (false, false) => bail!(
                "x86_64 Axvisor build config must explicitly enable exactly one virtualization \
                 backend feature: `vmx` or `svm`"
            ),
            _ => {}
        }
    }
    cargo.features.sort();
    cargo.features.dedup();
    Ok(())
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

fn default_axvisor_to_bin(arch: &str) -> bool {
    !matches!(arch, "x86_64" | "loongarch64")
}

fn ensure_axvisor_bin_arg(args: &mut Vec<String>) {
    if args.iter().any(|arg| arg == "--bin") {
        return;
    }

    args.push("--bin".to_string());
    args.push(AXVISOR_PACKAGE.to_string());
}
