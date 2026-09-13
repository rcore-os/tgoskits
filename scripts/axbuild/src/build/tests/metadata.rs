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

    assert!(info.features.contains(&"ax-std/lockdep".to_string()));
    assert!(info.features.contains(&"ax-std/smp".to_string()));
    assert!(!info.features.contains(&"lockdep".to_string()));
}
