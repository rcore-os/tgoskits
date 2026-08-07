use super::*;

#[test]
fn rejects_legacy_and_removed_platform_features() {
    for feature in ["axstd", "axstd/net", "plat-dyn", "ax-std/plat-dyn"] {
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
fn std_build_maps_arceos_features_to_ax_std_dependency() {
    let mut info = BuildInfo {
        features: vec![
            "ax-std".to_string(),
            "lockdep".to_string(),
            "ax-std/smp".to_string(),
        ],
        ..BuildInfo::default()
    };

    info.resolve_std_features();
    pass_std_build_nested_features(
        &mut info.features,
        &[],
        &[
            "lockdep".to_string(),
            "smp".to_string(),
            "std-compat".to_string(),
        ],
    );

    assert_eq!(
        info.features,
        vec!["ax-std/lockdep".to_string(), "ax-std/smp".to_string()]
    );
    assert!(!info.features.contains(&"lockdep".to_string()));
}

#[test]
fn makefile_features_use_ax_std_dependency_for_std_build() {
    let mut info = BuildInfo {
        features: Vec::new(),
        ..BuildInfo::default()
    };

    apply_makefile_features(&mut info, &[String::from("lockdep")]).unwrap();

    info.resolve_std_features();
    pass_std_build_nested_features(
        &mut info.features,
        &[],
        &["lockdep".to_string(), "std-compat".to_string()],
    );

    assert_eq!(info.features, vec!["ax-std/lockdep".to_string()]);
}

#[test]
fn unknown_ax_hal_features_are_not_platforms() {
    let metadata = repo_metadata();

    for feature in ["ax-hal/not-a-platform", "ax-hal/qemu-board"] {
        assert_eq!(ax_hal_platform_feature_name(feature, Some(&metadata)), None);
    }
}

#[test]
fn axvm_os_implementation_dependencies_are_facaded_by_ax_std() {
    let metadata = repo_metadata();
    let axvm = workspace_package(&metadata, "axvm").unwrap();
    let forbidden = [
        "ax-hal",
        "ax-kernel-guard",
        "ax-kspin",
        "ax-lazyinit",
        "ax-percpu",
        "ax-sync",
        "spin",
    ];

    let direct_forbidden = axvm
        .dependencies
        .iter()
        .filter(|dependency| forbidden.contains(&dependency.name.as_str()))
        .map(|dependency| dependency.name.as_str())
        .collect::<Vec<_>>();

    assert!(
        direct_forbidden.is_empty(),
        "axvm must obtain OS implementation capabilities through ax-std: {direct_forbidden:?}"
    );
}
