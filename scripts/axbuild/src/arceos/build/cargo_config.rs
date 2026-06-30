use anyhow::{Context, bail};
use ostool::build::config::Cargo;

use super::{ArceosBuildInfo, info::load_build_config_with_makefile_features_and_metadata};
use crate::{build, context::ResolvedBuildRequest};

pub(crate) fn load_cargo_config(request: &ResolvedBuildRequest) -> anyhow::Result<Cargo> {
    let metadata =
        build::cached_workspace_metadata().context("failed to load workspace metadata")?;
    let makefile_features = build::makefile_features_from_env();
    let config = load_build_config_with_makefile_features_and_metadata(
        request,
        &makefile_features,
        Some(metadata),
    )?;
    if config.app_c.is_some() {
        bail!(
            "ArceOS build config {} uses `app-c`; use the C app build path",
            request.build_info_path.display()
        );
    }
    let build_info = config.build_info;

    build_info.into_prepared_base_cargo_config_with_metadata(
        &request.package,
        &request.target,
        request.plat_dyn,
        metadata,
    )
}

pub(crate) fn load_c_app_cargo_config(request: &ResolvedBuildRequest) -> anyhow::Result<Cargo> {
    let metadata =
        build::cached_workspace_metadata().context("failed to load workspace metadata")?;
    let makefile_features = build::makefile_features_from_env();
    let mut build_info = load_build_config_with_makefile_features_and_metadata(
        request,
        &makefile_features,
        Some(metadata),
    )?
    .build_info;
    let plat_dyn = build_info.effective_plat_dyn(&request.target, request.plat_dyn);

    build_info.validated_max_cpu_num()?;
    build_info.prepare_non_dynamic_platform_for(
        &request.package,
        &request.target,
        plat_dyn,
        metadata,
    )?;
    build_info.resolve_features_with_metadata(
        &request.package,
        &request.target,
        plat_dyn,
        metadata,
    );
    let rustflags = build::toolchain_rustflags_for_features(&build_info.env, &build_info.features);
    let args = ArceosBuildInfo::build_cargo_args(&request.target, &rustflags);

    build_info.prepare_log_env();
    build_info.prepare_max_cpu_num_env()?;

    Ok(build_info.into_base_cargo_config_with_to_bin(
        request.package.clone(),
        request.target.clone(),
        args,
        false,
    ))
}
