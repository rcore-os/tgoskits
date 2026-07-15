use super::*;
use crate::build::info::StdFeaturePrefixFamily;

#[test]
fn rejects_packages_without_ax_std_dependency() {
    let workspace = temp_workspace("plain-app", "ax-api = \"0.1.0\"\n").unwrap();

    let metadata = metadata_for_manifest(&workspace.join("Cargo.toml"));
    let err = detect_std_feature_prefix_family("plain-app", &metadata).unwrap_err();

    assert!(err.to_string().contains("must directly depend on `ax-std`"));
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

    apply_makefile_features_with_prefix_family(
        &mut info,
        "arceos-app",
        &[String::from("lockdep")],
        Err(anyhow::anyhow!("std test packages do not depend on ax-std")),
    );

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

#[test]
fn default_platform_feature_uses_dynamic_platform() {
    let mut info = BuildInfo::default();

    info.resolve_features_with_prefix_family(
        "arceos-helloworld",
        "loongarch64-unknown-none-softfloat",
        Ok(StdFeaturePrefixFamily::AxStd),
        None,
    );

    assert!(!info.features.contains(&"ax-std/plat-dyn".to_string()));
}
