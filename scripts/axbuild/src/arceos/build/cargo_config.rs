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
    load_cargo_config_with_default_features(request, workspace, true)
}

pub(crate) fn load_cargo_config_with_default_features(
    request: &ResolvedBuildRequest,
    workspace: &WorkspaceContext,
    include_default_features: bool,
) -> anyhow::Result<Cargo> {
    let metadata = workspace.metadata();
    let axbuild_dir = workspace.axbuild_artifact_dir();
    let config = load_build_config_with_makefile_features(request, &[])?;
    if config.app_c.is_some() {
        bail!(
            "ArceOS build config {} uses `app-c`; use the C app build path",
            request.build_info_path.display()
        );
    }
    let to_bin = config.to_bin;
    let build_info = config.common;
    let mut cargo = if config.freestanding {
        build::into_prepared_no_std_cargo_config_with_metadata_and_defaults(
            build_info,
            &request.package,
            &request.target,
            metadata,
            build::BareKernelLinkMode::Pie,
            include_default_features,
            request.runtime,
        )?
    } else {
        build::into_prepared_std_cargo_config_with_metadata_and_defaults(
            build_info,
            &request.package,
            &request.target,
            metadata,
            &axbuild_dir,
            include_default_features,
            request.runtime,
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
    let config = load_build_config_with_makefile_features(request, &[])?;
    let to_bin = config.to_bin;
    let mut build_info = config.common;
    build_info.validate()?;
    build::resolve_c_app_features(&mut build_info)?;
    let mut cargo = build::into_prepared_no_std_cargo_config_with_metadata(
        build_info,
        &request.package,
        &request.target,
        metadata,
        build::BareKernelLinkMode::Default,
        request.runtime,
    )?;
    cargo.to_bin = to_bin;
    Ok(cargo)
}
