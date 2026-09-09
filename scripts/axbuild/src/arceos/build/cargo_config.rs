use anyhow::{Context, bail};
use ostool::build::config::Cargo;

use super::info::load_build_config_with_makefile_features;
use crate::{build, context::ResolvedBuildRequest};

pub(crate) fn load_cargo_config(request: &ResolvedBuildRequest) -> anyhow::Result<Cargo> {
    let metadata =
        build::cached_workspace_metadata().context("failed to load workspace metadata")?;
    let makefile_features = build::makefile_features_from_env();
    let config = load_build_config_with_makefile_features(request, &makefile_features)?;
    if config.app_c.is_some() {
        bail!(
            "ArceOS build config {} uses `app-c`; use the C app build path",
            request.build_info_path.display()
        );
    }
    let to_bin = config.to_bin;
    let mut cargo = config
        .build_info
        .into_prepared_std_cargo_config_with_metadata(
            &request.package,
            &request.target,
            metadata,
        )?;
    cargo.to_bin = to_bin;
    Ok(cargo)
}

pub(crate) fn load_c_app_cargo_config(request: &ResolvedBuildRequest) -> anyhow::Result<Cargo> {
    let metadata =
        build::cached_workspace_metadata().context("failed to load workspace metadata")?;
    let makefile_features = build::makefile_features_from_env();
    let config = load_build_config_with_makefile_features(request, &makefile_features)?;
    let to_bin = config.to_bin;
    let mut build_info = config.build_info;
    build_info.validated_max_cpu_num()?;
    build_info.resolve_c_app_features()?;
    let mut cargo = build_info.into_prepared_no_std_cargo_config_with_metadata(
        &request.package,
        &request.target,
        metadata,
        build::BareKernelLinkMode::Default,
    )?;
    cargo.to_bin = to_bin;
    Ok(cargo)
}
