use std::collections::BTreeSet;

use cargo_metadata::Package;
use serde::Deserialize;

use super::AX_HAL_PACKAGE;

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

fn normalize_clippy_target(target: &str) -> &str {
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

pub(super) fn feature_supported_on_clippy_target(
    package: &Package,
    feature: &str,
    target: Option<&str>,
) -> bool {
    let constraints = ax_hal_platform_constraints(package, feature);
    if constraints.is_empty() {
        return true;
    }
    let Some(target) = target else {
        return false;
    };
    clippy_target_arch(target).is_some_and(|arch| {
        constraints
            .iter()
            .all(|target_arches| target_arches.contains(&arch))
    })
}
