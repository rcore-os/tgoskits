use anyhow::bail;
use ostool::build::config::Cargo;

use super::info::config_with_makefile_features;
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
    let (cargo, files) =
        resolve_cargo_config_with_defaults(request, workspace, include_default_features)?;
    files.materialize()?;
    Ok(cargo)
}

pub(crate) fn resolve_cargo_config(
    request: &ResolvedBuildRequest,
    workspace: &WorkspaceContext,
) -> anyhow::Result<(Cargo, build::BuildFiles)> {
    resolve_cargo_config_with_defaults(request, workspace, true)
}

fn resolve_cargo_config_with_defaults(
    request: &ResolvedBuildRequest,
    workspace: &WorkspaceContext,
    include_default_features: bool,
) -> anyhow::Result<(Cargo, build::BuildFiles)> {
    let mut files = build::BuildFiles::default();
    let metadata = workspace.metadata();
    let axbuild_dir = workspace.axbuild_artifact_dir();
    let config = config_with_makefile_features(request, &[])?;
    if config.app_c.is_some() {
        bail!(
            "ArceOS build input {:?} uses `app-c`; use the C app build path",
            request.source
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
        build::resolve_std_cargo_config(
            build_info,
            build::StdBuildInput {
                package: &request.package,
                target: &request.target,
                metadata,
                axbuild_dir: &axbuild_dir,
                include_default_features,
                runtime: request.runtime,
            },
            &mut files,
        )?
    };
    cargo.to_bin |= to_bin;
    Ok((cargo, files))
}

pub(crate) fn load_c_app_cargo_config(
    request: &ResolvedBuildRequest,
    workspace: &WorkspaceContext,
) -> anyhow::Result<Cargo> {
    let metadata = workspace.metadata();
    let config = config_with_makefile_features(request, &[])?;
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
