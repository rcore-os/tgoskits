use super::*;

#[test]
fn rejects_legacy_and_removed_platform_features() {
    for feature in [
        "axstd",
        "axstd/net",
        "plat-dyn",
        "ax-std/plat-dyn",
        "stack-guard-page",
        "ax-std/stack-protector",
        "ax-runtime/stack-guard-page",
        "ax-libc/stack-guard-page",
        "axlibc/stack-protector",
        "starry-kernel/stack-protector",
    ] {
        let info = BuildInfo {
            features: vec![feature.to_string()],
            ..BuildInfo::default()
        };

        assert!(
            info.validate_features().is_err(),
            "{feature} must be rejected"
        );
    }
}

#[test]
fn removed_stack_hardening_features_explain_paging_migration() {
    let error = BuildInfo {
        features: vec!["ax-std/stack-protector".to_string()],
        ..BuildInfo::default()
    }
    .validate_features()
    .unwrap_err();

    assert!(error.to_string().contains("canonical `paging`"));
}

#[test]
fn stack_hardening_features_are_absent_from_public_package_graph() {
    let metadata = repo_metadata();
    let packages = [
        "ax-runtime",
        "ax-std",
        "ax-libc",
        "starry-kernel",
        "starryos",
        "axvisor",
    ];
    let removed = ["stack-guard-page", "stack-protector"];

    for package_name in packages {
        let package = metadata
            .packages
            .iter()
            .find(|package| package.name == package_name)
            .unwrap_or_else(|| panic!("workspace package {package_name} is missing"));
        for feature in &removed {
            assert!(
                !package.features.contains_key(*feature),
                "{package_name}/{feature}"
            );
        }
        for dependency in &package.dependencies {
            for feature in &removed {
                assert!(
                    !dependency
                        .features
                        .iter()
                        .any(|candidate| candidate == feature),
                    "{package_name} forwards removed {}/{}",
                    dependency.name,
                    feature
                );
            }
        }
    }
}
