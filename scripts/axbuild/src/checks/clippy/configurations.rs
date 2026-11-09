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
    #[serde(default)]
    cpu_suite: Option<CpuSuiteClippy>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CpuSuiteClippy {
    path: std::path::PathBuf,
    image: crate::test::normal::arceos::cpu::catalog::Image,
}

fn cpu_suite_configurations(
    package: &Package,
    source: CpuSuiteClippy,
) -> anyhow::Result<Vec<PackageClippyConfiguration>> {
    use crate::test::normal::arceos::cpu::catalog::Catalog;
    let suite_path = package
        .manifest_path
        .parent()
        .context("CPU image manifest directory")?
        .as_std_path()
        .join(source.path);
    let root = suite_path.parent().context("CPU suite directory")?;
    let catalog = Catalog::parse(&std::fs::read_to_string(&suite_path)?)?;
    let targets = super::targets::docs_rs_targets(package);
    let mut configurations = Vec::new();
    for id in &catalog.run_order {
        let batch = &catalog.batches[id];
        if batch.image != source.image {
            continue;
        }
        let selection = catalog.select(&batch.arch, Some(id))?.remove(0);
        let target = targets
            .iter()
            .find(|target| target.starts_with(&batch.arch))
            .with_context(|| {
                format!(
                    "CPU image {} has no static target for {}",
                    package.name, batch.arch
                )
            })?;
        let build: toml::Value =
            toml::from_str(&std::fs::read_to_string(root.join(&batch.build_config))?)?;
        let cpus = build
            .get("max_cpu_num")
            .and_then(toml::Value::as_integer)
            .filter(|cpus| *cpus > 0)
            .context("CPU batch must declare positive max_cpu_num")?;
        let mut env = selection.build_env();
        env.insert("SMP".to_owned(), cpus.to_string());
        configurations.push(PackageClippyConfiguration {
            name: id.clone(),
            target: target.clone(),
            features: selection.features(),
            rustflags: Vec::new(),
            env: env.into_iter().collect(),
        });
    }
    ensure!(
        !configurations.is_empty(),
        "CPU image {} has no declared batches",
        package.name
    );
    Ok(configurations)
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
    let cpu_suite = metadata.clippy.cpu_suite;
    let mut configurations = metadata
        .clippy
        .configurations
        .into_iter()
        .map(|configuration| validate_configuration(package, configuration, &mut names))
        .collect::<anyhow::Result<Vec<_>>>()?;
    if let Some(source) = cpu_suite {
        for configuration in cpu_suite_configurations(package, source)? {
            ensure!(
                names.insert(configuration.name.clone()),
                "duplicate CPU clippy configuration"
            );
            configurations.push(configuration);
        }
    }
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
