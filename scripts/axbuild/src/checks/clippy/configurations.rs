use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, ensure};
use cargo_metadata::Package;
use serde::Deserialize;

use super::targets::normalize_clippy_target;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PackageClippyConfiguration {
    pub(super) name: String,
    pub(super) target: String,
    pub(super) features: Vec<String>,
    pub(super) rustflags: Vec<String>,
    pub(super) env: Vec<(String, String)>,
}

#[derive(Debug, Default, Deserialize)]
struct PackageMetadata {
    #[serde(default)]
    clippy: ClippyMetadata,
}

#[derive(Debug, Default, Deserialize)]
struct ClippyMetadata {
    #[serde(default)]
    configurations: Vec<RawClippyConfiguration>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawClippyConfiguration {
    name: String,
    target: String,
    #[serde(default)]
    features: Vec<String>,
    #[serde(default)]
    rustflags: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
}

pub(super) fn package_clippy_configurations(
    package: &Package,
) -> anyhow::Result<Vec<PackageClippyConfiguration>> {
    if package.metadata.is_null() {
        return Ok(Vec::new());
    }
    let metadata = serde_json::from_value::<PackageMetadata>(package.metadata.clone())
        .with_context(|| format!("invalid clippy metadata for `{}`", package.name))?;
    let mut names = BTreeSet::new();
    let mut configurations = metadata
        .clippy
        .configurations
        .into_iter()
        .map(|configuration| validate_configuration(package, configuration, &mut names))
        .collect::<anyhow::Result<Vec<_>>>()?;
    configurations.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(configurations)
}

fn validate_configuration(
    package: &Package,
    configuration: RawClippyConfiguration,
    names: &mut BTreeSet<String>,
) -> anyhow::Result<PackageClippyConfiguration> {
    ensure!(
        !configuration.name.is_empty() && configuration.name.trim() == configuration.name,
        "clippy configuration name for `{}` must be non-empty and trimmed",
        package.name
    );
    ensure!(
        names.insert(configuration.name.clone()),
        "duplicate clippy configuration `{}` for `{}`",
        configuration.name,
        package.name
    );
    ensure!(
        !configuration.target.is_empty() && configuration.target.trim() == configuration.target,
        "clippy configuration `{}` target for `{}` must be non-empty and trimmed",
        configuration.name,
        package.name
    );

    let mut features = BTreeSet::new();
    for feature in configuration.features {
        ensure!(
            !feature.is_empty() && feature.trim() == feature,
            "clippy configuration `{}` feature for `{}` must be non-empty and trimmed",
            configuration.name,
            package.name
        );
        features.insert(feature);
    }
    for rustflag in &configuration.rustflags {
        ensure!(
            !rustflag.is_empty() && rustflag.trim() == rustflag,
            "clippy configuration `{}` rustflag for `{}` must be non-empty and trimmed",
            configuration.name,
            package.name
        );
    }

    Ok(PackageClippyConfiguration {
        name: configuration.name,
        target: normalize_clippy_target(&configuration.target).to_string(),
        features: features.into_iter().collect(),
        rustflags: configuration.rustflags,
        env: configuration.env.into_iter().collect(),
    })
}
