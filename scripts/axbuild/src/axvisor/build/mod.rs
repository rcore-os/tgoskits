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
    crate::build::apply_makefile_features(&mut config.build_info, &makefile_features)?;
    let known_platforms = platform_feature_names(metadata);
    reject_unsupported_nested_platform_features(&config.build_info.features, &known_platforms)?;
    let max_cpu_num = config.build_info.max_cpu_num;
    let host_noise = config.host_noise;
    let guest_restart = config.guest_restart;
    let mut cargo = config
        .build_info
        .into_prepared_base_cargo_config_with_metadata(
            &request.package,
            &config.target,
            metadata,
        )?;
    patch_axvisor_cargo_config(&mut cargo, request, &config.vm_configs)?;
    inject_host_noise_config(&mut cargo, host_noise.as_ref(), max_cpu_num)?;
    inject_guest_restart_config(&mut cargo, guest_restart.as_ref(), max_cpu_num)?;
    Ok(cargo)
}

fn inject_guest_restart_config(
    cargo: &mut Cargo,
    guest_restart: Option<&config::AxvisorGuestRestartConfig>,
    max_cpu_num: Option<usize>,
) -> anyhow::Result<()> {
    let Some(guest_restart) = guest_restart else {
        return Ok(());
    };
    if guest_restart.delay_ms == 0 {
        bail!("Axvisor guest_restart.delay_ms must be positive");
    }
    if guest_restart.ready_timeout_ms == 0 {
        bail!("Axvisor guest_restart.ready_timeout_ms must be positive");
    }
    if let Some(max_cpu_num) = max_cpu_num
        && guest_restart.cpu >= max_cpu_num
    {
        bail!(
            "Axvisor guest_restart.cpu {} is outside max_cpu_num {}",
            guest_restart.cpu,
            max_cpu_num
        );
    }

    cargo.env.insert(
        "AXVISOR_GUEST_RESTART_VM_ID".to_string(),
        guest_restart.vm_id.to_string(),
    );
    cargo.env.insert(
        "AXVISOR_GUEST_RESTART_CPU".to_string(),
        guest_restart.cpu.to_string(),
    );
    cargo.env.insert(
        "AXVISOR_GUEST_RESTART_DELAY_MS".to_string(),
        guest_restart.delay_ms.to_string(),
    );
    cargo.env.insert(
        "AXVISOR_GUEST_RESTART_READY_TIMEOUT_MS".to_string(),
        guest_restart.ready_timeout_ms.to_string(),
    );
    Ok(())
}

fn inject_host_noise_config(
    cargo: &mut Cargo,
    host_noise: Option<&config::AxvisorHostNoiseConfig>,
    max_cpu_num: Option<usize>,
) -> anyhow::Result<()> {
    let Some(host_noise) = host_noise else {
        return Ok(());
    };
    if host_noise.max_duration_ms == 0 {
        bail!("Axvisor host_noise.max_duration_ms must be positive");
    }
    if let Some(max_cpu_num) = max_cpu_num
        && host_noise.cpu >= max_cpu_num
    {
        bail!(
            "Axvisor host_noise.cpu {} is outside max_cpu_num {}",
            host_noise.cpu,
            max_cpu_num
        );
    }

    cargo.env.insert(
        "AXVISOR_HOST_NOISE_CPU".to_string(),
        host_noise.cpu.to_string(),
    );
    cargo.env.insert(
        "AXVISOR_HOST_NOISE_MAX_DURATION_MS".to_string(),
        host_noise.max_duration_ms.to_string(),
    );
    Ok(())
}

fn patch_axvisor_cargo_config(
    cargo: &mut Cargo,
    request: &ResolvedAxvisorRequest,
    config_vmconfigs: &[PathBuf],
) -> anyhow::Result<()> {
    cargo.package = request.package.clone();
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

fn ensure_axvisor_bin_arg(args: &mut Vec<String>) {
    if args.iter().any(|arg| arg == "--bin") {
        return;
    }

    args.push("--bin".to_string());
    args.push(AXVISOR_PACKAGE.to_string());
}
