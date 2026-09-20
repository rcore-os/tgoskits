use anyhow::bail;
use ostool::build::config::Cargo;

use super::info::load_build_config_with_makefile_features;
use crate::{
    build,
    context::{ResolvedBuildRequest, WorkspaceContext},
};

pub(crate) fn load_cargo_config(
    request: &ResolvedBuildRequest,
    workspace: &WorkspaceContext,
) -> anyhow::Result<Cargo> {
    let metadata = workspace.metadata();
    let axbuild_dir = workspace.axbuild_artifact_dir();
    let makefile_features = build::makefile_features_from_env();
    let config = load_build_config_with_makefile_features(request, &makefile_features)?;
    if config.app_c.is_some() {
        bail!(
            "ArceOS build config {} uses `app-c`; use the C app build path",
            request.build_info_path.display()
        );
    }
    let to_bin = config.to_bin;
    let mut cargo = if config.freestanding {
        config
            .build_info
            .into_prepared_no_std_cargo_config_with_metadata(
                &request.package,
                &request.target,
                metadata,
                build::BareKernelLinkMode::Pie,
            )?
    } else {
        config
            .build_info
            .into_prepared_std_cargo_config_with_metadata(
                &request.package,
                &request.target,
                metadata,
                &axbuild_dir,
            )?
    };
    cargo.to_bin |= to_bin;
    Ok(cargo)
}

pub(crate) fn load_c_app_cargo_config(
    request: &ResolvedBuildRequest,
    workspace: &WorkspaceContext,
) -> anyhow::Result<Cargo> {
    let metadata = workspace.metadata();
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
