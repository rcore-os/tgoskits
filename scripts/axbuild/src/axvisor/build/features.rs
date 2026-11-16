use ax_config::ConfigError;

pub(crate) fn reject_unsupported_nested_platform_features(
    features: &[String],
    known_platforms: &[String],
) -> Result<(), ConfigError> {
    if let Some(feature) = features
        .iter()
        .find(|feature| is_removed_dynamic_platform_feature(feature))
    {
        return Err(crate::build::invalid_config_value(
            "features",
            feature,
            "Axvisor has dynamic platform support enabled; remove dynamic platform features",
        ));
    }

    if let Some(feature) = features
        .iter()
        .find(|feature| matches!(feature.as_str(), "vmx" | "svm"))
    {
        return Err(crate::build::invalid_config_value(
            "features",
            feature,
            "x86 virtualization backend is selected from CPU capabilities at runtime",
        ));
    }

    if let Some(feature) = features.iter().find(|feature| {
        nested_platform_feature_name(feature, known_platforms).is_some()
            || known_platforms.iter().any(|platform| platform == *feature)
    }) {
        return Err(crate::build::invalid_config_value(
            "features",
            feature,
            "Axvisor build configs must use ax-hal platform features directly",
        ));
    }
    Ok(())
}

pub(super) fn is_removed_dynamic_platform_feature(feature: &str) -> bool {
    matches!(
        feature,
        "dyn-plat"
            | "plat-dyn"
            | "axplat-dyn"
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
        .filter(|name| is_platform_control_feature(name, known_platforms))
}

fn is_platform_control_feature(name: &str, known_platforms: &[String]) -> bool {
    name == "plat-dyn" || known_platforms.iter().any(|platform| platform == name)
}
