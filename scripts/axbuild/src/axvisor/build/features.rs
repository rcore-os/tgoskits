use anyhow::anyhow;

use super::metadata::platform_feature_names;

pub(super) fn normalize_axvisor_feature_surface(
    features: &mut Vec<String>,
    target: &str,
    metadata: &cargo_metadata::Metadata,
) -> anyhow::Result<()> {
    let _ = target;
    let known_platforms = platform_feature_names(metadata);
    retain_non_platform_features(features, &known_platforms);
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
        .find(|feature| is_removed_dynamic_platform_feature(feature))
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

pub(super) fn remove_dynamic_platform_features(features: &mut Vec<String>) {
    features.retain(|feature| !is_removed_dynamic_platform_feature(feature));
}

pub(super) fn is_removed_dynamic_platform_feature(feature: &str) -> bool {
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
    name == "plat-dyn" || known_platforms.iter().any(|platform| platform == name)
}
