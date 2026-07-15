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
    let mut envs = HashMap::new();
    pass_std_build_nested_features(
        &mut envs,
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
    assert!(envs.is_empty());
    assert!(!envs.values().any(|value| value.contains("arceos")));
    assert!(!info.features.contains(&"lockdep".to_string()));
}

#[test]
fn makefile_features_use_ax_std_dependency_for_std_build() {
    let mut info = BuildInfo {
        features: Vec::new(),
        ..BuildInfo::default()
    };

    apply_makefile_features(&mut info, "arceos-app", &[String::from("lockdep")]).unwrap();

    info.resolve_std_features();
    let mut envs = HashMap::new();
    pass_std_build_nested_features(
        &mut envs,
        &mut info.features,
        &[],
        &["lockdep".to_string(), "std-compat".to_string()],
    );

    assert_eq!(info.features, vec!["ax-std/lockdep".to_string()]);
    assert!(envs.is_empty());
}

#[test]
fn unknown_ax_hal_features_are_not_platforms() {
    let metadata = repo_metadata();

    for feature in ["ax-hal/not-a-platform", "ax-hal/qemu-board"] {
        assert_eq!(ax_hal_platform_feature_name(feature, Some(&metadata)), None);
    }
}
