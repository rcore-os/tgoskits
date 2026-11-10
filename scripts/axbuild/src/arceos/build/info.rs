use std::path::PathBuf;

use super::ArceosConfig;
use crate::{build, context::ResolvedBuildRequest};

pub(crate) fn resolve_build_info_path(
    package: &str,
    target: &str,
    explicit_path: Option<PathBuf>,
) -> anyhow::Result<PathBuf> {
    if let Some(path) = explicit_path {
        return Ok(path);
    }

    super::default_build_info_path(package, target)
}

pub(super) fn load_build_config_with_makefile_features(
    request: &ResolvedBuildRequest,
    makefile_features: &[String],
) -> anyhow::Result<ArceosConfig> {
    let mut config = request.config.clone();
    build::apply_makefile_features(&mut config.common, makefile_features)?;
    config.validate()?;
    build::validate_features(&config.common)?;
    Ok(config)
}

pub(crate) fn default_build_info_path(package: &str, target: &str) -> anyhow::Result<PathBuf> {
    Ok(build::default_build_info_path_in_workspace(
        &crate::context::workspace_root_path()?,
        package,
        target,
    ))
}
