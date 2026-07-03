use crate::clippy::{
    AXSTD_STD_CLIPPY_FEATURES, AXSTD_STD_CLIPPY_TARGET, AXSTD_STD_DEFAULT_FEATURE,
    AXSTD_STD_PACKAGE,
    check::{ClippyCheckKind, ClippyDepsMode},
    expand::expand_clippy_checks,
    selection::SelectedClippyPackage,
};

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
        !std_check.env.iter().any(|(key, _)| key == "AX_CONFIG_PATH"),
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
