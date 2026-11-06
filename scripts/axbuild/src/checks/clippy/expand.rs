use std::collections::BTreeSet;

use anyhow::Context;
use cargo_metadata::{Metadata, Package};

use super::{
    AXSTD_STD_DEFAULT_FEATURE, AXSTD_STD_PACKAGE, DEFAULT_FEATURE,
    check::{ClippyCheck, ClippyCheckKind},
    configurations::package_clippy_configurations,
    env::{clippy_env, feature_clippy_env},
    targets::{docs_rs_targets, feature_requires_host_target, feature_supported_on_clippy_target},
};

pub(super) fn expand_clippy_checks(
    packages: &[Package],
    metadata: &Metadata,
) -> anyhow::Result<Vec<ClippyCheck>> {
    let mut checks = Vec::new();
    for package in packages {
        let mut features: BTreeSet<_> = package
            .features
            .keys()
            .filter(|feature| feature.as_str() != DEFAULT_FEATURE)
            .cloned()
            .collect();
        if package.name == AXSTD_STD_PACKAGE {
            features.insert(AXSTD_STD_DEFAULT_FEATURE.to_string());
        }
        let host_only_features = features
            .iter()
            .filter(|feature| feature_requires_host_target(package, feature))
            .cloned()
            .collect::<BTreeSet<_>>();
        features.retain(|feature| !host_only_features.contains(feature));
        let targets = docs_rs_targets(package);
        let target_iter = if targets.is_empty() {
            vec![None]
        } else {
            targets.into_iter().map(Some).collect()
        };
        let env = clippy_env(package);

        for target in target_iter {
            checks.push(ClippyCheck {
                package: package.name.to_string(),
                kind: ClippyCheckKind::Base,
                target: target.clone(),
                env: env.clone(),
            });

            for feature in &features {
                if !feature_supported_on_clippy_target(package, feature, target.as_deref()) {
                    continue;
                }
                let feature_env = feature_clippy_env(package, feature, env.clone(), metadata)
                    .with_context(|| {
                        format!(
                            "failed to prepare clippy env for `{}` feature `{feature}`",
                            package.name
                        )
                    })?;
                checks.push(ClippyCheck {
                    package: package.name.to_string(),
                    kind: ClippyCheckKind::Feature(feature.clone()),
                    target: target.clone(),
                    env: feature_env,
                });
            }
        }

        for feature in host_only_features {
            let feature_env = feature_clippy_env(package, &feature, env.clone(), metadata)
                .with_context(|| {
                    format!(
                        "failed to prepare clippy env for `{}` feature `{feature}`",
                        package.name
                    )
                })?;
            checks.push(ClippyCheck {
                package: package.name.to_string(),
                kind: ClippyCheckKind::Feature(feature),
                target: None,
                env: feature_env,
            });
        }
        for configuration in package_clippy_configurations(package)? {
            checks.push(ClippyCheck {
                package: package.name.to_string(),
                kind: ClippyCheckKind::Configuration {
                    name: configuration.name,
                    features: configuration.features,
                    rustflags: configuration.rustflags,
                },
                target: Some(configuration.target),
                env: configuration.env,
            });
        }
    }

    Ok(checks)
}
