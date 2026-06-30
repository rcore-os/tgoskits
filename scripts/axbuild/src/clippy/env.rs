use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use anyhow::Context;
use cargo_metadata::{Metadata, Package};
use serde::Deserialize;

use super::{
    AX_CONFIG_PATH_ENV, AXCONFIG_FILE, AXSTD_STD_CLIPPY_TARGET, AXSTD_STD_DEFAULT_FEATURE,
    AXSTD_STD_PACKAGE, check::ClippyAxconfigOverride,
};

#[derive(Debug, Default, Deserialize)]
struct PackageAxbuildMetadata {
    #[serde(default)]
    axbuild: AxbuildMetadata,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct AxbuildMetadata {
    #[serde(default)]
    clippy_feature_axconfig_overrides: HashMap<String, Vec<String>>,
}

pub(super) fn clippy_env(package: &Package) -> Vec<(String, String)> {
    let Some(manifest_dir) = package.manifest_path.parent() else {
        return Vec::new();
    };
    let axconfig = manifest_dir.join(AXCONFIG_FILE);
    if !axconfig.exists() {
        return Vec::new();
    }

    vec![(AX_CONFIG_PATH_ENV.to_string(), axconfig.to_string())]
}

fn package_axconfig_path(package: &Package) -> Option<PathBuf> {
    let manifest_dir = package.manifest_path.parent()?;
    let axconfig = manifest_dir.join(AXCONFIG_FILE);
    axconfig.exists().then(|| axconfig.into_std_path_buf())
}

pub(super) fn feature_axconfig_overrides(package: &Package) -> HashMap<String, Vec<String>> {
    serde_json::from_value::<PackageAxbuildMetadata>(package.metadata.clone())
        .map(|metadata| metadata.axbuild.clippy_feature_axconfig_overrides)
        .unwrap_or_default()
}

pub(super) fn clippy_axconfig_override(
    package: &Package,
    target: Option<&str>,
    feature: &str,
    overrides: &[String],
    workspace_root: &Path,
) -> Option<ClippyAxconfigOverride> {
    if overrides.is_empty() {
        return None;
    }
    let target = target?.to_string();
    let platform_config = package_axconfig_path(package)?;
    let out_config = crate::context::axbuild_tmp_dir(workspace_root)
        .join("axconfig")
        .join(package.name.as_str())
        .join(target.as_str())
        .join("clippy")
        .join(feature)
        .join(".axconfig.toml");

    Some(ClippyAxconfigOverride {
        target,
        platform_config,
        out_config,
        overrides: overrides.to_vec(),
    })
}

fn with_axconfig_env_override(
    mut env: Vec<(String, String)>,
    override_config: Option<&ClippyAxconfigOverride>,
) -> Vec<(String, String)> {
    let Some(override_config) = override_config else {
        return env;
    };
    env.retain(|(key, _)| key != AX_CONFIG_PATH_ENV);
    env.push((
        AX_CONFIG_PATH_ENV.to_string(),
        override_config.out_config.display().to_string(),
    ));
    env
}

fn axstd_std_clippy_env(metadata: &Metadata) -> anyhow::Result<Vec<(String, String)>> {
    let mut envs = HashMap::new();
    crate::build::prepare_std_build_env(&mut envs, AXSTD_STD_CLIPPY_TARGET, true, metadata)
        .context("failed to prepare ax-std std clippy config")?;
    Ok(envs.into_iter().collect())
}

pub(super) fn feature_clippy_env(
    package: &Package,
    feature: &str,
    base_env: Vec<(String, String)>,
    axconfig_override: Option<&ClippyAxconfigOverride>,
    metadata: &Metadata,
) -> anyhow::Result<Vec<(String, String)>> {
    if package.name == AXSTD_STD_PACKAGE && feature == AXSTD_STD_DEFAULT_FEATURE {
        return axstd_std_clippy_env(metadata);
    }

    Ok(with_axconfig_env_override(base_env, axconfig_override))
}
