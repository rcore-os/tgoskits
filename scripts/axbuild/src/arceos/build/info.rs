use std::{fs, path::PathBuf};

use anyhow::Context;

use super::ArceosBuildConfig;
#[cfg(test)]
use super::ArceosBuildInfo;
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

#[cfg(test)]
pub(super) fn load_build_info(request: &ResolvedBuildRequest) -> anyhow::Result<ArceosBuildInfo> {
    let makefile_features = build::makefile_features_from_env();
    load_build_info_with_makefile_features(request, &makefile_features)
}

#[cfg(test)]
fn load_build_info_with_makefile_features(
    request: &ResolvedBuildRequest,
    makefile_features: &[String],
) -> anyhow::Result<ArceosBuildInfo> {
    Ok(load_build_config_with_makefile_features(request, makefile_features)?.build_info)
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

#[cfg(test)]
pub(super) fn resolve_build_info_path_in_dir(dir: &std::path::Path, target: &str) -> PathBuf {
    let bare_path = dir.join(format!("build-{target}.toml"));
    if bare_path.exists() {
        return bare_path;
    }

    let dotted_path = dir.join(format!(".build-{target}.toml"));
    if dotted_path.exists() {
        return dotted_path;
    }

    dotted_path
}
