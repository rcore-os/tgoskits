use std::collections::BTreeSet;

use anyhow::Context;
use cargo_metadata::Metadata;

use super::{
    AXSTD_STD_DEFAULT_FEATURE, AXSTD_STD_PACKAGE, DEFAULT_FEATURE,
    check::{ClippyCheck, ClippyCheckKind, ClippyDepsMode},
    env::{clippy_env, feature_clippy_env},
    selection::SelectedClippyPackage,
    targets::{docs_rs_targets, feature_supported_on_clippy_target},
};

pub(super) fn expand_clippy_checks(
    packages: &[SelectedClippyPackage],
    metadata: &Metadata,
) -> anyhow::Result<Vec<ClippyCheck>> {
    let mut checks = Vec::new();
    for selected in packages {
        let package = &selected.package;
        let mut features: BTreeSet<_> = package
            .features
            .keys()
            .filter(|feature| feature.as_str() != DEFAULT_FEATURE)
            .cloned()
            .collect();
        if package.name == AXSTD_STD_PACKAGE {
            features.insert(AXSTD_STD_DEFAULT_FEATURE.to_string());
        }
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
                deps_mode: selected.deps_mode.clone(),
                target: target.clone(),
                env: env.clone(),
            });

            if matches!(selected.deps_mode, ClippyDepsMode::WithDeps) {
                continue;
            }

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
                    deps_mode: selected.deps_mode.clone(),
                    target: target.clone(),
                    env: feature_env,
                });
            }
        }
    }

    Ok(checks)
}
