use std::collections::{BTreeMap, BTreeSet};

use cargo_metadata::Package;
use serde::Deserialize;

use super::{AX_HAL_PACKAGE, HOST_TEST_FEATURE};

const CLIPPY_TARGET_ALIASES: &[(&str, &str)] = &[
    (
        "aarch64-unknown-linux-gnu",
        "aarch64-unknown-none-softfloat",
    ),
    ("aarch64-unknown-none", "aarch64-unknown-none-softfloat"),
    (
        "loongarch64-unknown-none",
        "loongarch64-unknown-none-softfloat",
    ),
];

const AX_HAL_PLATFORM_FEATURE_TARGET_ARCHES: &[(&str, &[&str])] = &[];
const CLIPPY_TARGET_ARCHES: &[&str] = &["aarch64", "loongarch64", "riscv64", "x86_64"];

#[derive(Debug, Default, Deserialize)]
struct PackageDocsMetadata {
    #[serde(default, rename = "docs.rs")]
    docs_rs: DocsRsMetadata,
    #[serde(default)]
    docs: NestedDocsMetadata,
}

#[derive(Debug, Default, Deserialize)]
struct NestedDocsMetadata {
    #[serde(default)]
    rs: DocsRsMetadata,
}

#[derive(Debug, Default, Deserialize)]
struct DocsRsMetadata {
    #[serde(default)]
    targets: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
struct PackageFeatureTargetMetadata {
    #[serde(default)]
    clippy: ClippyFeatureTargetMetadata,
}

#[derive(Debug, Default, Deserialize)]
struct ClippyFeatureTargetMetadata {
    #[serde(default, rename = "feature-targets")]
    feature_targets: BTreeMap<String, Vec<String>>,
}

pub(super) fn docs_rs_targets(package: &Package) -> Vec<String> {
    let Ok(metadata) = serde_json::from_value::<PackageDocsMetadata>(package.metadata.clone())
    else {
        return Vec::new();
    };
    let targets = if metadata.docs_rs.targets.is_empty() {
        metadata.docs.rs.targets
    } else {
        metadata.docs_rs.targets
    };

    let mut unique_targets = BTreeSet::new();
    for target in targets {
        unique_targets.insert(normalize_clippy_target(&target).to_string());
    }

    unique_targets.into_iter().collect()
}

pub(super) fn normalize_clippy_target(target: &str) -> &str {
    CLIPPY_TARGET_ALIASES
        .iter()
        .find_map(|(source, normalized)| (*source == target).then_some(*normalized))
        .unwrap_or(target)
}

fn clippy_target_arch(target: &str) -> Option<&'static str> {
    if target.starts_with("aarch64-") {
        Some("aarch64")
    } else if target.starts_with("loongarch64-") {
        Some("loongarch64")
    } else if target.starts_with("riscv64") {
        Some("riscv64")
    } else if target.starts_with("x86_64-") {
        Some("x86_64")
    } else {
        None
    }
}

fn ax_hal_platform_target_arches(feature: &str) -> Option<&'static [&'static str]> {
    AX_HAL_PLATFORM_FEATURE_TARGET_ARCHES
        .iter()
        .find_map(|(platform_feature, target_arches)| {
            (*platform_feature == feature).then_some(*target_arches)
        })
}

fn ax_hal_feature_dependency(feature_dependency: &str) -> Option<&str> {
    feature_dependency
        .strip_prefix("ax-hal/")
        .or_else(|| feature_dependency.strip_prefix("ax-hal?/"))
}

fn ax_hal_platform_constraints<'a>(
    package: &'a Package,
    feature: &'a str,
) -> Vec<&'static [&'static str]> {
    let mut constraints = Vec::new();
    if package.name == AX_HAL_PACKAGE
        && let Some(target_arches) = ax_hal_platform_target_arches(feature)
    {
        constraints.push(target_arches);
    }

    if let Some(feature_dependencies) = package.features.get(feature) {
        constraints.extend(
            feature_dependencies
                .iter()
                .filter_map(|feature_dependency| ax_hal_feature_dependency(feature_dependency))
                .filter_map(ax_hal_platform_target_arches),
        );
    }

    constraints
}

/// Reads `[package.metadata.clippy.feature-targets]`.
///
/// Invalid or empty entries deliberately fall back to an unconstrained check,
/// so a metadata typo cannot silently remove a feature from the clippy matrix.
fn metadata_feature_target_arches(
    package: &Package,
    feature: &str,
) -> Option<BTreeSet<&'static str>> {
    let metadata =
        serde_json::from_value::<PackageFeatureTargetMetadata>(package.metadata.clone()).ok()?;
    let configured_arches = metadata.clippy.feature_targets.get(feature)?;
    let mut arches = BTreeSet::new();
    for configured_arch in configured_arches {
        let arch = CLIPPY_TARGET_ARCHES
            .iter()
            .copied()
            .find(|candidate| *candidate == configured_arch.as_str())?;
        arches.insert(arch);
    }
    (!arches.is_empty()).then_some(arches)
}

pub(super) fn feature_supported_on_clippy_target(
    package: &Package,
    feature: &str,
    target: Option<&str>,
) -> bool {
    let constraints = ax_hal_platform_constraints(package, feature);
    let metadata_arches = metadata_feature_target_arches(package, feature);
    if constraints.is_empty() && metadata_arches.is_none() {
        return true;
    }
    let Some(target) = target else {
        return false;
    };
    clippy_target_arch(target).is_some_and(|arch| {
        constraints
            .iter()
            .all(|target_arches| target_arches.contains(&arch))
            && metadata_arches
                .as_ref()
                .is_none_or(|arches| arches.contains(arch))
    })
}

pub(super) fn feature_requires_host_target(package: &Package, feature: &str) -> bool {
    fn requires_host_target<'a>(
        package: &'a Package,
        feature: &'a str,
        visited: &mut BTreeSet<&'a str>,
    ) -> bool {
        if feature == HOST_TEST_FEATURE {
            return true;
        }
        if !visited.insert(feature) {
            return false;
        }

        package.features.get(feature).is_some_and(|dependencies| {
            dependencies.iter().any(|dependency| {
                dependency == HOST_TEST_FEATURE
                    || (package.features.contains_key(dependency)
                        && requires_host_target(package, dependency, visited))
            })
        })
    }

    requires_host_target(package, feature, &mut BTreeSet::new())
}
