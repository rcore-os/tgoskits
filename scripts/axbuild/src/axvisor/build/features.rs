use anyhow::anyhow;

use super::metadata::{platform_feature_names, platform_metadata_entries};
use crate::context::arch_for_target_checked;

const REMOVED_AXVISOR_PLATFORM_FEATURES: &[&str] = &["x86-qemu-q35", concat!("riscv64", "-sg2002")];

pub(super) fn normalize_axvisor_feature_surface(
    features: &mut Vec<String>,
    target: &str,
    plat_dyn: bool,
    metadata: &cargo_metadata::Metadata,
) -> anyhow::Result<()> {
    let known_platforms = platform_feature_names(metadata);
    let selected_platform =
        select_axvisor_platform_feature(features, target, plat_dyn, &known_platforms, metadata)?;

    let Some(platform) = selected_platform else {
        retain_non_platform_features(features, &known_platforms);
        return Ok(());
    };

    features.retain(|feature| {
        if feature == &platform {
            return true;
        }

        if nested_platform_feature_name(feature, &known_platforms).is_some() {
            return false;
        }

        if ax_hal_platform_feature_name(feature, &known_platforms).is_some() {
            return false;
        }

        if known_platforms.iter().any(|platform| platform == feature) {
            return false;
        }

        true
    });
    if !features.iter().any(|feature| feature == &platform) {
        features.push(platform);
    }
    Ok(())
}

fn retain_non_platform_features(features: &mut Vec<String>, known_platforms: &[String]) {
    features.retain(|feature| {
        nested_platform_feature_name(feature, known_platforms).is_none()
            && ax_hal_platform_feature_name(feature, known_platforms).is_none()
            && !known_platforms.iter().any(|platform| platform == feature)
    });
}

pub(super) fn reject_unsupported_nested_platform_features(
    features: &[String],
    known_platforms: &[String],
) -> anyhow::Result<()> {
    if let Some(feature) = features
        .iter()
        .find(|feature| removed_axvisor_platform_feature_name(feature).is_some())
    {
        return Err(anyhow!(
            "Axvisor platform feature `{feature}` has been removed; use `plat_dyn = true` and a \
             dynamic platform board config"
        ));
    }

    if let Some(feature) = features
        .iter()
        .find(|feature| is_axvisor_plat_dyn_feature(feature))
    {
        return Err(anyhow!(
            "Axvisor depends on an ax-std surface with dynamic platform support enabled; remove \
             dynamic platform features from `features`; found `{feature}`"
        ));
    }

    if let Some(feature) = features.iter().find(|feature| {
        nested_platform_feature_name(feature, known_platforms).is_some()
            || known_platforms.iter().any(|platform| platform == *feature)
    }) {
        return Err(anyhow!(
            "Axvisor build configs must use ax-hal platform features directly; found `{feature}`"
        ));
    }
    Ok(())
}

pub(super) fn normalize_axvisor_plat_dyn_features(features: &mut Vec<String>) {
    features.retain(|feature| !is_axvisor_plat_dyn_feature(feature));
}

pub(super) fn is_axvisor_plat_dyn_feature(feature: &str) -> bool {
    matches!(
        feature,
        "dyn-plat"
            | "plat-dyn"
            | "axplat-dyn"
            | "ax-feat/plat-dyn"
            | "ax-hal/plat-dyn"
            | "ax-std/plat-dyn"
            | "axvm/plat-dyn"
            | "ax-driver/plat-dyn"
    ) || feature.starts_with("axplat-dyn/")
}

fn removed_axvisor_platform_feature_name(feature: &str) -> Option<&str> {
    let name = feature
        .strip_prefix("ax-hal/")
        .or_else(|| feature.strip_prefix("ax-std/"))
        .or_else(|| feature.strip_prefix("ax-feat/"))
        .unwrap_or(feature);
    REMOVED_AXVISOR_PLATFORM_FEATURES
        .iter()
        .find(|platform| **platform == name)
        .copied()
}

fn select_axvisor_platform_feature(
    features: &[String],
    target: &str,
    plat_dyn: bool,
    known_platforms: &[String],
    metadata: &cargo_metadata::Metadata,
) -> anyhow::Result<Option<String>> {
    if plat_dyn {
        return Ok(None);
    }

    let explicit = features
        .iter()
        .filter(|feature| ax_hal_platform_feature_name(feature, known_platforms).is_some())
        .cloned()
        .collect::<Vec<_>>();

    if explicit.len() > 1 {
        return Err(anyhow!(
            "Axvisor build configs must select only one platform feature; found {}",
            explicit.join(", ")
        ));
    }
    if let Some(platform) = explicit.into_iter().next() {
        return Ok(Some(platform));
    }

    default_axvisor_platform_feature(target, metadata)
}

fn default_axvisor_platform_feature(
    target: &str,
    metadata: &cargo_metadata::Metadata,
) -> anyhow::Result<Option<String>> {
    let arch = arch_for_target_checked(target)?;
    let candidates = platform_metadata_entries(metadata)
        .into_iter()
        .filter(|platform| !platform.dynamic && platform.arch == arch)
        .collect::<Vec<_>>();
    let defaults = candidates
        .iter()
        .filter(|platform| platform.default_for_arch)
        .collect::<Vec<_>>();
    let platform = match defaults.as_slice() {
        [platform] => Some(*platform),
        [] => match candidates.as_slice() {
            [platform] => Some(platform),
            [] => None,
            _ => {
                return Err(anyhow!(
                    "Axvisor build configs must select an explicit platform feature for arch \
                     `{arch}`"
                ));
            }
        },
        _ => {
            return Err(anyhow!(
                "multiple Axvisor default platform features are registered for arch `{arch}`"
            ));
        }
    };
    Ok(platform.map(|platform| format!("ax-hal/{}", platform.platform)))
}

fn nested_platform_feature_name<'a>(
    feature: &'a str,
    known_platforms: &[String],
) -> Option<&'a str> {
    feature
        .strip_prefix("ax-std/")
        .or_else(|| feature.strip_prefix("ax-feat/"))
        .filter(|name| is_platform_control_feature(name, known_platforms))
}

fn ax_hal_platform_feature_name<'a>(
    feature: &'a str,
    known_platforms: &[String],
) -> Option<&'a str> {
    feature
        .strip_prefix("ax-hal/")
        .filter(|name| known_platforms.iter().any(|platform| platform == name))
}

fn is_platform_control_feature(name: &str, known_platforms: &[String]) -> bool {
    matches!(name, "plat-dyn" | "defplat" | "myplat")
        || known_platforms.iter().any(|platform| platform == name)
}
