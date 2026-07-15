use std::collections::BTreeSet;

use anyhow::bail;

const C_DEFINE_FEATURE_PREFIX: &str = "c-define:";

pub(super) fn dynamic_pie_for_c_app(features: &[String]) -> bool {
    let _ = features;
    true
}

pub(super) fn c_config_features(features: &[String]) -> BTreeSet<String> {
    let config_features: BTreeSet<_> = features
        .iter()
        .filter_map(|feature| {
            if feature.starts_with(C_DEFINE_FEATURE_PREFIX) {
                return None;
            }
            if feature.starts_with("ax-hal/")
                || feature.starts_with("ax-driver/")
                || feature.starts_with("ax-runtime/")
            {
                return None;
            }
            feature
                .strip_prefix("ax-libc/")
                .or_else(|| feature.strip_prefix("ax-std/"))
                .or(Some(feature.as_str()))
        })
        .filter(|feature| {
            !matches!(*feature, "ax-libc" | "ax-std" | "plat-dyn") && !feature.contains('/')
        })
        .map(str::to_string)
        .collect();
    config_features
}

pub(super) fn c_defines(features: &[String]) -> BTreeSet<String> {
    features
        .iter()
        .filter_map(|feature| feature.strip_prefix(C_DEFINE_FEATURE_PREFIX))
        .map(str::to_string)
        .collect()
}

pub(super) fn c_compiler_features(
    cargo_features: &[String],
    case_features: &[String],
) -> Vec<String> {
    let mut features = cargo_features.to_vec();
    features.extend(
        case_features
            .iter()
            .filter(|feature| feature.starts_with(C_DEFINE_FEATURE_PREFIX))
            .cloned(),
    );
    features
}

pub(super) fn has_feature(features: &[String], name: &str) -> bool {
    features.iter().any(|feature| {
        feature == name
            || feature.strip_prefix("ax-libc/") == Some(name)
            || feature.strip_prefix("ax-std/") == Some(name)
    })
}

pub(super) fn c_define_name(feature: &str) -> String {
    feature.replace('-', "_").to_uppercase()
}

pub(super) fn map_c_app_features(
    case_features: &[String],
    base_features: &[String],
) -> anyhow::Result<Vec<String>> {
    const LIB_FEATURES: &[&str] = &[
        "fp-simd",
        "irq",
        "alloc",
        "multitask",
        "lockdep",
        "fs",
        "net",
        "fd",
        "pipe",
        "select",
        "poll",
        "epoll",
        "ext4fs",
        "fatfs",
    ];

    let mut features = BTreeSet::new();
    for feature in base_features {
        reject_removed_c_app_feature(feature)?;
        let normalized = feature
            .strip_prefix("ax-std/")
            .or_else(|| feature.strip_prefix("ax-libc/"))
            .unwrap_or(feature);
        if feature.starts_with("ax-hal/")
            || feature.starts_with("ax-driver/")
            || feature.starts_with("ax-runtime/")
        {
            features.insert(feature.clone());
            continue;
        }
        match normalized {
            "ax-std" | "ax-libc" => {}
            "plat-dyn" => bail!(
                "C app feature `plat-dyn` is no longer supported; dynamic platform selection is \
                 automatic"
            ),
            "smp" => {
                features.insert("smp".to_string());
            }
            feature if LIB_FEATURES.contains(&feature) => {
                features.insert(feature.to_string());
            }
            feature => {
                features.insert(feature.to_string());
            }
        }
    }
    for feature in case_features {
        if feature.starts_with(C_DEFINE_FEATURE_PREFIX) {
            continue;
        }
        reject_removed_c_app_feature(feature)?;
        let normalized = feature
            .strip_prefix("ax-std/")
            .or_else(|| feature.strip_prefix("ax-libc/"))
            .unwrap_or(feature);
        if feature.starts_with("ax-hal/")
            || feature.starts_with("ax-driver/")
            || feature.starts_with("ax-runtime/")
        {
            features.insert(feature.clone());
            continue;
        }
        if normalized == "plat-dyn" {
            bail!(
                "C app feature `plat-dyn` is no longer supported; dynamic platform selection is \
                 automatic"
            );
        }
        features.insert(normalized.to_string());
    }
    Ok(features.into_iter().collect())
}

fn reject_removed_c_app_feature(feature: &str) -> anyhow::Result<()> {
    if feature == concat!("ax-driver/", "plat", "-static") {
        bail!("C app feature `{feature}` is no longer supported; remove it from the configuration");
    }
    Ok(())
}
