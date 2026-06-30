use std::path::PathBuf;

use super::common::{expand, pkg_with_manifest_path, pkg_with_manifest_path_and_metadata};
use crate::clippy::{
    AX_CONFIG_PATH_ENV, AXSTD_STD_CLIPPY_FEATURES, AXSTD_STD_CLIPPY_TARGET,
    AXSTD_STD_DEFAULT_FEATURE, AXSTD_STD_PACKAGE,
    check::{ClippyCheckKind, ClippyDepsMode},
    env::feature_axconfig_overrides,
    expand::expand_clippy_checks,
    selection::SelectedClippyPackage,
};

#[test]
fn package_axconfig_is_passed_as_clippy_env() {
    let temp = tempfile::tempdir().unwrap();
    let package_dir = temp.path().join("alpha");
    std::fs::create_dir_all(&package_dir).unwrap();
    std::fs::write(package_dir.join("axconfig.toml"), "").unwrap();

    let package = pkg_with_manifest_path(
        "alpha",
        "alpha 0.1.0 (path+file:///tmp/alpha)",
        &[],
        None,
        package_dir.join("Cargo.toml"),
    );

    let checks = expand(&[package]);

    assert_eq!(
        checks[0].env,
        vec![(
            "AX_CONFIG_PATH".to_string(),
            package_dir.join("axconfig.toml").display().to_string(),
        )]
    );
}

#[test]
fn feature_axconfig_overrides_apply_only_to_that_feature_check() {
    let temp = tempfile::tempdir().unwrap();
    let package_dir = temp.path().join("alpha");
    std::fs::create_dir_all(&package_dir).unwrap();
    std::fs::write(package_dir.join("axconfig.toml"), "").unwrap();

    let package = pkg_with_manifest_path_and_metadata(
        "alpha",
        "alpha 0.1.0 (path+file:///tmp/alpha)",
        &[("cntv-timer", &[]), ("gic-v3", &[])],
        Some(&["aarch64-unknown-none"]),
        package_dir.join("Cargo.toml"),
        serde_json::json!({
            "axbuild": {
                "clippy-feature-axconfig-overrides": {
                    "cntv-timer": ["devices.timer-irq=27"]
                }
            }
        }),
    );

    let checks = expand(&[package]);

    assert_eq!(
        checks
            .iter()
            .map(|check| (check.label(), check.env.clone()))
            .collect::<Vec<_>>(),
        vec![
            (
                "alpha (base, target: aarch64-unknown-none-softfloat)".to_string(),
                vec![(
                    "AX_CONFIG_PATH".to_string(),
                    package_dir.join("axconfig.toml").display().to_string(),
                )],
            ),
            (
                "alpha (feature: cntv-timer, target: aarch64-unknown-none-softfloat)".to_string(),
                vec![(
                    "AX_CONFIG_PATH".to_string(),
                    "/tmp/ws/tmp/axbuild/axconfig/alpha/aarch64-unknown-none-softfloat/clippy/\
                     cntv-timer/.axconfig.toml"
                        .to_string(),
                )],
            ),
            (
                "alpha (feature: gic-v3, target: aarch64-unknown-none-softfloat)".to_string(),
                vec![(
                    "AX_CONFIG_PATH".to_string(),
                    package_dir.join("axconfig.toml").display().to_string(),
                )],
            ),
        ]
    );
}

#[test]
fn malformed_feature_axconfig_overrides_are_ignored() {
    let package = pkg_with_manifest_path_and_metadata(
        "alpha",
        "alpha 0.1.0 (path+file:///tmp/alpha)",
        &[("cntv-timer", &[])],
        Some(&["aarch64-unknown-none"]),
        PathBuf::from("/tmp/alpha/Cargo.toml"),
        serde_json::json!({
            "axbuild": {
                "clippy-feature-axconfig-overrides": {
                    "cntv-timer": "devices.timer-irq=27",
                    "gic-v3": [27, true]
                }
            }
        }),
    );

    assert!(feature_axconfig_overrides(&package).is_empty());
}

#[test]
fn axstd_default_config_is_passed_as_clippy_env() {
    let metadata = crate::build::workspace_metadata().unwrap();
    let package = metadata
        .packages
        .iter()
        .find(|package| package.name == AXSTD_STD_PACKAGE)
        .cloned()
        .expect("ax-std package should be in workspace metadata");

    let checks = expand_clippy_checks(
        &[SelectedClippyPackage {
            package,
            deps_mode: ClippyDepsMode::NoDeps,
        }],
        &metadata,
    )
    .unwrap();

    assert!(
        checks[0].env.is_empty(),
        "base ax-std clippy check should not use std-build env: {:?}",
        checks[0].env
    );

    let std_check = checks
        .iter()
        .find(|check| {
            matches!(
                &check.kind,
                ClippyCheckKind::Feature(feature) if feature == AXSTD_STD_DEFAULT_FEATURE
            )
        })
        .expect("ax-std default clippy check should exist");

    assert!(
        std_check
            .env
            .iter()
            .any(|(key, value)| { key == "AX_TARGET" && value == AXSTD_STD_CLIPPY_TARGET }),
        "expected AX_TARGET in {:?}",
        std_check.env
    );
    assert!(
        !std_check
            .env
            .iter()
            .any(|(key, _)| key == AX_CONFIG_PATH_ENV),
        "dynamic ax-std clippy check should not use static AX_CONFIG_PATH: {:?}",
        std_check.env
    );
    assert!(
        !std_check.env.iter().any(|(key, _)| key == "RUSTFLAGS"),
        "std ax-std clippy check should not inject custom RUSTFLAGS: {:?}",
        std_check.env
    );
    assert!(
        std_check
            .cargo_args()
            .windows(2)
            .any(|window| window == ["--features", AXSTD_STD_CLIPPY_FEATURES]),
        "expected expanded ax-std std features in {:?}",
        std_check.cargo_args()
    );
}
