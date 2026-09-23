use std::{fs, path::PathBuf};

use anyhow::Context;

use super::ArceosBuildConfig;
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
) -> anyhow::Result<ArceosBuildConfig> {
    build::ensure_build_info(&request.build_info_path, ArceosBuildConfig::default_config)?;
    let content = fs::read_to_string(&request.build_info_path)?;
    build::reject_removed_std_field(&request.build_info_path, &content)?;
    let mut config: ArceosBuildConfig = toml::from_str(&content).with_context(|| {
        format!(
            "failed to parse build info {}",
            request.build_info_path.display()
        )
    })?;
    config.validate_runtime()?;
    config.build_info.validate_features()?;

    build::apply_makefile_features(&mut config.build_info, makefile_features)?;

    if let Some(smp) = request.smp {
        config.build_info.max_cpu_num = Some(smp);
    }
    config.build_info.validate_features()?;

    Ok(config)
}

pub(crate) fn default_build_info_path(package: &str, target: &str) -> anyhow::Result<PathBuf> {
    Ok(build::default_build_info_path_in_workspace(
        &crate::context::workspace_root_path()?,
        package,
        target,
    ))
}
